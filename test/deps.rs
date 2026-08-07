//! Fase K: deps — `calisto add/remove/lock`, o wrapper fino do bundle
//! (decisao da Fase A: nada de instalador proprio).
//!
//! Invariantes cobertos:
//! - sem Gemfile (walk-up) -> erro claro sugerindo `bundle init`
//! - o wrapper roda `bundle <sub>` com cwd na raiz do projeto (dir do
//!   Gemfile) e BUNDLE_GEMFILE setado; args passam direto
//! - o bundle e o do ruby da versao certa (Fase I): `.ruby-version` ->
//!   vendor/ruby-<v>/bin/ruby (CALISTO_BUNDLE_RUBY exportado p/ observar;
//!   gated em vendor/ruby-3.4.4, como versions.rs)
//! - PATH prefixado com o bin dir do ruby (trap do restart do bundler: lock
//!   que pina outro bundler re-executa via shebang e precisa de ruby no PATH)
//! - exit code do bundle propaga (CALISTO_BUNDLE fake p/ testes hermeticos)

mod common;

use common::*;
use std::path::{Path, PathBuf};
use std::process::Output;

/// Fake do `bundle` para testes: registra argv, ruby resolvido, Gemfile, cwd
/// e 1o entry do PATH num log; exit code configurável via FAKE_BUNDLE_RC.
fn fake_bundle(dir: &Path) -> PathBuf {
    let script = dir.join("fake-bundle.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         printf '%s\\n' \"$*\" >> \"${FAKE_BUNDLE_LOG}\"\n\
         printf 'ruby=%s\\n' \"$CALISTO_BUNDLE_RUBY\" >> \"${FAKE_BUNDLE_LOG}\"\n\
         printf 'gemfile=%s\\n' \"$BUNDLE_GEMFILE\" >> \"${FAKE_BUNDLE_LOG}\"\n\
         printf 'pwd=%s\\n' \"$(pwd)\" >> \"${FAKE_BUNDLE_LOG}\"\n\
         printf 'path0=%s\\n' \"$(printf '%s' \"$PATH\" | cut -d: -f1)\" >> \"${FAKE_BUNDLE_LOG}\"\n\
         exit \"${FAKE_BUNDLE_RC:-0}\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

fn run_with_bundle(
    dir: &Path,
    cwd: &Path,
    args: &[&str],
    log: &Path,
    rc: Option<&str>,
) -> Output {
    let fake = fake_bundle(dir);
    let fake_s = fake.to_str().unwrap();
    let log_s = log.to_str().unwrap();
    let mut env: Vec<(&str, &str)> = vec![("CALISTO_BUNDLE", fake_s), ("FAKE_BUNDLE_LOG", log_s)];
    if let Some(rc) = rc {
        env.push(("FAKE_BUNDLE_RC", rc));
    }
    run_opt(
        dir,
        RunOpts { args, env: &env, stdin: None, cwd: Some(cwd), timeout: 30 },
    )
}

#[test]
fn deps_without_gemfile_fails_clearly() {
    let dir = runtime_dir("deps-nogemfile");
    let sub = dir.join("sem-projeto");
    std::fs::create_dir_all(&sub).unwrap();
    let out = run_at_dir(&dir, &sub, &["add", "sinatra"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("Gemfile"), "erro deveria citar Gemfile: {err}");
    assert!(err.contains("bundle init"), "erro deveria sugerir bundle init: {err}");
}

#[test]
fn add_passes_args_and_runs_at_app_root() {
    let dir = runtime_dir("deps-add");
    let app = fixture("gemapp");
    let log = dir.join("add.log");
    let out = run_with_bundle(
        &dir,
        &app,
        &["add", "sinatra", "--group", "web"],
        &log,
        None,
    );
    assert!(
        out.status.success(),
        "calisto add falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = s.lines().collect();
    assert_eq!(lines[0], "add sinatra --group web", "{s}");
    assert!(lines[1].contains("/vendor/") && lines[1].ends_with("bin/ruby"), "ruby resolvido: {s}");
    assert!(lines[2].ends_with("/gemapp/Gemfile"), "BUNDLE_GEMFILE: {s}");
    assert!(lines[3].ends_with("/gemapp"), "cwd deveria ser a raiz do projeto: {s}");
    assert!(lines[4].contains("/vendor/") && lines[4].ends_with("bin"), "PATH prefixado: {s}");
}

#[test]
fn remove_and_lock_pass_through() {
    let dir = runtime_dir("deps-rmlock");
    let app = fixture("gemapp");
    let log = dir.join("rm.log");
    let out = run_with_bundle(&dir, &app, &["remove", "rack"], &log, None);
    assert!(out.status.success());
    let s = std::fs::read_to_string(&log).unwrap();
    assert!(s.lines().next().unwrap() == "remove rack", "{s}");

    let log2 = dir.join("lock.log");
    let out = run_with_bundle(&dir, &app, &["lock"], &log2, None);
    assert!(out.status.success());
    let s = std::fs::read_to_string(&log2).unwrap();
    assert!(s.lines().next().unwrap() == "lock", "{s}");
}

#[test]
fn bundle_exit_code_propagates() {
    let dir = runtime_dir("deps-rc");
    let app = fixture("gemapp");
    let log = dir.join("rc.log");
    let out = run_with_bundle(&dir, &app, &["add", "rack"], &log, Some("9"));
    assert_eq!(out.status.code(), Some(9));
}

#[test]
fn deps_use_version_ruby_from_ruby_version() {
    // gated em vendor/ruby-3.4.4 (mesmo gate do versions.rs)
    let ruby_344 = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/ruby-3.4.4/bin/ruby");
    if !ruby_344.is_file() {
        eprintln!("SKIP deps_use_version_ruby_from_ruby_version: RUBY_VERSION=3.4.4 scripts/build-ruby.sh");
        return;
    }
    let dir = runtime_dir("deps-ver");
    let app = dir.join("app344");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join(".ruby-version"), "3.4.4\n").unwrap();
    std::fs::write(app.join("Gemfile"), "source \"https://rubygems.org\"\n").unwrap();
    let log = dir.join("ver.log");
    let out = run_with_bundle(&dir, &app, &["add", "json"], &log, None);
    assert!(
        out.status.success(),
        "calisto add falhou: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = std::fs::read_to_string(&log).unwrap();
    let ruby = s.lines().nth(1).unwrap();
    assert!(
        ruby.contains("ruby-3.4.4/bin/ruby"),
        "bundle deveria ser o do ruby 3.4.4: {s}"
    );
}

fn run_at_dir(dir: &Path, cwd: &Path, args: &[&str]) -> Output {
    run_opt(
        dir,
        RunOpts { args, env: &[], stdin: None, cwd: Some(cwd), timeout: 30 },
    )
}
