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

/// Fase M.2: com o daemon vivo, `doctor` reporta a memoria do daemon e de um
/// child de probe (smaps_rollup), separando Shared_Clean de Private_Dirty —
/// os numeros que provam o CoW dos forks. Sem daemon, nao reporta (doctor
/// nao spawna daemon: diagnostico, nao boot).
#[test]
fn doctor_reports_daemon_and_probe_child_memory() {
    let dir = runtime_dir("doctormem");
    // sem daemon: sem linhas de memoria, exit 0
    let out = run(&dir, &["doctor"]);
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(!s.contains("daemon memory"), "sem daemon nao deve reportar memoria: {s}");

    // com daemon: RSS/Pss/Shared_Clean/Private_Dirty do daemon e do child
    run(&dir, &["run", "--preload", "0", fixture("hello.rb").to_str().unwrap()]);
    let out = run(&dir, &["doctor"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let s = String::from_utf8_lossy(&out.stdout);
    for line in ["daemon memory:", "probe child memory:"] {
        let Some(l) = s.lines().find(|l| l.contains(line)) else {
            panic!("doctor deveria reportar '{line}': {s}");
        };
        for key in ["RSS", "Pss", "Shared_Clean", "Private_Dirty"] {
            let toks: Vec<&str> = l.split_whitespace().collect();
            let Some(i) = toks.iter().position(|t| *t == key) else {
                panic!("'{line}' sem {key}: {l}");
            };
            let v = toks[i + 1];
            assert!(
                v.parse::<f64>().unwrap_or(-1.0) >= 0.0,
                "{key} deveria ser um numero: {v}"
            );
            assert_eq!(toks[i + 2], "MiB");
        }
    }
    // CoW: o child compartilha paginas limpas com o daemon (Shared_Clean > 0)
    // e sujou paginas proprias ao trabalhar (RSS > RSS do daemon — o probe
    // alocou 300k objetos; sem CoW o RSS do child explodiria)
    let daemon = s.lines().find(|l| l.contains("daemon memory:")).unwrap();
    let child = s.lines().find(|l| l.contains("probe child memory:")).unwrap();
    let tok = |l: &str, k: &str| -> f64 {
        let toks: Vec<&str> = l.split_whitespace().collect();
        let i = toks.iter().position(|t| *t == k).unwrap();
        toks[i + 1].parse().unwrap()
    };
    assert!(tok(child, "Shared_Clean") > 0.0, "child deveria compartilhar paginas: {child}");
    assert!(
        tok(child, "RSS") > tok(daemon, "RSS"),
        "probe child deveria ter alocado alem do daemon: {child} vs {daemon}"
    );

    run(&dir, &["stop"]);
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
