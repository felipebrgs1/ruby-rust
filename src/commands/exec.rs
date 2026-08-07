//! calisto — exec
//!
//! calisto exec + exec_argv (bundle-exec-like, usado pelos scripts).
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — commands/exec (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use crate::appconfig::*;
use crate::protocol::*;
use crate::runtime::*;
use crate::commands::run::run_cold;
use crate::shims::*;







/// Roda um argv (bin + args) como `calisto exec` no daemon quente: o shim
/// `exec.rb` resolve o bin (caminho de arquivo -> spec do bundle ativo ->
/// PATH) e carrega in-process (kernel_load do bundler). Usado por `calisto
/// exec` e pelos scripts de `[scripts]` do calisto.toml (Fase H). `cold` roda
/// o shim no interpretador direto com cwd na raiz (paridade com --cold).
pub fn exec_argv(ruby: &Path, app: &Option<AppConfig>, cold: bool, argv: &[String]) -> i32 {
    if argv.is_empty() {
        eprintln!("calisto: exec needs a command: calisto exec <bin> [args...]");
        return 1;
    }
    let root = app
        .as_ref()
        .map(|a| a.root.clone())
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let dir = match app_daemon(app) {
        Some(a) => app_runtime_dir(a, ruby),
        None => daemon_dir_for(ruby),
    };
    fs::create_dir_all(&dir).ok();
    let shim = dir.join("exec.rb");
    if !shim.is_file() {
        if let Err(e) = fs::write(&shim, EXEC_SHIM) {
            eprintln!("calisto: cannot write exec shim: {e}");
            return 1;
        }
    }

    if cold {
        return run_cold(ruby, &shim.to_string_lossy(), argv, &root);
    }

    let mut stream = match app_daemon(app) {
        Some(a) => match connect_or_spawn_app_daemon(ruby, a) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("calisto: {e}");
                return 1;
            }
        },
        None => match connect_or_spawn_daemon(ruby, &run_preload(app)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("calisto: {e}");
                return 1;
            }
        },
    };
    run_request_full(&mut stream, &root.to_string_lossy(), &[], &shim.to_string_lossy(), argv)
}



/// `calisto exec <bin> [args...]` — roda o binario de uma gem no daemon
/// quente, no contexto da app (Gemfile ativo + boot pre-carregado com
/// calisto.toml). Resolucao no shim ruby (espelho do `bundle exec`), sem
/// depender de binstub; binario ruby e `load` in-process.
pub fn cmd_exec(args: &[String]) -> i32 {
    load_dotenv(); // .env do cwd (walk up) entra no env do binario
    let app = match load_app_config() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    let Some(ruby) = ruby_or_err() else {
        return 1;
    };
    exec_argv(&ruby, &app, false, args)
}
