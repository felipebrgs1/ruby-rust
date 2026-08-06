//! Harness de integração (equivalente ao `test/lib` do repo ruby).
//!
//! Cada teste roda o binário `calisto` de verdade com um runtime dir isolado
//! (`CALISTO_RUNTIME_DIR`), então daemons de testes paralelos não colidem.
//! `run_opt` tem timeout embutido: se o calisto travar (ex.: daemon segurando
//! um pipe), o teste falha em vez de pendurar a suíte.

use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_calisto");
static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Runtime dir único por chamada (socket/pid do daemon isolados).
pub fn runtime_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("calisto-test-{}-{n}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("test/fixtures")
        .join(name)
}

pub fn calisto(dir: &Path) -> Command {
    let mut c = Command::new(BIN);
    c.env("CALISTO_RUNTIME_DIR", dir);
    c
}

pub struct RunOpts<'a> {
    pub args: &'a [&'a str],
    pub env: &'a [(&'a str, &'a str)],
    pub stdin: Option<&'a [u8]>,
    pub cwd: Option<&'a Path>,
    pub timeout: u64,
}

pub fn run(dir: &Path, args: &[&str]) -> Output {
    run_opt(
        dir,
        RunOpts { args, env: &[], stdin: None, cwd: None, timeout: 30 },
    )
}

pub fn run_opt(dir: &Path, opts: RunOpts) -> Output {
    let mut cmd = calisto(dir);
    for (k, v) in opts.env {
        cmd.env(k, v);
    }
    if let Some(c) = opts.cwd {
        cmd.current_dir(c);
    }
    cmd.args(opts.args);
    cmd.stdin(if opts.stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn calisto");
    if let Some(data) = opts.stdin {
        child.stdin.as_mut().unwrap().write_all(data).unwrap();
    }
    drop(child.stdin.take()); // fecha o pipe -> filho vê EOF

    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut v = Vec::new();
        stdout.read_to_end(&mut v).unwrap();
        v
    });
    let err = std::thread::spawn(move || {
        let mut v = Vec::new();
        stderr.read_to_end(&mut v).unwrap();
        v
    });

    let deadline = Instant::now() + Duration::from_secs(opts.timeout);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "calisto {:?} excedeu {}s (daemon segurando pipe?)",
                opts.args, opts.timeout
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Output { status, stdout: out.join().unwrap(), stderr: err.join().unwrap() }
}

/// Spawn sem esperar: usado por testes que precisam interagir com o processo
/// enquanto o script roda (ler pid do filho, mandar sinais, matar o cliente).
pub fn spawn_stdout(dir: &Path, args: &[&str]) -> (Child, BufReader<std::process::ChildStdout>) {
    let mut child = calisto(dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn calisto");
    let stdout = BufReader::new(child.stdout.take().unwrap());
    (child, stdout)
}

pub fn stop(dir: &Path) -> Output {
    run(dir, &["stop"])
}
