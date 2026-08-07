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
