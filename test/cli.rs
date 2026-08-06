//! CLI: help, comandos, caminhos de erro, ciclo do daemon.

mod common;

use common::*;

#[test]
fn help_prints_usage() {
    let dir = runtime_dir("help");
    let out = run(&dir, &["help"]);
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("calisto"));
    assert!(s.contains("USAGE"));
    assert!(s.contains("run"));
}

#[test]
fn unknown_command_fails() {
    let dir = runtime_dir("unknown");
    let out = run(&dir, &["frobnicate"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown command"));
}

#[test]
fn run_without_script_fails() {
    let dir = runtime_dir("noscript");
    let out = run(&dir, &["run"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("run needs a script"));
}

#[test]
fn missing_script_fails() {
    let dir = runtime_dir("nofile");
    let out = run(&dir, &["run", "nao-existe.rb"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot open"));
}

#[test]
fn doctor_reports_pinned_ruby() {
    let dir = runtime_dir("doctor");
    let out = run(&dir, &["doctor"]);
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("pinned ruby"));
    assert!(s.contains("3.4"), "doctor deveria reportar o ruby pinado 3.4.x: {s}");
}

#[test]
fn status_and_stop_lifecycle() {
    let dir = runtime_dir("lifecycle");
    let before = run(&dir, &["status"]);
    assert!(String::from_utf8_lossy(&before.stdout).contains("not running"));

    run(&dir, &["run", "--preload", "0", fixture("hello.rb").to_str().unwrap()]);
    let during = run(&dir, &["status"]);
    assert!(String::from_utf8_lossy(&during.stdout).contains("running"));

    run(&dir, &["stop"]);
    let after = run(&dir, &["status"]);
    assert!(String::from_utf8_lossy(&after.stdout).contains("not running"));
    assert!(!dir.join("calisto.sock").exists(), "socket deveria ser removido no stop");
}
