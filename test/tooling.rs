//! Fase J: init / upgrade / completions — o ciclo de vida do calisto (o
//! `bun init`/`bun upgrade` do Ruby).
//!
//! Invariantes cobertos:
//! - `calisto init` gera calisto.toml + hello.rb + .gitignore; o app roda com
//!   `calisto run` BARE (convencao npm/bun: `[scripts] start`) e com
//!   `calisto run hello.rb` (arquivo existente sempre vence); --cold concorda
//! - init nunca sobrescreve arquivo existente sem --force; nome que e arquivo
//!   e flag desconhecida sao erros claros
//! - `calisto upgrade` roda o script de build (CALISTO_BUILD_SCRIPT nos
//!   testes; caminho real e junto ao vendor), passando RUBY_VERSION para a
//!   versao pedida, e propaga o exit code do script; sem versao = pin default
//! - upgrade de versao sem sha conhecido falha ANTES de spawnar o script;
//!   script ausente e erro claro; mais de um argumento e erro de uso
//! - completions bash/zsh imprimem scripts instalaveis; shell desconhecido ou
//!   ausente e erro de uso

mod common;

use common::*;
use std::path::{Path, PathBuf};
use std::process::Output;

fn run_at(dir: &Path, cwd: &Path, args: &[&str]) -> Output {
    run_opt(
        dir,
        RunOpts { args, env: &[], stdin: None, cwd: Some(cwd), timeout: 30 },
    )
}

// --- init --------------------------------------------------------------------

#[test]
fn init_scaffolds_app_that_runs() {
    let dir = runtime_dir("init-app");
    // cwd explicito: `calisto init` escreve no cwd (nunca herdar o do teste)
    let out = run_at(&dir, &dir, &["init", "meu-app"]);
    assert!(
        out.status.success(),
        "init falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let app = dir.join("meu-app");
    assert!(app.join("calisto.toml").is_file());
    assert!(app.join("hello.rb").is_file());
    assert!(app.join(".gitignore").is_file());
    let toml = std::fs::read_to_string(app.join("calisto.toml")).unwrap();
    assert!(toml.contains("[scripts]") && toml.contains("start"), "{toml}");
    let hello = String::from_utf8_lossy(&out.stdout);
    assert!(hello.contains("Initialized calisto app in ./meu-app"), "{hello}");

    // bare `calisto run` roda o `start` do calisto.toml (o hello.rb)
    let out = run_at(&dir, &app, &["run"]);
    assert!(
        out.status.success(),
        "run bare falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello from calisto!");

    // arquivo existente sempre vence sobre o script de mesmo nome
    let out = run_at(&dir, &app, &["run", "hello.rb"]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello from calisto!");

    // paridade cold/warm (regra de ouro do run)
    let out = run_at(&dir, &app, &["run", "--cold"]);
    assert!(
        out.status.success(),
        "run --cold falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Hello from calisto!");
}

#[test]
fn init_refuses_to_clobber_without_force() {
    let dir = runtime_dir("init-clobber");
    let app = dir.join("meu-app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("calisto.toml"), "# app real\n").unwrap();
    let out = run_at(&dir, &dir, &["init", "meu-app"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ja existe"), "erro deveria citar o arquivo: {err}");
    // --force sobrescreve
    let out = run_at(&dir, &dir, &["init", "--force", "meu-app"]);
    assert!(
        out.status.success(),
        "init --force falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn init_in_current_dir_and_bad_args() {
    let dir = runtime_dir("init-cwd");
    let app = dir.join("app");
    std::fs::create_dir_all(&app).unwrap();
    let out = run_at(&dir, &app, &["init"]);
    assert!(
        out.status.success(),
        "init no cwd falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(app.join("calisto.toml").is_file());

    // nome que e arquivo -> erro claro
    std::fs::write(app.join("bloqueio"), "x").unwrap();
    let out = run_at(&dir, &app, &["init", "bloqueio"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("arquivo"));

    // flag desconhecida -> erro
    let out = run_at(&dir, &app, &["init", "--bogus"]);
    assert_eq!(out.status.code(), Some(1));
}

// --- upgrade -----------------------------------------------------------------

/// Script de build fake para testes: registra RUBY_VERSION (ou "default"
/// quando ausente) no FAKE_BUILD_LOG e sai com FAKE_BUILD_RC (default 0).
fn fake_build_script(dir: &Path) -> PathBuf {
    let script = dir.join("fake-build.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nprintf '%s\\n' \"${RUBY_VERSION:-default}\" >> \"${FAKE_BUILD_LOG}\"\nexit \"${FAKE_BUILD_RC:-0}\"\n",
    )
    .unwrap();
    script
}

#[test]
fn upgrade_runs_build_script_with_version_env() {
    let dir = runtime_dir("upgrade-env");
    let script = fake_build_script(&dir);
    let log = dir.join("build.log");
    let env: &[(&str, &str)] = &[
        ("CALISTO_BUILD_SCRIPT", script.to_str().unwrap()),
        ("FAKE_BUILD_LOG", log.to_str().unwrap()),
    ];

    // sem versao: rebuild do pin (RUBY_VERSION nao definido -> "default")
    let out = run_opt(
        &dir,
        RunOpts { args: &["upgrade"], env, stdin: None, cwd: None, timeout: 30 },
    );
    assert!(
        out.status.success(),
        "upgrade falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // versao especifica: RUBY_VERSION vai pro script
    let out = run_opt(
        &dir,
        RunOpts { args: &["upgrade", "3.4.4"], env, stdin: None, cwd: None, timeout: 30 },
    );
    assert!(out.status.success());
    let log_content = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = log_content.lines().collect();
    assert_eq!(lines, vec!["default", "3.4.4"]);

    // idempotente: rodar de novo nao e erro
    let out = run_opt(
        &dir,
        RunOpts { args: &["upgrade"], env, stdin: None, cwd: None, timeout: 30 },
    );
    assert!(out.status.success());
}

#[test]
fn upgrade_propagates_failure_and_validates_version_fast() {
    let dir = runtime_dir("upgrade-fail");
    let script = fake_build_script(&dir);
    let log = dir.join("build.log");
    let fail_env: &[(&str, &str)] = &[
        ("CALISTO_BUILD_SCRIPT", script.to_str().unwrap()),
        ("FAKE_BUILD_LOG", log.to_str().unwrap()),
        ("FAKE_BUILD_RC", "7"),
    ];
    // exit code do script propaga
    let out = run_opt(
        &dir,
        RunOpts { args: &["upgrade"], env: fail_env, stdin: None, cwd: None, timeout: 30 },
    );
    assert_eq!(out.status.code(), Some(7));

    // versao sem sha conhecido: erro claro ANTES de spawnar o script
    let env: &[(&str, &str)] = &[
        ("CALISTO_BUILD_SCRIPT", script.to_str().unwrap()),
        ("FAKE_BUILD_LOG", log.to_str().unwrap()),
    ];
    let out = run_opt(
        &dir,
        RunOpts { args: &["upgrade", "9.9.9"], env, stdin: None, cwd: None, timeout: 30 },
    );
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("sha256"), "erro deveria citar o sha: {err}");
    let log_content = std::fs::read_to_string(&log).unwrap();
    assert!(!log_content.contains("9.9.9"), "script nao deveria rodar");

    // script ausente: erro claro
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["upgrade"],
            env: &[("CALISTO_BUILD_SCRIPT", "/nao/existe/build.sh")],
            stdin: None,
            cwd: None,
            timeout: 30,
        },
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("nao encontrado"));

    // mais de um argumento: erro de uso
    let out = run_opt(
        &dir,
        RunOpts { args: &["upgrade", "3.4.4", "3.4.10"], env, stdin: None, cwd: None, timeout: 30 },
    );
    assert_eq!(out.status.code(), Some(1));
}

// --- Fase Q: distribuicao ----------------------------------------------------

/// "Ruby" fake: script executavel que imprime a versao falsa — prova que o
/// calisto resolveu o ruby do CALISTO_HOME (e nao o vendor do checkout).
fn fake_ruby(dir: &Path, version: &str) {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("vendor").join(format!("ruby-{version}/bin"));
    std::fs::create_dir_all(&bin).unwrap();
    let rb = bin.join("ruby");
    std::fs::write(&rb, "#!/bin/sh\necho fake ruby 9.9.9\n").unwrap();
    let mut perm = std::fs::metadata(&rb).unwrap().permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(&rb, perm).unwrap();
}

#[test]
fn calisto_home_selects_vendor() {
    let dir = runtime_dir("q3-home");
    fake_ruby(&dir, "3.4.10");
    let env: &[(&str, &str)] = &[("CALISTO_HOME", dir.to_str().unwrap())];
    // o exe do teste subiria ate o vendor REAL do checkout — o CALISTO_HOME
    // precisa vencer: o --version mostra o ruby fake
    let out = run_opt(
        &dir,
        RunOpts { args: &["--version"], env, stdin: None, cwd: None, timeout: 30 },
    );
    assert!(
        out.status.success(),
        "--version falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("calisto 0.1.0"), "{stdout}");
    assert!(
        stdout.contains("fake ruby 9.9.9"),
        "CALISTO_HOME deveria vencer o walk-up do checkout: {stdout}"
    );

    // CALISTO_HOME sem vendor: o ruby do pin nao existe -> erro com o build
    let empty = runtime_dir("q3-empty");
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "-e", "puts 1"],
            env: &[("CALISTO_HOME", empty.to_str().unwrap())],
            stdin: None,
            cwd: None,
            timeout: 30,
        },
    );
    assert_eq!(out.status.code(), Some(1));
}

/// Monta um release fake num dir: tarball do ruby <v> (layout ruby-<v>/ na
/// raiz, como o release.sh) + .sha256 no formato do sha256sum -c.
fn fake_release(serve: &Path, version: &str, corrupt: bool) {
    let name = format!("calisto-ruby-{version}-linux-x86_64.tar.gz");
    let staging = serve.join("staging");
    std::fs::create_dir_all(&staging.join(format!("ruby-{version}/bin"))).unwrap();
    std::fs::write(
        staging.join(format!("ruby-{version}/bin/ruby")),
        "#!/bin/sh\necho fake ruby\n",
    )
    .unwrap();
    let st = std::process::Command::new("tar")
        .arg("czf")
        .arg(serve.join(&name))
        .arg("-C")
        .arg(&staging)
        .arg(format!("ruby-{version}"))
        .status()
        .unwrap();
    assert!(st.success());
    let hash = if corrupt {
        "0".repeat(64)
    } else {
        let out = std::process::Command::new("sha256sum")
            .arg(serve.join(&name))
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().split_whitespace().next().unwrap().to_string()
    };
    std::fs::write(serve.join(format!("{name}.sha256")), format!("{hash}  {name}\n")).unwrap();
    std::fs::remove_dir_all(&staging).unwrap();
}

#[test]
fn upgrade_downloads_prebuilt_without_build_script() {
    let dir = runtime_dir("q2-download");
    let serve = runtime_dir("q2-serve");
    fake_release(&serve, "3.4.4", false);
    let env: &[(&str, &str)] = &[
        // instalacao portatil: CALISTO_HOME sem scripts/ (sem build possivel)
        ("CALISTO_HOME", dir.to_str().unwrap()),
        ("CALISTO_UPGRADE_URL", &format!("file://{}", serve.display())),
    ];
    let out = run_opt(
        &dir,
        RunOpts { args: &["upgrade", "3.4.4"], env, stdin: None, cwd: None, timeout: 60 },
    );
    assert!(
        out.status.success(),
        "upgrade falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let installed = dir.join("vendor/ruby-3.4.4/bin/ruby");
    assert!(installed.is_file(), "ruby baixado deveria estar em {}", installed.display());
    assert!(String::from_utf8_lossy(&out.stderr).contains("instalado em"));
}

#[test]
fn upgrade_download_rejects_bad_sha256() {
    let dir = runtime_dir("q2-badsha");
    let serve = runtime_dir("q2-serve-bad");
    fake_release(&serve, "3.4.4", true);
    let env: &[(&str, &str)] = &[
        ("CALISTO_HOME", dir.to_str().unwrap()),
        ("CALISTO_UPGRADE_URL", &format!("file://{}", serve.display())),
    ];
    let out = run_opt(
        &dir,
        RunOpts { args: &["upgrade", "3.4.4"], env, stdin: None, cwd: None, timeout: 60 },
    );
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("sha256"), "erro deveria citar o sha: {err}");
    assert!(
        !dir.join("vendor/ruby-3.4.4").exists(),
        "sha invalido nao pode extrair"
    );
}

#[test]
fn upgrade_source_flag_forces_build() {
    let dir = runtime_dir("q2-source");
    let script = fake_build_script(&dir);
    let log = dir.join("build.log");
    let env: &[(&str, &str)] = &[
        ("CALISTO_BUILD_SCRIPT", script.to_str().unwrap()),
        ("FAKE_BUILD_LOG", log.to_str().unwrap()),
    ];
    let out = run_opt(
        &dir,
        RunOpts { args: &["upgrade", "--source"], env, stdin: None, cwd: None, timeout: 30 },
    );
    assert!(
        out.status.success(),
        "--source com script presente deveria buildar: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // sem script + --source: erro claro (nao tenta download)
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["upgrade", "--source"],
            env: &[("CALISTO_HOME", dir.to_str().unwrap())],
            stdin: None,
            cwd: None,
            timeout: 30,
        },
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--source"));
}

// --- completions -------------------------------------------------------------

#[test]
fn completions_print_installable_scripts() {
    let dir = runtime_dir("completions");
    let out = run(&dir, &["completions", "bash"]);
    assert!(out.status.success());
    let bash = String::from_utf8_lossy(&out.stdout);
    assert!(bash.contains("complete -F _calisto calisto"), "{bash}");
    for cmd in ["run", "test", "task", "serve", "exec", "repl", "build", "init", "upgrade", "completions", "status"] {
        assert!(bash.contains(cmd), "bash completion sem '{cmd}': {bash}");
    }
    let out = run(&dir, &["completions", "zsh"]);
    assert!(out.status.success());
    let zsh = String::from_utf8_lossy(&out.stdout);
    assert!(zsh.contains("#compdef calisto") && zsh.contains("compdef _calisto calisto"), "{zsh}");

    // sem shell ou shell desconhecido: erro de uso
    let out = run(&dir, &["completions"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("bash|zsh"));
    let out = run(&dir, &["completions", "fish"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("desconhecido"));
}
