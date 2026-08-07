//! Fase G: `calisto run -e` (paridade com ruby -e, <50ms quente), `calisto
//! exec <bin>` (resolucao estilo bundle exec no daemon quente, sem binstub) e
//! `calisto repl` (IRB no contexto da app pre-carregada).
//!
//! Invariantes cobertos:
//! - warm e --cold concordam no `-e` (paridade e regra de ouro do run)
//! - `-e` tem semantica de `ruby -e`: $0 = "-e", ARGV = args, backtrace
//!   "-e:1", multiplos -e concatenados com newline, sem DATA
//! - `-e` no daemon da app roda no boot congelado (nao re-boota)
//! - `exec` resolve via spec do bundle (hermetico: rake no gemapp), via PATH
//!   (fora de bundle) e via caminho de arquivo com shebang ruby (load
//!   in-process); comando inexistente -> 127
//! - `repl` roda IRB como child do fork da app (boot UMA vez)

mod common;

use common::*;
use std::path::Path;
use std::time::Instant;

fn app_run(dir: &Path, app: &str, args: &[&str]) -> std::process::Output {
    run_opt(
        dir,
        RunOpts {
            args,
            env: &[],
            stdin: None,
            cwd: Some(&fixture(app)),
            timeout: 60,
        },
    )
}

fn stop_app(dir: &Path, app: &str) {
    let _ = run_opt(
        dir,
        RunOpts {
            args: &["stop"],
            env: &[],
            stdin: None,
            cwd: Some(&fixture(app)),
            timeout: 10,
        },
    );
}

#[test]
fn run_e_matches_cold_and_ruby_semantics() {
    // $0/ARGV/__LINE__ idem `ruby -e`, e warm == --cold (paridade e
    // invariante do run).
    let dir = runtime_dir("evalpar");
    let args = ["run", "-e", "p $0; p ARGV; p __LINE__", "a1", "a2"];
    let warm = run(&dir, &args);
    assert!(warm.status.success(), "{}", String::from_utf8_lossy(&warm.stderr));
    let warm_out = String::from_utf8_lossy(&warm.stdout);
    assert!(warm_out.contains("\"-e\""), "$0 deve ser \"-e\": {warm_out}");
    assert!(warm_out.contains("[\"a1\", \"a2\"]"), "ARGV deve ser os args: {warm_out}");
    assert!(warm_out.contains("1"), "__LINE__ deve comecar em 1: {warm_out}");

    let cold = run(&dir, &["run", "--cold", "-e", "p $0; p ARGV; p __LINE__", "a1", "a2"]);
    assert!(cold.status.success(), "{}", String::from_utf8_lossy(&cold.stderr));
    assert_eq!(
        warm_out,
        String::from_utf8_lossy(&cold.stdout),
        "warm e --cold devem concordar no -e"
    );
}

#[test]
fn run_e_backtrace_is_dash_e() {
    // backtrace com nome de arquivo "-e" e linha 1, como `ruby -e`
    let dir = runtime_dir("evalbt");
    let warm = run(&dir, &["run", "-e", "raise 'boom'"]);
    assert_eq!(warm.status.code(), Some(1), "raise deve sair 1");
    let err = String::from_utf8_lossy(&warm.stderr);
    assert!(err.contains("-e:1:in"), "backtrace deve apontar -e:1: {err}");

    let cold = run(&dir, &["run", "--cold", "-e", "raise 'boom'"]);
    assert_eq!(cold.status.code(), Some(1));
    assert_eq!(err, String::from_utf8_lossy(&cold.stderr), "backtrace cold == warm");
}

#[test]
fn run_e_multiple_flags_concatenate_like_ruby() {
    // ruby junta multiplos -e com "\n": __LINE__ segue a concatenacao (1 e 2)
    let dir = runtime_dir("evalmulti");
    let args = ["run", "-e", "puts __LINE__", "-e", "puts __LINE__"];
    let warm = run(&dir, &args);
    assert!(warm.status.success(), "{}", String::from_utf8_lossy(&warm.stderr));
    let warm_out = String::from_utf8_lossy(&warm.stdout);
    let lines: Vec<&str> = warm_out.lines().collect();
    assert_eq!(lines, ["1", "2"], "multiplos -e devem concatenar com newline: {warm_out}");

    let cold = run(&dir, &["run", "--cold", "-e", "puts __LINE__", "-e", "puts __LINE__"]);
    assert!(cold.status.success(), "{}", String::from_utf8_lossy(&cold.stderr));
    assert_eq!(warm_out, String::from_utf8_lossy(&cold.stdout), "paridade cold/warm");
}

#[test]
fn run_e_warm_under_50ms() {
    // marco da Fase G: `calisto run -e 'puts 1+1'` quente <50ms (o 1o run
    // paga o spawn do daemon; o 2o e fork + eval)
    let dir = runtime_dir("evalfast");
    let first = run(&dir, &["run", "-e", "puts 1+1"]);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));

    let t0 = Instant::now();
    let second = run(&dir, &["run", "-e", "puts 1+1"]);
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    assert_eq!(String::from_utf8_lossy(&second.stdout).trim(), "2");
    let ms = t0.elapsed().as_millis();
    assert!(ms < 50, "eval quente deve ser <50ms: {ms}ms");
}

#[test]
fn run_e_in_app_context_boot_once() {
    // `-e` no daemon da app (preloadapp, boot simulado 2s): o 1o paga o boot,
    // o 2o e fork; o boot NAO re-roda (contador == 1) — eval roda no contexto
    // do boot congelado, como o run normal.
    let dir = runtime_dir("evalapp");
    let app = fixture("preloadapp");
    let bc = dir.join("boot_count");
    let env = [("BOOT_COUNT_FILE", bc.to_str().unwrap())];

    let first = run_opt(
        &dir,
        RunOpts {
            args: &["run", "-e", "puts :booted"],
            env: &env,
            stdin: None,
            cwd: Some(&app),
            timeout: 60,
        },
    );
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert_eq!(
        std::fs::read_to_string(&bc).unwrap().trim(),
        "1",
        "1o run paga o boot no daemon"
    );

    let second = run_opt(
        &dir,
        RunOpts {
            args: &["run", "-e", "puts :booted2"],
            env: &env,
            stdin: None,
            cwd: Some(&app),
            timeout: 60,
        },
    );
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    assert_eq!(
        std::fs::read_to_string(&bc).unwrap().trim(),
        "1",
        "eval nao pode re-bootar a app"
    );
    stop_app(&dir, "preloadapp");
}

#[test]
fn exec_runs_gem_bin_from_bundle() {
    // Hermetico (gemapp: 5 default gems no lock, sem bundle install): rake e
    // resolvido via specs ativadas do Gemfile e roda no daemon quente.
    let dir = runtime_dir("execrake");
    let app = fixture("gemapp");
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["exec", "rake", "--version"],
            env: &[],
            stdin: None,
            cwd: Some(&app),
            timeout: 30,
        },
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("rake") && out.contains("13.2.1"), "{out}");
}

#[test]
fn exec_falls_back_to_path() {
    // Sem Gemfile: resolucao cai no PATH (como `bundle exec` fora de bundle);
    // sh e binario nativo (exec direto, nao load).
    let dir = runtime_dir("execpath");
    let out = run(&dir, &["exec", "sh", "-c", "echo path-ok"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("path-ok"));
}

#[test]
fn exec_loads_ruby_script_in_process() {
    // Caminho de arquivo com shebang ruby -> load in-process (kernel_load do
    // bundler): $0 vira o caminho e ARGV os args.
    let dir = runtime_dir("execfile");
    let project = dir.join("execproj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("tool.rb"),
        "#!/usr/bin/env ruby\n# frozen_string_literal: true\nputs \"tool: #{$0} #{ARGV.join(',')}\"\n",
    )
    .unwrap();
    std::fs::set_permissions(
        project.join("tool.rb"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["exec", "./tool.rb", "x", "y"],
            env: &[],
            stdin: None,
            cwd: Some(&project),
            timeout: 30,
        },
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("tool:") && out.contains("x,y"), "{out}");
}

#[test]
fn exec_not_found_exits_127() {
    let dir = runtime_dir("execmissing");
    let out = run(&dir, &["exec", "calisto-definitely-not-a-real-bin-xyz"]);
    assert_eq!(out.status.code(), Some(127), "bin inexistente deve sair 127");
    assert!(String::from_utf8_lossy(&out.stderr).contains("nao encontrado"));
}

#[test]
fn repl_runs_irb_in_app_context() {
    // IRB como child do fork do daemon da app (preloadapp): o boot roda UMA
    // vez (contador == 1) e o REPL enxerga o contexto do boot.
    let dir = runtime_dir("replapp");
    let app = fixture("preloadapp");
    let bc = dir.join("boot_count");

    use std::io::{Read, Write};
    let mut child = common::calisto(&dir)
        .args(["repl"])
        .current_dir(&app)
        .env("BOOT_COUNT_FILE", bc.to_str().unwrap())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn calisto repl");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"puts 1+1\nexit\n")
        .unwrap();
    drop(child.stdin.take());

    // le o stdout com timeout (repl que nao sai = fail, nao pendura)
    let mut stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        stdout.read_to_string(&mut out).unwrap();
        tx.send(out).unwrap();
    });
    let out = match rx.recv_timeout(std::time::Duration::from_secs(20)) {
        Ok(o) => o,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("repl nao terminou em 20s");
        }
    };
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
    assert!(
        out.contains("\n2\n") || out.contains("=> 2"),
        "IRB deve avaliar puts 1+1: {out}"
    );
    assert_eq!(
        std::fs::read_to_string(&bc).unwrap().trim(),
        "1",
        "repl nao pode re-bootar a app"
    );
    stop_app(&dir, "preloadapp");
}

#[test]
fn exec_runs_in_app_context_fast() {
    // `exec` no daemon da app: 2o run <500ms (fork do boot congelado) e o
    // boot nao re-roda. usa preloadapp (hermetico) com rake... preloadapp nao
    // tem Gemfile; exec cai no PATH (sh) — o que prova o daemon da app no
    // caminho, com boot pago uma vez.
    let dir = runtime_dir("execapp");
    let app = fixture("preloadapp");
    let bc = dir.join("boot_count");
    let env = [("BOOT_COUNT_FILE", bc.to_str().unwrap())];

    let first = run_opt(
        &dir,
        RunOpts {
            args: &["exec", "sh", "-c", "echo via-app-daemon"],
            env: &env,
            stdin: None,
            cwd: Some(&app),
            timeout: 60,
        },
    );
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert_eq!(std::fs::read_to_string(&bc).unwrap().trim(), "1", "boot roda no daemon");

    let t0 = Instant::now();
    let second = run_opt(
        &dir,
        RunOpts {
            args: &["exec", "sh", "-c", "echo warm"],
            env: &env,
            stdin: None,
            cwd: Some(&app),
            timeout: 60,
        },
    );
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    let ms = t0.elapsed().as_millis();
    assert!(ms < 500, "exec warm deve ser <500ms: {ms}ms");
    assert_eq!(std::fs::read_to_string(&bc).unwrap().trim(), "1", "exec nao re-boota");
    stop_app(&dir, "preloadapp");
}
