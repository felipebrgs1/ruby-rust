//! calisto — serve
//!
//! calisto serve (config.ru via rackup).
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — commands/serve (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::fs;
use std::path::PathBuf;
use crate::appconfig::*;
use crate::protocol::*;
use crate::runtime::*;
use crate::shims::*;







/// `calisto serve [-p PORT] [-o HOST]` — sobe a Rack app do config.ru como
/// child do fork do daemon quente (rackup/rack com puma/webrick do bundle).
/// Fica em foreground; Ctrl-C/kill no cliente derruba o server via
/// client-death kill do daemon.
pub fn cmd_serve(args: &[String]) -> i32 {
    let mut port = "3000".to_string();
    let mut host = "127.0.0.1".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-p" | "--port" => {
                i += 1;
                match args.get(i) {
                    Some(v) => port = v.clone(),
                    None => {
                        eprintln!("calisto: -p precisa de um valor");
                        return 1;
                    }
                }
            }
            "-o" | "--host" => {
                i += 1;
                match args.get(i) {
                    Some(v) => host = v.clone(),
                    None => {
                        eprintln!("calisto: -o precisa de um valor");
                        return 1;
                    }
                }
            }
            s => {
                eprintln!("calisto: argumento inesperado '{s}'");
                return 1;
            }
        }
        i += 1;
    }

    load_dotenv();
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
    if !root.join("config.ru").is_file() {
        eprintln!(
            "calisto serve: {} nao tem config.ru (Rack app esperado)",
            root.display()
        );
        return 1;
    }

    let Some(ruby) = ruby_or_err() else {
        return 1;
    };
    let dir = match app_daemon(&app) {
        Some(a) => app_runtime_dir(a, &ruby),
        None => daemon_dir_for(&ruby),
    };
    fs::create_dir_all(&dir).ok();
    let launcher = dir.join("serve.rb");
    if !launcher.is_file() {
        if let Err(e) = fs::write(&launcher, SERVE_LAUNCHER) {
            eprintln!("calisto: cannot write serve launcher: {e}");
            return 1;
        }
    }

    let mut stream = match app_daemon(&app) {
        Some(a) => match connect_or_spawn_app_daemon(&ruby, a) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("calisto: {e}");
                return 1;
            }
        },
        None => match connect_or_spawn_daemon(&ruby, &run_preload(&app)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("calisto: {e}");
                return 1;
            }
        },
    };
    let extra = [
        ("CALISTO_SERVE_PORT", port.as_str()),
        ("CALISTO_SERVE_HOST", host.as_str()),
    ];
    run_request_full(&mut stream, &root.to_string_lossy(), &extra, &launcher.to_string_lossy(), &[])
}
