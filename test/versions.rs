//! Fase I: multi-versões de ruby — seleção por `.ruby-version`/Gemfile,
//! daemons isolados por versão e erro claro quando a versão não está
//! instalada (hoje: pin único 3.4.10 em vendor/current).
//!
//! Invariantes cobertos:
//! - `.ruby-version` (prefixo `ruby-` tolerado) seleciona vendor/ruby-<v> e
//!   cold/warm concordam na versão
//! - diretiva `ruby "x.y.z"` do Gemfile seleciona (sem .ruby-version)
//! - versão não instalada -> erro claro nomeando a versão e o build
//! - daemons genéricos isolados por versão (sockets distintos; stop de um não
//!   derruba o outro)
//! - daemon da app (preload) por versão: hash do socket inclui a versão —
//!   o mesmo app em versões diferentes paga o boot UMA vez cada
//!
//! Os testes de 3.4.4 são gated em `vendor/ruby-3.4.4` (RUBY_VERSION=3.4.4
//! scripts/build-ruby.sh), como os goldens de bundle.

mod common;

use common::*;
use std::path::Path;
use std::time::Instant;

fn have_ruby(version: &str) -> bool {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("vendor/ruby-{version}/bin/ruby"))
        .is_file()
}

fn run_in(dir: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    run_opt(
        dir,
        RunOpts {
            args,
            env: &[],
            stdin: None,
            cwd: Some(cwd),
            timeout: 60,
        },
    )
}

#[test]
fn dot_ruby_version_selects_and_cold_agrees() {
    if !have_ruby("3.4.4") {
        eprintln!("SKIP dot_ruby_version_selects: rode RUBY_VERSION=3.4.4 scripts/build-ruby.sh");
        return;
    }
    let dir = runtime_dir("rvsel");
    let project = dir.join("rvproj");
    std::fs::create_dir_all(&project).unwrap();
    // prefixo `ruby-` tolerado (rbenv-style)
    std::fs::write(project.join(".ruby-version"), "ruby-3.4.4\n").unwrap();

    let warm = run_in(&dir, &project, &["run", "-e", "puts RUBY_VERSION"]);
    assert!(warm.status.success(), "{}", String::from_utf8_lossy(&warm.stderr));
    assert_eq!(String::from_utf8_lossy(&warm.stdout).trim(), "3.4.4");

    let cold = run_in(&dir, &project, &["run", "--cold", "-e", "puts RUBY_VERSION"]);
    assert!(cold.status.success(), "{}", String::from_utf8_lossy(&cold.stderr));
    assert_eq!(cold.stdout, warm.stdout, "cold/warm devem concordar na versao");
    let _ = run_in(&dir, &project, &["stop"]);
}

#[test]
fn gemfile_ruby_directive_selects() {
    if !have_ruby("3.4.4") {
        eprintln!("SKIP gemfile_ruby_directive_selects: rode RUBY_VERSION=3.4.4 scripts/build-ruby.sh");
        return;
    }
    // Sem .ruby-version, a diretiva `ruby "3.4.4"` do Gemfile seleciona.
    // Lock vazio hermetico (sem bundle install, sem rede — bundler/setup com
    // bundle vazio e no-op).
    let dir = runtime_dir("rvgemfile");
    let project = dir.join("rvgem");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("Gemfile"),
        "source \"https://rubygems.org\"\nruby \"3.4.4\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("Gemfile.lock"),
        "GEM\n  remote: https://rubygems.org/\n  specs:\n\nPLATFORMS\n  ruby\n\nDEPENDENCIES\n\nBUNDLED WITH\n   2.6.7\n",
    )
    .unwrap();

    let out = run_in(&dir, &project, &["run", "-e", "puts RUBY_VERSION"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "3.4.4");
    let _ = run_in(&dir, &project, &["stop"]);
}

#[test]
fn missing_version_fails_clearly() {
    // .ruby-version apontando uma versao nao instalada -> ERRO claro (exit
    // != 0) nomeando a versao e o comando de build — substitui o warning da
    // Fase 1-2.
    let dir = runtime_dir("rvmissing");
    let project = dir.join("rvbad");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(".ruby-version"), "9.9.9\n").unwrap();

    let out = run_in(&dir, &project, &["run", "--cold", "-e", "puts 1"]);
    assert_ne!(out.status.code(), Some(0), "versao ausente deve abortar");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("9.9.9"), "deve nomear a versao: {err}");
    assert!(err.contains("build-ruby.sh"), "deve apontar o build: {err}");
    assert!(err.contains(".ruby-version"), "deve apontar a origem: {err}");
}

#[test]
fn daemons_isolated_per_version() {
    if !have_ruby("3.4.4") {
        eprintln!("SKIP daemons_isolated_per_version: rode RUBY_VERSION=3.4.4 scripts/build-ruby.sh");
        return;
    }
    // Mesmo runtime dir: o projeto 3.4.4 e o default (sem pedido) rodam
    // daemons SEPARADOS — sockets distintos; stop de um nao derruba o outro.
    let dir = runtime_dir("rviso");
    let vproj = dir.join("v344");
    std::fs::create_dir_all(&vproj).unwrap();
    std::fs::write(vproj.join(".ruby-version"), "3.4.4\n").unwrap();
    let default = dir.join("default");
    std::fs::create_dir_all(&default).unwrap();

    let out = run_in(&dir, &vproj, &["run", "-e", "puts RUBY_VERSION"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "3.4.4");
    let out = run_in(&dir, &default, &["run", "-e", "puts RUBY_VERSION"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "3.4.10");

    assert!(dir.join("ruby-3.4.4/calisto.sock").exists(), "daemon 3.4.4 dedicado");
    assert!(dir.join("calisto.sock").exists(), "daemon default dedicado");

    // stop do projeto 3.4.4 derruba SO o daemon 3.4.4
    let out = run_in(&dir, &vproj, &["stop"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!dir.join("ruby-3.4.4/calisto.sock").exists());
    let out = run_in(&dir, &default, &["status"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("running"),
        "default deve continuar vivo: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = run_in(&dir, &default, &["stop"]);
}

#[test]
fn app_daemon_separated_per_version() {
    if !have_ruby("3.4.4") {
        eprintln!("SKIP app_daemon_separated_per_version: rode RUBY_VERSION=3.4.4 scripts/build-ruby.sh");
        return;
    }
    // Daemon da app (preload) por versao: o mesmo app com .ruby-version 3.4.4
    // e sem ele ganha daemons SEPARADOS (o hash do socket inclui a versao),
    // cada um pagando o boot UMA vez (contador).
    let dir = runtime_dir("rvapp");
    let project = dir.join("verapp");
    std::fs::create_dir_all(project.join("config")).unwrap();
    std::fs::write(
        project.join("calisto.toml"),
        "[run]\npreload = \"config/environment.rb\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join("config/environment.rb"),
        "# frozen_string_literal: true\ncount_file = ENV.fetch(\"BOOT_COUNT_FILE\", File.expand_path(\"boot_count\", __dir__))\nn = File.exist?(count_file) ? File.read(count_file).to_i + 1 : 1\nFile.write(count_file, n.to_s)\nsleep 1\n",
    )
    .unwrap();
    let bc344 = dir.join("bc344");
    let bcdef = dir.join("bcdef");

    // versao 3.4.4: boot UMA vez, 2o eval e fork rapido
    std::fs::write(project.join(".ruby-version"), "3.4.4\n").unwrap();
    let first = run_opt(
        &dir,
        RunOpts {
            args: &["run", "-e", "puts RUBY_VERSION"],
            env: &[("BOOT_COUNT_FILE", bc344.to_str().unwrap())],
            stdin: None,
            cwd: Some(&project),
            timeout: 60,
        },
    );
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert_eq!(String::from_utf8_lossy(&first.stdout).trim(), "3.4.4");
    assert_eq!(std::fs::read_to_string(&bc344).unwrap().trim(), "1", "1o paga o boot");
    let t0 = Instant::now();
    let second = run_opt(
        &dir,
        RunOpts {
            args: &["run", "-e", "puts RUBY_VERSION"],
            env: &[("BOOT_COUNT_FILE", bc344.to_str().unwrap())],
            stdin: None,
            cwd: Some(&project),
            timeout: 60,
        },
    );
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    assert!(t0.elapsed().as_millis() < 500, "2o eval deve ser fork rapido");
    assert_eq!(std::fs::read_to_string(&bc344).unwrap().trim(), "1", "nao re-boota");

    // default (sem .ruby-version): daemon SEPARADO, boot pago de novo
    std::fs::remove_file(project.join(".ruby-version")).unwrap();
    let first = run_opt(
        &dir,
        RunOpts {
            args: &["run", "-e", "puts RUBY_VERSION"],
            env: &[("BOOT_COUNT_FILE", bcdef.to_str().unwrap())],
            stdin: None,
            cwd: Some(&project),
            timeout: 60,
        },
    );
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert_eq!(String::from_utf8_lossy(&first.stdout).trim(), "3.4.10");
    assert_eq!(std::fs::read_to_string(&bcdef).unwrap().trim(), "1", "daemon separado re-boota");

    // dois daemons da app distintos em apps/
    let n = std::fs::read_dir(dir.join("apps")).unwrap().count();
    assert_eq!(n, 2, "apps/ deve ter um daemon por versao");
    let _ = run_in(&dir, &project, &["stop"]);
}

/// Fase S: o 3.4.4 (rebuildado com --enable-shared) roda o daemon EMBUTIDO —
/// o processo do daemon e o proprio binario calisto. Antes do rebuild, o
/// 3.4.4 era o unico caso do server.rb legado (build pre-shared); a Fase S
/// fechou esse gap: pidfile -> /proc/<pid>/exe == calisto em TODAS as
/// versoes.
#[test]
fn ruby344_daemon_runs_embedded() {
    if !have_ruby("3.4.4") {
        eprintln!("SKIP ruby344_daemon_runs_embedded: rode RUBY_VERSION=3.4.4 scripts/build-ruby.sh");
        return;
    }
    let dir = runtime_dir("rvembed344");
    let project = dir.join("proj344");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(".ruby-version"), "3.4.4\n").unwrap();

    let out = run_in(&dir, &project, &["run", "-e", "puts RUBY_VERSION"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "3.4.4");

    // daemon 3.4.4 vivo e o binario calisto (daemon generico por versao:
    // runtime_dir/ruby-3.4.4/calisto.pid)
    let pid = std::fs::read_to_string(dir.join("ruby-3.4.4/calisto.pid")).expect("pidfile do daemon 3.4.4");
    let pid = pid.trim().parse::<u32>().expect("pid numerico");
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).expect("exe do daemon");
    let bin = std::fs::canonicalize(BIN).expect("binario calisto");
    assert_eq!(exe, bin, "daemon 3.4.4 deve ser o proprio calisto (embutido)");
    let _ = run_in(&dir, &project, &["stop"]);
}
