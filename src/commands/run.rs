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

/// Flags ruby do child (Fase R — paridade de CLI): `-I/-r/-w/-W/-c/-E`.
/// Serializadas em CALISTO_RUN_FLAGS (env_blob do RUN) — o child aplica
/// DEPOIS do Bundler.setup (o -I precisa sobreviver ao cleanup do load
/// path); o cold monta o argv equivalente. Daemons antigos/legado ignoram
/// o campo (var desconhecida no env do child — sem mudanca de protocolo).
#[derive(Default, Clone, Debug)]
pub struct RunFlags {
    pub load_paths: Vec<String>,
    pub requires: Vec<String>,
    /// -1 = `-w` ($VERBOSE=true); 0/1/2 = `-W<n>` (nil/false/true).
    pub verbose: Option<i8>,
    pub syntax_check: bool,
    pub encoding: Option<String>,
}

impl RunFlags {
    pub fn any(&self) -> bool {
        !self.load_paths.is_empty()
            || !self.requires.is_empty()
            || self.verbose.is_some()
            || self.syntax_check
            || self.encoding.is_some()
    }

    /// Formato do env_blob: segmentos `chave:valor` separados por \x1e.
    pub fn blob(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for d in &self.load_paths {
            parts.push(format!("I:{d}"));
        }
        for l in &self.requires {
            parts.push(format!("r:{l}"));
        }
        match self.verbose {
            Some(-1) => parts.push("w".into()),
            Some(n) => parts.push(format!("W:{n}")),
            None => {}
        }
        if self.syntax_check {
            parts.push("c".into());
        }
        if let Some(e) = &self.encoding {
            parts.push(format!("E:{e}"));
        }
        parts.join("\u{1f}")
    }

    /// Parse do blob (lado do daemon/child — espelho do serializador).
    pub fn parse(blob: &str) -> RunFlags {
        let mut f = RunFlags::default();
        for seg in blob.split('\u{1f}') {
            if let Some(v) = seg.strip_prefix("I:") {
                f.load_paths.push(v.to_string());
            } else if let Some(v) = seg.strip_prefix("r:") {
                f.requires.push(v.to_string());
            } else if seg == "w" {
                f.verbose = Some(-1);
            } else if let Some(v) = seg.strip_prefix("W:") {
                if let Ok(n) = v.parse::<i8>() {
                    if (0..=2).contains(&n) {
                        f.verbose = Some(n);
                    }
                }
            } else if seg == "c" {
                f.syntax_check = true;
            } else if let Some(v) = seg.strip_prefix("E:") {
                f.encoding = Some(v.to_string());
            }
        }
        f
    }
}

pub fn cmd_run(args: &[String]) -> i32 {
    let mut cold = false;
    let mut show_time = false;
    let mut preload_opt: Option<String> = None;
    let mut eval_parts: Vec<String> = Vec::new();
    let mut flags = RunFlags::default();
    let mut i = 0;
    while i < args.len() && (args[i].starts_with('-') || args[i] == "-e") {
        let a = args[i].clone();
        // `--` termina o parsing de flags (como o ruby): o resto e script+args
        if a == "--" {
            i += 1;
            break;
        }
        match a.as_str() {
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
            "-I" => {
                i += 1;
                match args.get(i) {
                    Some(v) => flags.load_paths.push(v.clone()),
                    None => {
                        eprintln!("calisto: -I needs a directory");
                        return 1;
                    }
                }
            }
            "-r" => {
                i += 1;
                match args.get(i) {
                    Some(v) => flags.requires.push(v.clone()),
                    None => {
                        eprintln!("calisto: -r needs a library name");
                        return 1;
                    }
                }
            }
            "-w" => flags.verbose = Some(-1),
            "-E" => {
                i += 1;
                match args.get(i) {
                    Some(v) => flags.encoding = Some(v.clone()),
                    None => {
                        eprintln!("calisto: -E needs an encoding name");
                        return 1;
                    }
                }
            }
            "-c" => flags.syntax_check = true,
            "-v" | "--version" => return cmd_run_version(),
            other => {
                // formas anexadas do ruby: -Ilib, -rlib, -W0..2, -Eutf-8
                if let Some(v) = other.strip_prefix("-I") {
                    if !v.is_empty() {
                        flags.load_paths.push(v.to_string());
                        i += 1;
                        continue;
                    }
                } else if let Some(v) = other.strip_prefix("-r") {
                    if !v.is_empty() {
                        flags.requires.push(v.to_string());
                        i += 1;
                        continue;
                    }
                } else if let Some(v) = other.strip_prefix("-W") {
                    if let Ok(n) = v.parse::<i8>() {
                        if (0..=2).contains(&n) {
                            flags.verbose = Some(n);
                            i += 1;
                            continue;
                        }
                    }
                } else if let Some(v) = other.strip_prefix("-E") {
                    if !v.is_empty() {
                        flags.encoding = Some(v.to_string());
                        i += 1;
                        continue;
                    }
                }
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
            run_cold_eval(&ruby, &code, script_args, &flags)
        } else if let Some(app) = app_daemon(&app) {
            run_fast_app_eval(&ruby, &code, script_args, app, &flags)
        } else {
            run_fast_eval(&ruby, &code, script_args, &preload, &flags)
        }
    } else if let Some(cmd) = &script_cmd {
        // script do calisto.toml: roda como `calisto exec` no daemon (resolve
        // o bin no bundle/PATH e carrega in-process), com os args do CLI no
        // final do comando. --cold roda o shim no interpretador direto
        // (paridade cold/warm e invariante do run).
        if flags.any() {
            // as flags ruby (-I/-r/-w/-W/-c/-E) sao opcoes do interpretador
            // para arquivo/-e; num script do calisto.toml o comando e outro
            // (ex.: "bin/rails server") — ignorar silenciosamente confundiria
            eprintln!(
                "calisto: ruby flags (-I/-r/-w/-W/-c/-E) nao se aplicam a scripts do calisto.toml"
            );
            return 1;
        }
        let mut full = cmd.clone();
        full.extend(script_args.iter().cloned());
        exec_argv(&ruby, &app, cold, &full)
    } else if cold {
        run_cold(&ruby, script.unwrap(), script_args, &cwd, &flags)
    } else if let Some(app) = app_daemon(&app) {
        run_fast_app(&ruby, script.unwrap(), script_args, app, &flags)
    } else {
        run_fast(&ruby, script.unwrap(), script_args, &preload, &flags)
    };
    if show_time {
        eprintln!("calisto: elapsed: {:?}", t0.elapsed());
    }
    code
}



/// `calisto run -v` / `--version`: paridade com `ruby -v` — imprime a
/// descricao da VM (RUBY_DESCRIPTION do ruby resolvido, Fase I) e sai 0
/// sem rodar script nenhum.
pub fn cmd_run_version() -> i32 {
    let Some(ruby) = ruby_or_err() else {
        return 1;
    };
    match Command::new(&ruby).arg("-v").status() {
        Ok(st) => exit_code(st),
        Err(e) => {
            eprintln!("calisto: cannot execute {}: {e}", ruby.display());
            1
        }
    }
}



pub fn run_cold(ruby: &Path, script: &str, args: &[String], cwd: &Path, flags: &RunFlags) -> i32 {
    // -rbundler/setup: ativa o Gemfile do cwd (como `bundle exec ruby`);
    // no-op fora de bundle, mantendo a paridade com o daemon warm.
    // -I <runtime>: shims nativos calisto/sqlite.rb + calisto/hash.rb (o
    // hash cai no fallback Digest — paridade cold/warm; o sqlite levanta
    // LoadError claro, e nativo do daemon).
    // Fase R: flags ruby na ordem do child warm — -w antes do bundler/setup
    // (paridade de warnings), -I/-r/-E depois (o Bundler.setup limpa o
    // $LOAD_PATH), -c por ultimo (o ruby -c pula os requires).
    let shims = native_shims_dir(ruby);
    let mut cmd = Command::new(ruby);
    match flags.verbose {
        Some(-1) => { cmd.arg("-w"); }
        Some(n) => { cmd.arg(format!("-W{n}")); }
        None => {}
    };
    cmd.arg("-I").arg(&shims).arg("-rbundler/setup");
    if let Some(enc) = &flags.encoding {
        cmd.arg("-E").arg(enc);
    }
    for d in &flags.load_paths {
        cmd.arg("-I").arg(d);
    }
    for l in &flags.requires {
        cmd.arg("-r").arg(l);
    }
    if flags.syntax_check {
        cmd.arg("-c");
    }
    cmd.arg(script).args(args).current_dir(cwd);
    match cmd.status() {
        Ok(st) => exit_code(st),
        Err(e) => {
            eprintln!("calisto: cannot execute {}: {e}", ruby.display());
            1
        }
    }
}



/// `--cold -e 'code'`: interpretador direto com `-e`, idem `ruby -e`.
pub fn run_cold_eval(ruby: &Path, code: &str, args: &[String], flags: &RunFlags) -> i32 {
    let shims = native_shims_dir(ruby);
    let mut cmd = Command::new(ruby);
    match flags.verbose {
        Some(-1) => { cmd.arg("-w"); }
        Some(n) => { cmd.arg(format!("-W{n}")); }
        None => {}
    };
    cmd.arg("-I").arg(&shims).arg("-rbundler/setup");
    if let Some(enc) = &flags.encoding {
        cmd.arg("-E").arg(enc);
    }
    for d in &flags.load_paths {
        cmd.arg("-I").arg(d);
    }
    for l in &flags.requires {
        cmd.arg("-r").arg(l);
    }
    if flags.syntax_check {
        cmd.arg("-c");
    }
    cmd.arg("-e").arg(code).args(args);
    match cmd.status() {
        Ok(st) => exit_code(st),
        Err(e) => {
            eprintln!("calisto: cannot execute {}: {e}", ruby.display());
            1
        }
    }
}



pub fn run_fast(ruby: &Path, script: &str, args: &[String], preload: &str, flags: &RunFlags) -> i32 {
    let mut stream = match connect_or_spawn_daemon(ruby, preload) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    run_request(&mut stream, script, args, flags)
}



/// Fase B: daemon dedicado da app (entrypoint pre-carregado no boot).
pub fn run_fast_app(ruby: &Path, script: &str, args: &[String], app: &AppConfig, flags: &RunFlags) -> i32 {
    let mut stream = match connect_or_spawn_app_daemon(ruby, app) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    run_request(&mut stream, script, args, flags)
}



pub fn run_fast_eval(ruby: &Path, code: &str, args: &[String], preload: &str, flags: &RunFlags) -> i32 {
    let mut stream = match connect_or_spawn_daemon(ruby, preload) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    eval_request(&mut stream, code, args, flags)
}



/// `-e` no daemon da app (boot congelado, como o run normal).
pub fn run_fast_app_eval(ruby: &Path, code: &str, args: &[String], app: &AppConfig, flags: &RunFlags) -> i32 {
    let mut stream = match connect_or_spawn_app_daemon(ruby, app) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    eval_request(&mut stream, code, args, flags)
}
