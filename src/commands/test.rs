//! calisto — test
//!
//! calisto test (minitest/rspec no daemon, --watch).
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — commands/test (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::os::unix::net::{UnixStream};

use std::time::{Duration, Instant};
use crate::appconfig::*;
use crate::protocol::*;
use crate::runtime::*;






#[derive(Clone, Copy, PartialEq)]
pub enum TestFramework {
    Minitest,
    Rspec,
}

impl TestFramework {
    fn dir(self) -> &'static str {
        match self {
            TestFramework::Minitest => "test",
            TestFramework::Rspec => "spec",
        }
    }
    fn suffix(self) -> &'static str {
        match self {
            TestFramework::Minitest => "_test.rb",
            TestFramework::Rspec => "_spec.rb",
        }
    }
    fn name(self) -> &'static str {
        match self {
            TestFramework::Minitest => "minitest",
            TestFramework::Rspec => "rspec",
        }
    }
}



/// Coleta recursiva de arquivos terminados em `suffix` (sem deps: read_dir).
pub fn walk_files(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk_files(&p, suffix, out);
        } else if p
            .file_name()
            .map(|n| n.to_string_lossy().ends_with(suffix))
            .unwrap_or(false)
        {
            out.push(p);
        }
    }
}



/// rspec quando ha `.rspec` ou `spec/*_spec.rb`; senao minitest (`test/`).
pub fn detect_framework(root: &Path) -> TestFramework {
    if root.join(".rspec").is_file() {
        return TestFramework::Rspec;
    }
    let mut specs = Vec::new();
    walk_files(&root.join("spec"), "_spec.rb", &mut specs);
    if !specs.is_empty() {
        return TestFramework::Rspec;
    }
    TestFramework::Minitest
}



pub fn test_file_snapshot(files: &[PathBuf]) -> Vec<(u64, u64)> {
    files
        .iter()
        .map(|f| {
            fs::metadata(f)
                .map(|m| {
                    let mtime = m
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    (m.len(), mtime)
                })
                .unwrap_or((0, 0))
        })
        .collect()
}



/// `calisto test [--watch] [arquivo|dir...]`
///
/// Detecta minitest (`test/**/*_test.rb`) ou rspec (`spec/**/*_spec.rb`) e
/// roda cada arquivo como um fork do daemon quente — o daemon de teste da app
/// (RAILS_ENV=test no boot, socket proprio) paga o boot UMA vez; por arquivo
/// e so o fork. Arquivos rodam em paralelo (1 worker por CPU, teto no numero
/// de arquivos) aproveitando o accept loop multi-conexao do daemon.
pub fn cmd_test(args: &[String]) -> i32 {
    let mut watch = false;
    let mut filters: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--watch" | "-w" => watch = true,
            s if s.starts_with('-') => {
                eprintln!("calisto: flag desconhecida '{s}'");
                return 1;
            }
            s => filters.push(s.to_string()),
        }
        i += 1;
    }

    load_dotenv(); // .env do cwd (walk up) entra no env dos children de teste
    let app = match load_app_config() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    let root = app
        .as_ref()
        .map(|a| a.root.clone())
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let framework = detect_framework(&root);
    let mut files = Vec::new();
    walk_files(&root.join(framework.dir()), framework.suffix(), &mut files);

    // filtros: diretorios/prefixos restringem a descoberta; arquivos explicitos
    // entram direto (fora da arvore de testes, ex.: um teste temporario)
    if !filters.is_empty() {
        files.retain(|f| {
            filters.iter().any(|pat| {
                let p = Path::new(pat);
                f == p
                    || f.starts_with(p)
                    || f.strip_prefix(&root)
                        .map(|rel| rel == p)
                        .unwrap_or(false)
            })
        });
        for pat in &filters {
            let p = Path::new(pat);
            if p.is_file() {
                // arquivo explicito: resolve contra o root para dedup com o
                // descoberto (que e absoluto) — um pat relativo nao deve
                // adicionar o mesmo arquivo duas vezes
                let abs = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    root.join(p)
                };
                if !files.contains(&abs) {
                    files.push(abs);
                }
            }
        }
    }
    files.sort();
    files.dedup();

    if files.is_empty() {
        eprintln!(
            "calisto: nenhum teste {} encontrado em {} (rode na raiz do projeto)",
            framework.suffix(),
            root.join(framework.dir()).display()
        );
        return 1;
    }

    let Some(ruby) = ruby_or_err() else {
        return 1;
    };
    let extra = [
        ("RAILS_ENV", "test"),
        ("RACK_ENV", "test"),
        ("CALISTO_LOAD_PATH", framework.dir()),
    ];

    let dir = match app_daemon(&app) {
        Some(a) => {
            let dir = app_test_runtime_dir(a, &ruby);
            if let Err(e) = connect_or_spawn_test_daemon(&ruby, a) {
                eprintln!("calisto: {e}");
                return 1;
            }
            dir
        }
        None => {
            let preload = run_preload(&app);
            if let Err(e) = connect_or_spawn_daemon(&ruby, &preload) {
                eprintln!("calisto: {e}");
                return 1;
            }
            daemon_dir_for(&ruby)
        }
    };

    let run_once = || {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(files.len())
            .max(1);
        let next = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let dir = dir.clone();
                let root = root.clone();
                let extra = extra;
                let files = files.clone();
                let next = next.clone();
                std::thread::spawn(move || {
                    let mut results = Vec::new();
                    loop {
                        let idx = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if idx >= files.len() {
                            break;
                        }
                        let file = &files[idx];
                        let t0 = Instant::now();
                        let mut stream = match UnixStream::connect(dir.join("calisto.sock")) {
                            Ok(s) => s,
                            Err(e) => {
                                results.push((
                                    file.display().to_string(),
                                    2,
                                    t0.elapsed(),
                                    format!("cannot connect to daemon: {e}"),
                                ));
                                continue;
                            }
                        };
                        let code = run_request_full(
                            &mut stream,
                            &root.to_string_lossy(),
                            &extra,
                            &file.to_string_lossy(),
                            &[],
                        );
                        results.push((file.display().to_string(), code, t0.elapsed(), String::new()));
                    }
                    results
                })
            })
            .collect();

        let mut results: Vec<(String, i32, Duration, String)> =
            handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
        results.sort_by(|a, b| a.0.cmp(&b.0));
        let mut failures = 0usize;
        let mut total_ms: u128 = 0;
        for (path, code, elapsed, err) in &results {
            let ms = elapsed.as_millis();
            total_ms += ms;
            if *code == 0 {
                println!("ok {path} ({ms}ms)");
            } else {
                failures += 1;
                println!("FAIL {path} ({ms}ms, exit {code}){err}");
            }
        }
        println!(
            "calisto: {} arquivo(s) de teste, {} falhou(ram) em {}ms (framework: {})",
            results.len(),
            failures,
            total_ms,
            framework.name()
        );
        if failures > 0 {
            1
        } else {
            0
        }
    };

    let code = run_once();
    if watch {
        let mut snap = test_file_snapshot(&files);
        loop {
            std::thread::sleep(Duration::from_millis(300));
            let now = test_file_snapshot(&files);
            if now != snap {
                snap = now;
                println!("calisto: mudanca detectada, re-rodando...");
                run_once();
            }
        }
    }
    code
}
