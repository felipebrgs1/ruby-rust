//! calisto — run
//!
//! calisto run (script, -e, scripts do calisto.toml).
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — commands/run (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command};
use std::time::{Instant};
use crate::appconfig::*;
use crate::commands::exec::exec_argv;
use crate::protocol::*;
use crate::runtime::*;
use crate::shims::*;







// ---- commands -----------------------------------------------------------------

pub fn cmd_run(args: &[String]) -> i32 {
    let mut cold = false;
    let mut show_time = false;
    let mut preload_opt: Option<String> = None;
    let mut eval_parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() && (args[i].starts_with("--") || args[i] == "-e") {
        match args[i].as_str() {
            "--cold" => cold = true,
            "--time" => show_time = true,
            "-e" | "--eval" => {
                i += 1;
                match args.get(i) {
                    Some(code) => eval_parts.push(code.clone()),
                    None => {
                        eprintln!("calisto: -e needs code");
                        return 1;
                    }
                }
            }
            "--preload" => {
                i += 1;
                match args.get(i) {
                    Some(v) => preload_opt = Some(v.clone()),
                    None => {
                        eprintln!("calisto: --preload needs a value");
                        return 1;
                    }
                }
            }
            other => {
                eprintln!("calisto: unknown flag '{other}'");
                return 1;
            }
        }
        i += 1;
    }
    let rest = &args[i..];
    let eval_mode = !eval_parts.is_empty();
    // `-e` no lugar do script: multiplos -e viram um codigo so (o ruby junta
    // com "\n"; __LINE__ segue a concatenacao). Args restantes = ARGV.
    let script = if eval_mode { None } else { rest.first() };
    let script_args = if eval_mode {
        rest
    } else if rest.is_empty() {
        &[]
    } else {
        &rest[1..]
    };

    load_dotenv(); // .env do cwd (walk up) entra no env do run/cold/daemon

    let app = match load_app_config() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };

    // Fase H: um nome que nao e arquivo resolve para [scripts.NAME] do
    // calisto.toml (o package.json do Ruby) — arquivo existente sempre vence.
    // Fase J: `calisto run` sem script roda o `start` do calisto.toml quando
    // existe (convencao npm/bun — e o que o `calisto init` gera).
    let mut script_cmd: Option<Vec<String>> = None;
    if !eval_mode {
        match rest.first() {
            Some(s) => {
                if !Path::new(s).is_file() {
                    match app.as_ref().map(|a| a.script_command(s)) {
                        Some(Ok(Some(argv))) => script_cmd = Some(argv),
                        Some(Err(e)) => {
                            eprintln!("calisto: {e}");
                            return 1;
                        }
                        Some(Ok(None)) | None => {
                            eprintln!(
                                "calisto: cannot open {s}: no such file (e sem [scripts.{s}] no calisto.toml)"
                            );
                            return 1;
                        }
                    }
                }
            }
            None => match app.as_ref().map(|a| a.script_command("start")) {
                Some(Ok(Some(argv))) => script_cmd = Some(argv),
                Some(Err(e)) => {
                    eprintln!("calisto: {e}");
                    return 1;
                }
                Some(Ok(None)) | None => {
                    eprintln!(
                        "calisto: run needs a script or -e: calisto run [flags] [-e 'code' | script.rb] [args...]"
                    );
                    return 1;
                }
            },
        }
    }

    let Some(ruby) = ruby_or_err() else {
        return 1;
    };
    let preload = match &preload_opt {
        Some(v) => normalize_preload(v),
        None => run_preload(&app),
    };
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let t0 = Instant::now();
    let code = if eval_mode {
        let code = eval_parts.join("\n");
        if cold {
            run_cold_eval(&ruby, &code, script_args)
        } else if let Some(app) = app_daemon(&app) {
            run_fast_app_eval(&ruby, &code, script_args, app)
        } else {
            run_fast_eval(&ruby, &code, script_args, &preload)
        }
    } else if let Some(cmd) = &script_cmd {
        // script do calisto.toml: roda como `calisto exec` no daemon (resolve
        // o bin no bundle/PATH e carrega in-process), com os args do CLI no
        // final do comando. --cold roda o shim no interpretador direto
        // (paridade cold/warm e invariante do run).
        let mut full = cmd.clone();
        full.extend(script_args.iter().cloned());
        exec_argv(&ruby, &app, cold, &full)
    } else if cold {
        run_cold(&ruby, script.unwrap(), script_args, &cwd)
    } else if let Some(app) = app_daemon(&app) {
        run_fast_app(&ruby, script.unwrap(), script_args, app)
    } else {
        run_fast(&ruby, script.unwrap(), script_args, &preload)
    };
    if show_time {
        eprintln!("calisto: elapsed: {:?}", t0.elapsed());
    }
    code
}



pub fn run_cold(ruby: &Path, script: &str, args: &[String], cwd: &Path) -> i32 {
    // -rbundler/setup: ativa o Gemfile do cwd (como `bundle exec ruby`);
    // no-op fora de bundle, mantendo a paridade com o daemon warm.
    // -I <runtime>: shims nativos calisto/sqlite.rb + calisto/hash.rb (o
    // hash cai no fallback Digest — paridade cold/warm; o sqlite levanta
    // LoadError claro, e nativo do daemon).
    let shims = native_shims_dir(ruby);
    match Command::new(ruby)
        .arg("-I")
        .arg(&shims)
        .arg("-rbundler/setup")
        .arg(script)
        .args(args)
        .current_dir(cwd)
        .status() {
        Ok(st) => exit_code(st),
        Err(e) => {
            eprintln!("calisto: cannot execute {}: {e}", ruby.display());
            1
        }
    }
}



/// `--cold -e 'code'`: interpretador direto com `-e`, idem `ruby -e`.
pub fn run_cold_eval(ruby: &Path, code: &str, args: &[String]) -> i32 {
    let shims = native_shims_dir(ruby);
    match Command::new(ruby)
        .arg("-I")
        .arg(&shims)
        .arg("-rbundler/setup")
        .arg("-e")
        .arg(code)
        .args(args)
        .status() {
        Ok(st) => exit_code(st),
        Err(e) => {
            eprintln!("calisto: cannot execute {}: {e}", ruby.display());
            1
        }
    }
}



pub fn run_fast(ruby: &Path, script: &str, args: &[String], preload: &str) -> i32 {
    let mut stream = match connect_or_spawn_daemon(ruby, preload) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    run_request(&mut stream, script, args)
}



/// Fase B: daemon dedicado da app (entrypoint pre-carregado no boot).
pub fn run_fast_app(ruby: &Path, script: &str, args: &[String], app: &AppConfig) -> i32 {
    let mut stream = match connect_or_spawn_app_daemon(ruby, app) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    run_request(&mut stream, script, args)
}



pub fn run_fast_eval(ruby: &Path, code: &str, args: &[String], preload: &str) -> i32 {
    let mut stream = match connect_or_spawn_daemon(ruby, preload) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    eval_request(&mut stream, code, args)
}



/// `-e` no daemon da app (boot congelado, como o run normal).
pub fn run_fast_app_eval(ruby: &Path, code: &str, args: &[String], app: &AppConfig) -> i32 {
    let mut stream = match connect_or_spawn_app_daemon(ruby, app) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    eval_request(&mut stream, code, args)
}
