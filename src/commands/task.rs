//! calisto — task
//!
//! calisto task (rake no daemon quente).
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — commands/task (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::fs;
use std::path::PathBuf;
use crate::appconfig::*;
use crate::protocol::*;
use crate::runtime::*;
use crate::shims::*;







/// `calisto task <args...>` — rake no daemon quente (ex.: `calisto task
/// db:migrate`). Mesma semantica de `calisto run bin/rake <args>` no daemon
/// da app (dev); sem calisto.toml usa o daemon generico.
pub fn cmd_task(args: &[String]) -> i32 {
    load_dotenv(); // .env do cwd (walk up) entra no env do rake
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
    // nome "task.rb" (nao "rake.rb"): com o dir de runtime no $LOAD_PATH
    // (Fase P — shims calisto/*), um "rake.rb" no dir sombrearia o
    // `require "rake"` do proprio rake (o exe/rake faz require "rake")
    let shim = dir.join("task.rb");
    if !shim.is_file() {
        if let Err(e) = fs::write(&shim, RAKE_SHIM) {
            eprintln!("calisto: cannot write rake shim: {e}");
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
