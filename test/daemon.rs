//! Daemon: lifecycle, reuso de socket, recovery de socket morto, sinais,
//! orfãos, concorrência e a não-retenção de pipes.

mod common;

use common::*;
use std::io::{BufRead, Write};
use std::path::Path;
use std::time::{Duration, Instant};

fn sleep_pid(dir: &Path) -> (std::process::Child, u32) {
    let (mut child, mut out) = spawn_stdout(
        dir,
        &["run", "--preload", "0", fixture("sleep.rb").to_str().unwrap()],
    );
    let mut line = String::new();
    out.read_line(&mut line).expect("filho deveria imprimir o pid");
    let pid: u32 = line
        .trim()
        .strip_prefix("pid=")
        .expect("linha do pid")
        .parse()
        .expect("pid numerico");
    (child, pid)
}

#[test]
fn daemon_reuses_same_socket() {
    let dir = runtime_dir("reuse");
    let hello = fixture("hello.rb");
    run(&dir, &["run", "--preload", "0", hello.to_str().unwrap()]);
    let pid1 = std::fs::read_to_string(dir.join("calisto.pid")).unwrap();
    run(&dir, &["run", "--preload", "0", hello.to_str().unwrap()]);
    let pid2 = std::fs::read_to_string(dir.join("calisto.pid")).unwrap();
    assert_eq!(pid1, pid2, "segundo run deve reusar o daemon existente");
    stop(&dir);
}

#[test]
fn stale_socket_is_recovered() {
    let dir = runtime_dir("stale");
    // socket morto (sem listener atras dele)
    let _ = std::os::unix::net::UnixListener::bind(dir.join("calisto.sock"));
    let out = run(&dir, &["run", "--preload", "0", fixture("hello.rb").to_str().unwrap()]);
    assert!(out.status.success(), "daemon deve limpar socket morto e servir");
    let status = run(&dir, &["status"]);
    assert!(String::from_utf8_lossy(&status.stdout).contains("running"));
    stop(&dir);
}

#[test]
fn killing_client_kills_child_no_orphan() {
    let dir = runtime_dir("orphan");
    let (mut child, pid) = sleep_pid(&dir);
    // SIGINT no cliente (como Ctrl-C)
    let st = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(st.success());
    let status = child.wait().unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(2), "cliente morre com SIGINT");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if !Path::new(&format!("/proc/{pid}")).exists() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "filho {pid} virou orfao apos matar o cliente"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    stop(&dir);
}

#[test]
fn child_killed_by_signal_reports_128_plus_sig() {
    // SIGKILL é incapturável: o waitpid vê termsig=9 -> 128+9=137.
    // (SIGTERM o Ruby converte em SignalException -> uncaught -> exit 1,
    //  idem `ruby script.rb`.)
    let dir = runtime_dir("sigkill");
    let (mut child, pid) = sleep_pid(&dir);
    let st = std::process::Command::new("kill").args(["-KILL", &pid.to_string()]).status().unwrap();
    assert!(st.success());
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(137), "SIGKILL no filho -> 128+9");
    stop(&dir);
}

#[test]
fn concurrent_runs_serialize_through_daemon() {
    let dir = runtime_dir("conc");
    let hello = fixture("hello.rb").to_str().unwrap().to_string();
    let d1 = dir.clone();
    let d2 = dir.clone();
    let h1 = hello.clone();
    let h2 = hello.clone();
    let t1 = std::thread::spawn(move || run(&d1, &["run", "--preload", "0", &h1]));
    let t2 = std::thread::spawn(move || run(&d2, &["run", "--preload", "0", &h2]));
    let (o1, o2) = (t1.join().unwrap(), t2.join().unwrap());
    assert!(o1.status.success() && o2.status.success());
    assert!(o1.stdout == o2.stdout);
    stop(&dir);
}

#[test]
fn long_running_child_does_not_block_new_runs() {
    // Fase E: accept loop multi-conexao. Um child de longa duracao (server,
    // sidekiq, suíte lenta) nao pode bloquear novos RUNs — o daemon antigo
    // ficava preso no wait_for e o 2o run pendurava (timeout do run_opt).
    let dir = runtime_dir("multiconn");
    let (mut sleeper, pid) = sleep_pid(&dir);

    // o RUN concorrente completa mesmo com o sleeper rodando
    let out = run(&dir, &["run", "--preload", "0", fixture("hello.rb").to_str().unwrap()]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // o sleeper continua vivo (o RUN paralelo nao o matou)
    assert!(Path::new(&format!("/proc/{pid}")).exists(), "sleeper nao deveria morrer");

    // matar o cliente do sleeper derruba o child (sem orfao) e o daemon
    // continua servindo
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let out = run(&dir, &["run", "--preload", "0", fixture("hello.rb").to_str().unwrap()]);
    assert!(out.status.success(), "daemon deveria continuar servindo");
    stop(&dir);
}

#[test]
fn pipeline_does_not_hang() {
    // O daemon desanexa o proprio stdio; se um pipe ficasse retido, o
    // run_opt com timeout de 10s falharia em vez de pendurar a suite.
    let dir = runtime_dir("pipe");
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "--preload", "0", fixture("raise.rb").to_str().unwrap()],
            env: &[],
            stdin: None,
            cwd: None,
            timeout: 10,
        },
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("exploded"));
    stop(&dir);
}

/// Fase L: o daemon roda EMBUTIDO (VM in-process via dlopen da libruby) —
/// o processo do daemon e o proprio binario calisto, nao um ruby externo.
#[test]
fn daemon_runs_embedded_in_process() {
    let dir = runtime_dir("embed");
    let out = run(&dir, &["run", "-e", "puts RUBY_VERSION"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "3.4.10");

    // o daemon vivo e o binario calisto (pidfile -> /proc/<pid>/exe)
    let pid = std::fs::read_to_string(dir.join("calisto.pid")).expect("pidfile");
    let pid = pid.trim().parse::<u32>().expect("pid numerico");
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).expect("exe do daemon");
    let bin = std::fs::canonicalize(BIN).expect("binario calisto");
    assert_eq!(exe, bin, "daemon deve ser o proprio calisto (embutido)");
    stop(&dir);
}

/// Fase L: CALISTO_NO_EMBED=1 forca o modo legado (spawn do ruby externo) —
/// fallback para rubies sem libruby.so (ex.: builds pre --enable-shared).
#[test]
fn daemon_legacy_fallback_with_no_embed() {
    let dir = runtime_dir("embedlegacy");
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "-e", "puts RUBY_VERSION"],
            env: &[("CALISTO_NO_EMBED", "1")],
            stdin: None,
            cwd: None,
            timeout: 30,
        },
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "3.4.10");

    let pid = std::fs::read_to_string(dir.join("calisto.pid")).expect("pidfile");
    let pid = pid.trim().parse::<u32>().expect("pid numerico");
    let exe = std::fs::read_link(format!("/proc/{pid}/exe")).expect("exe do daemon");
    let exe = exe.to_string_lossy().into_owned();
    assert!(exe.contains("/bin/ruby"), "daemon legado deve ser o ruby externo: {exe}");
    stop(&dir);
}
