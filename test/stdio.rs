//! Semântica de execução: stdio, argv, env, cwd, exit codes, backtraces.
//! Cobre a paridade cold (interprete direto) x warm (daemon + fork).

mod common;

use common::*;

fn hi(dir: &std::path::Path) -> String {
    String::from_utf8_lossy(
        &run(dir, &["run", "--preload", "0", fixture("hello.rb").to_str().unwrap()]).stdout,
    )
    .into_owned()
}

#[test]
fn hello_runs_and_exits_zero() {
    let dir = runtime_dir("hello");
    let out = run(&dir, &["run", "--preload", "0", fixture("hello.rb").to_str().unwrap()]);
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("hello from calisto"));
    assert!(s.contains("3.4.10"));
    assert!(s.contains("args: []"));
}

#[test]
fn script_args_are_passed() {
    let dir = runtime_dir("args");
    let out = run(
        &dir,
        &["run", "--preload", "0", fixture("args.rb").to_str().unwrap(), "--foo", "bar"],
    );
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "--foo|bar");
}

#[test]
fn exit_code_propagates() {
    let dir = runtime_dir("exit5");
    let out = run(&dir, &["run", "--preload", "0", fixture("exit5.rb").to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(5));
    assert!(String::from_utf8_lossy(&out.stdout).contains("exiting with 5"));
}

#[test]
fn cold_and_warm_agree() {
    let dir = runtime_dir("parity");
    let cold = run(&dir, &["run", "--cold", fixture("hello.rb").to_str().unwrap()]);
    let warm = run(&dir, &["run", "--preload", "0", fixture("hello.rb").to_str().unwrap()]);
    assert!(cold.status.success() && warm.status.success());
    assert_eq!(cold.stdout, warm.stdout, "cold e warm devem produzir a mesma saida");

    let cold5 = run(&dir, &["run", "--cold", fixture("exit5.rb").to_str().unwrap()]);
    let warm5 = run(&dir, &["run", "--preload", "0", fixture("exit5.rb").to_str().unwrap()]);
    assert_eq!(cold5.status.code(), Some(5));
    assert_eq!(warm5.status.code(), Some(5));
}

#[test]
fn exception_prints_backtrace_without_daemon_frames() {
    let dir = runtime_dir("raise");
    let out = run(&dir, &["run", "--preload", "0", fixture("raise.rb").to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("exploded"));
    assert!(err.contains("raise.rb"), "backtrace deve apontar para o script: {err}");
    assert!(!err.contains("daemon.rb"), "frames internos do daemon devem ser cortados: {err}");
}

#[test]
fn stdin_is_forwarded_to_child() {
    let dir = runtime_dir("stdin");
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "--preload", "0", fixture("stdin.rb").to_str().unwrap()],
            env: &[],
            stdin: Some(b"ola-pipe\n"),
            cwd: None,
            timeout: 30,
        },
    );
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "stdout: got=ola-pipe");
    assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "stderr: got=ola-pipe");
}

#[test]
fn client_env_is_forwarded() {
    let dir = runtime_dir("env");
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "--preload", "0", fixture("env.rb").to_str().unwrap()],
            env: &[("CALISTO_TEST_VAR", "xyz")],
            stdin: None,
            cwd: None,
            timeout: 30,
        },
    );
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "xyz");
}

#[test]
fn client_cwd_is_forwarded() {
    let dir = runtime_dir("cwd");
    let sub = dir.join("subdir");
    std::fs::create_dir_all(&sub).unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "--preload", "0", fixture("cwd.rb").to_str().unwrap()],
            env: &[],
            stdin: None,
            cwd: Some(&sub),
            timeout: 30,
        },
    );
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), sub.to_str().unwrap());
}

#[test]
fn require_relative_works() {
    let dir = runtime_dir("reqrel");
    let out = run(&dir, &["run", "--preload", "0", fixture("reqrel.rb").to_str().unwrap()]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "helper=ok");
}

#[test]
fn data_section_is_available() {
    let dir = runtime_dir("data");
    let out = run(&dir, &["run", "--preload", "0", fixture("data.rb").to_str().unwrap()]);
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "dados de teste");
}
