//! calisto — doctor
//!
//! calisto status/stop/doctor (smaps_rollup, probe).
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — commands/doctor (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::fs;
use std::process::{Command};
use std::time::{Duration};
use crate::appconfig::*;
use crate::protocol::*;
use crate::runtime::*;







pub fn cmd_status() -> i32 {
    let dir = current_runtime_dir();
    match daemon_connect_at(&dir) {
        Some(mut s) => {
            let ok = send_cmd(&mut s, "PING", &[]).is_ok()
                && read_line(&mut s).map(|l| l == "OK").unwrap_or(false);
            if ok {
                let pid = fs::read_to_string(dir.join("calisto.pid")).unwrap_or_default();
                println!("daemon: running (pid {})", pid.trim());
            } else {
                println!("daemon: socket present but unresponsive (stale)");
            }
        }
        None => {
            println!("daemon: not running");
        }
    }
    // o daemon de teste da app (RAILS_ENV=test) tambem e reportado
    if let Ok(Some(app)) = load_app_config() {
        if let Ok(ruby) = resolve_ruby() {
            let tdir = app_test_runtime_dir(&app, &ruby);
            match daemon_connect_at(&tdir) {
            Some(mut s) => {
                let ok = send_cmd(&mut s, "PING", &[]).is_ok()
                    && read_line(&mut s).map(|l| l == "OK").unwrap_or(false);
                if ok {
                    let pid = fs::read_to_string(tdir.join("calisto.pid")).unwrap_or_default();
                    println!("test daemon: running (pid {})", pid.trim());
                } else {
                    println!("test daemon: socket present but unresponsive (stale)");
                }
            }
            None => println!("test daemon: not running"),
        }
        }
    }
    0
}



pub fn cmd_stop() -> i32 {
    let dir = current_runtime_dir();
    let had = daemon_connect_at(&dir).is_some();
    stop_daemon_at(&dir);
    if let Ok(Some(app)) = load_app_config() {
        if let Ok(ruby) = resolve_ruby() {
            stop_daemon_at(&app_test_runtime_dir(&app, &ruby)); // test daemon tambem
        }
    }
    println!("daemon: {}", if had { "stopped" } else { "not running" });
    0
}



/// Memoria do processo (Fase M.2): smaps_rollup do /proc (uma linha por
/// categoria, barato — nao soma o smaps inteiro). Separar Shared_Clean de
/// Private_Dirty e o que prova o CoW: paginas compartilhadas (herdadas do
/// fork) vs o custo real de cada child.
pub struct MemStats {
    rss_kb: u64,
    pss_kb: u64,
    shared_clean_kb: u64,
    private_dirty_kb: u64,
}



pub fn read_smaps_rollup(pid: i32) -> Option<MemStats> {
    let content = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).ok()?;
    let mut m = MemStats { rss_kb: 0, pss_kb: 0, shared_clean_kb: 0, private_dirty_kb: 0 };
    for line in content.lines() {
        let mut it = line.split_whitespace();
        let (Some(k), Some(v)) = (it.next(), it.next()) else { continue };
        let Ok(kb) = v.parse::<u64>() else { continue };
        match k.trim_end_matches(':') {
            "Rss" => m.rss_kb = kb,
            "Pss" => m.pss_kb = kb,
            "Shared_Clean" => m.shared_clean_kb = kb,
            "Private_Dirty" => m.private_dirty_kb = kb,
            _ => {}
        }
    }
    (m.rss_kb > 0).then_some(m)
}



pub fn fmt_mib(kb: u64) -> String {
    format!("{:.1} MiB", kb as f64 / 1024.0)
}



pub fn print_mem_line(label: &str, m: &MemStats) {
    println!(
        "  {label}: RSS {} | Pss {} | Shared_Clean {} | Private_Dirty {}",
        fmt_mib(m.rss_kb),
        fmt_mib(m.pss_kb),
        fmt_mib(m.shared_clean_kb),
        fmt_mib(m.private_dirty_kb)
    );
}



/// Codigo do child de probe: escreve o pid, suja paginas de verdade
/// (alocacao + GC espalham as escritas pelo heap — e isso que o CoW mede),
/// sinaliza `DONE` e dorme enquanto o doctor le o smaps_rollup (o child so
/// existe vivo durante o RUN).
pub const PROBE_CODE: &str = r#"
File.write(ENV.fetch("CALISTO_PROBE_PID"), Process.pid.to_s)
a = 300_000.times.map { Object.new }
GC.start
a = nil
File.write(ENV.fetch("CALISTO_PROBE_DONE"), "1")
sleep 3
"#;



pub fn report_daemon_memory() {
    let dir = current_runtime_dir();
    let Some(pid) = fs::read_to_string(dir.join("calisto.pid"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
    else {
        return;
    };
    if let Some(m) = read_smaps_rollup(pid) {
        print_mem_line("daemon memory", &m);
    }
    let Some(mut s) = daemon_connect_at(&dir) else { return };
    // child de probe via EVAL no daemon quente (thread: o STATUS so chega no
    // fim do sleep — a leitura acontece no meio, via pid/done files)
    let probe_pid = std::env::temp_dir().join(format!(
        "calisto-probe-{}-{}.pid",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let probe_done = probe_pid.with_extension("done");
    let (pid_path, done_path) = (probe_pid.display().to_string(), probe_done.display().to_string());
    let handle = std::thread::spawn(move || {
        let cwd = env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default();
        let extra = [
            ("CALISTO_PROBE_PID", pid_path.as_str()),
            ("CALISTO_PROBE_DONE", done_path.as_str()),
        ];
        let _ = eval_request_full(&mut s, &cwd, &extra, PROBE_CODE, &[], &crate::commands::run::RunFlags::default());
    });
    // pid aparece em ~10ms; DONE depois do trabalho do probe (alocacao + GC)
    let mut child: Option<i32> = None;
    for _ in 0..200 {
        if probe_done.is_file() {
            child = fs::read_to_string(&probe_pid).ok().and_then(|p| p.trim().parse().ok());
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Some(child_pid) = child {
        if let Some(m) = read_smaps_rollup(child_pid) {
            print_mem_line("probe child memory", &m);
        }
    }
    let _ = handle.join();
    let _ = fs::remove_file(&probe_pid);
    let _ = fs::remove_file(&probe_done);
}



pub fn cmd_doctor() -> i32 {
    let Some(ruby) = ruby_or_err() else {
        return 1;
    };
    println!("calisto doctor");
    println!("  pinned ruby: {}", ruby.display());
    let _ = Command::new(&ruby).arg("-v").status();
    match load_app_config().ok().flatten().and_then(|a| a.preload) {
        Some(p) => println!("  app preload: {}", p.display()),
        None => println!(
            "  preload: {}",
            env::var("CALISTO_PRELOAD").unwrap_or_else(|_| DEFAULT_PRELOAD.to_string())
        ),
    }
    cmd_status();
    // Fase M.2: memoria do daemon + child de probe (CoW) — so quando o daemon
    // esta vivo (doctor nao spawna daemon: diagnostico, nao boot)
    if daemon_connect_at(&current_runtime_dir()).is_some() {
        report_daemon_memory();
    }
    0
}
