//! calisto — repl
//!
//! calisto repl (IRB no daemon).
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — commands/repl (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::fs;
use std::path::PathBuf;
use crate::appconfig::*;
use crate::protocol::*;
use crate::runtime::*;
use crate::shims::*;







/// `calisto repl [args...]` — IRB interativo como child do fork: no daemon da
/// app (calisto.toml) o REPL herda o boot pre-carregado (console de app); no
/// daemon generico, stdlib preloaded. Fica em foreground; Ctrl-C/kill no
/// cliente derruba o child via client-death kill (como `calisto serve`).
pub fn cmd_repl(args: &[String]) -> i32 {
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

    let Some(ruby) = ruby_or_err() else {
        return 1;
    };
    let dir = match app_daemon(&app) {
        Some(a) => app_runtime_dir(a, &ruby),
        None => daemon_dir_for(&ruby),
    };
    fs::create_dir_all(&dir).ok();
    let shim = dir.join("repl.rb");
    if !shim.is_file() {
        if let Err(e) = fs::write(&shim, REPL_SHIM) {
            eprintln!("calisto: cannot write repl shim: {e}");
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
    run_request_full(&mut stream, &root.to_string_lossy(), &[], &shim.to_string_lossy(), args, &crate::commands::run::RunFlags::default())
}
