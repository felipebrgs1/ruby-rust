//! calisto — runtime
//!
//! dirs de runtime, resolucao do ruby (Fase I) e spawn/connect dos daemons.
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — runtime (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::os::unix::net::{UnixStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use crate::appconfig::*;
use crate::protocol::*;






pub const PINNED_RUBY: &str = "3.4.10";



pub fn uid() -> u32 {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in status.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() == Some("Uid:") {
            return parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        }
    }
    0
}



pub fn runtime_dir() -> PathBuf {
    if let Ok(d) = env::var("CALISTO_RUNTIME_DIR") {
        let d = PathBuf::from(d);
        fs::create_dir_all(&d).ok();
        return d;
    }
    if let Ok(x) = env::var("XDG_RUNTIME_DIR") {
        let d = PathBuf::from(x).join("calisto");
        if fs::create_dir_all(&d).is_ok() {
            return d;
        }
    }
    let d = PathBuf::from(format!("/tmp/calisto-{}", uid()));
    fs::create_dir_all(&d).ok();
    d
}



/// Dir que contem `vendor/` — a base dos rubies empacotados (Fase Q.3).
/// Ordem: `CALISTO_HOME` (instalacao portatil do curl|sh — `~/.calisto`,
/// vendor la dentro; se apontar para um dir sem vendor, o resolve_ruby erra
/// com a mensagem de build — a instalacao esta quebrada) > busca subindo do
/// proprio executavel (comportamento de checkout/desenvolvimento — o
/// binario vive em target/debug e o vendor na raiz do repo) > `vendor`
/// relativo ao cwd (fallback historico).
pub fn vendor_root() -> Option<PathBuf> {
    if let Ok(home) = env::var("CALISTO_HOME") {
        let v = PathBuf::from(&home).join("vendor");
        if !home.is_empty() {
            return Some(v);
        }
    }
    let mut dir = env::current_exe().ok()?.parent().map(Path::to_path_buf);
    while let Some(d) = dir {
        if d.join("vendor").is_dir() {
            return Some(d.join("vendor"));
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    let rel = PathBuf::from("vendor");
    rel.is_dir().then_some(rel)
}



pub fn vendor_ruby(vendor: &Option<PathBuf>, version: &str) -> Option<PathBuf> {
    let cand = vendor.as_ref()?.join(format!("ruby-{version}/bin/ruby"));
    cand.is_file().then_some(cand)
}



/// Versao pedida pela primeira linha do `.ruby-version` (rbenv): prefixo
/// `ruby-` tolerado, linhas vazias ignoradas.
pub fn ruby_version_from_file(file: &Path) -> Option<String> {
    let content = fs::read_to_string(file).ok()?;
    let v = content
        .lines()
        .next()?
        .trim()
        .trim_start_matches("ruby-")
        .trim();
    (!v.is_empty()).then(|| v.to_string())
}



/// Versao pedida pela diretiva `ruby "x.y.z"` do Gemfile (walk up) — ou
/// `ruby file: ".ruby-version"`, que segue o arquivo. So consultada quando nao
/// ha `.ruby-version`.
pub fn ruby_version_from_gemfile(file: &Path) -> Option<String> {
    let content = fs::read_to_string(file).ok()?;
    for line in content.lines() {
        let Some(rest) = line.trim().strip_prefix("ruby") else {
            continue; // linha comum (source, gem, group...) — segue procurando
        };
        let rest = rest.trim_start();
        // ruby "3.4.4" | ruby '3.4.4'
        let quoted = rest
            .strip_prefix('"')
            .and_then(|s| s.split('"').next())
            .or_else(|| rest.strip_prefix('\'').and_then(|s| s.split('\'').next()))
            .unwrap_or("");
        if !quoted.is_empty() && quoted.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Some(quoted.to_string());
        }
        // ruby file: ".ruby-version"
        if let Some(name) = rest.strip_prefix("file:") {
            if let Some(name) = name.trim().strip_prefix('"').and_then(|s| s.split('"').next()) {
                return ruby_version_from_file(&file.parent()?.join(name));
            }
        }
    }
    None
}



/// Resolve o ruby do calisto (Fase I — multi-versoes):
///   CALISTO_RUBY (override) > .ruby-version (walk up, rbenv) > diretiva
///   `ruby "x.y.z"` do Gemfile > vendor/current (pin default).
/// Versao pedida que nao esta instalada em vendor/ruby-<v> e ERRO claro com o
/// comando de build (substitui o warning da Fase 1-2).
pub fn resolve_ruby() -> Result<PathBuf, String> {
    if let Ok(p) = env::var("CALISTO_RUBY") {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return Ok(pb);
        }
        eprintln!("calisto: warning: CALISTO_RUBY={p} is not a file; ignoring");
    }
    let vendor = vendor_root();
    if let Some(file) = find_in_parents(".ruby-version") {
        if let Some(want) = ruby_version_from_file(&file) {
            if let Some(ruby) = vendor_ruby(&vendor, &want) {
                return Ok(ruby);
            }
            return Err(format!(
                "calisto: ruby {want} pedido por {} nao esta instalado \
                 (vendor/ruby-{want} ausente); rode RUBY_VERSION={want} scripts/build-ruby.sh",
                file.display()
            ));
        }
    }
    let gemfile = env::var_os("BUNDLE_GEMFILE")
        .map(PathBuf::from)
        .or_else(|| find_in_parents("Gemfile"));
    if let Some(gemfile) = gemfile {
        if let Some(want) = ruby_version_from_gemfile(&gemfile) {
            if let Some(ruby) = vendor_ruby(&vendor, &want) {
                return Ok(ruby);
            }
            return Err(format!(
                "calisto: ruby {want} pedido pelo Gemfile nao esta instalado \
                 (vendor/ruby-{want} ausente); rode RUBY_VERSION={want} scripts/build-ruby.sh"
            ));
        }
    }
    if let Some(ruby) = vendor_ruby(&vendor, PINNED_RUBY) {
        return Ok(ruby);
    }
    if let Some(r) = &vendor {
        let cand = r.join("current/bin/ruby");
        if cand.is_file() {
            return Ok(cand);
        }
    }
    eprintln!("calisto: warning: no pinned ruby found (run scripts/build-ruby.sh); falling back to PATH");
    Ok(PathBuf::from("ruby"))
}



/// Resolve o ruby ou imprime o erro e retorna None (callers: `let Some(ruby)
/// = ruby_or_err() else { return 1 };`).
pub fn ruby_or_err() -> Option<PathBuf> {
    match resolve_ruby() {
        Ok(ruby) => Some(ruby),
        Err(e) => {
            eprintln!("{e}");
            None
        }
    }
}



/// Versao deduzida do caminho do ruby (`vendor/ruby-<v>/bin/ruby`). None para
/// o pin default (vendor/current) e para rubies fora do vendor — esses usam os
/// runtime dirs classicos (sem mudar hashes nem sockets existentes).
pub fn ruby_version_of(path: &Path) -> Option<String> {
    let name = path.parent()?.parent()?.file_name()?.to_str()?;
    let v = name.strip_prefix("ruby-")?;
    (v != PINNED_RUBY).then(|| v.to_string())
}



/// Runtime dir do daemon generico: por versao quando o ruby resolvido nao e o
/// pin default — daemons de VMs diferentes nao podem dividir o socket.
pub fn daemon_dir_for(ruby: &Path) -> PathBuf {
    match ruby_version_of(ruby) {
        Some(v) => runtime_dir().join(format!("ruby-{v}")),
        None => runtime_dir(),
    }
}



pub fn daemon_connect_at(dir: &Path) -> Option<UnixStream> {
    UnixStream::connect(dir.join("calisto.sock")).ok()
}



pub fn connect_or_spawn_daemon_in(
    ruby: &Path,
    dir: &Path,
    preload: &str,
    flags: &[&str],
    setup: impl FnOnce(&mut Command),
    compact: bool,
) -> Result<UnixStream, String> {
    let sock = dir.join("calisto.sock");
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // Fase S: modo unico de daemon — o proprio binario calisto vira o
    // processo do daemon com a VM CRuby in-process (dlopen da libruby via
    // crates/calisto-ruby; accept loop em Rust). Ruby sem libruby.so (build
    // pre --enable-shared — instalacoes antigas) e erro claro com o comando
    // de rebuild: o daemon legado morreu na Fase S. flags sao
    // repassadas como `-r<gem>` do boot (`-rbundler/setup` antes do script
    // continua sendo flag de verdade).
    let _ = calisto_ruby::libruby_path(ruby).ok_or_else(|| {
        let version = ruby_version_of(ruby).unwrap_or_else(|| PINNED_RUBY.to_string());
        format!(
            "{} nao tem libruby.so (build pre --enable-shared); \
             rode CALISTO_REBUILD=1 RUBY_VERSION={version} scripts/build-ruby.sh",
            ruby.display()
        )
    })?;
    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot resolve own executable: {e}"))?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon").arg("--internal").args(flags);
    cmd.env("CALISTO_EMBED_RUBY", ruby);
    cmd.env("CALISTO_SOCKET", &sock)
        .env("CALISTO_PIDFILE", dir.join("calisto.pid"))
        .env("CALISTO_PRELOAD", preload)
        // Fase M.3: limita as arenas do glibc no daemon — o heap do preload
        // fragmenta em arenas por thread que os children (fork single-thread,
        // pos rb_thread_atfork) nunca usam. 2 arenas cobrem main + timer
        // thread da VM; o child herda a config ja lida pelo glibc.
        .env("MALLOC_ARENA_MAX", "2")
        // Fase M.1: compactacao pre-fork (GC.start + GC.compact pos-boot) —
        // default on no daemon de app; ver `app_compact`.
        .env("CALISTO_COMPACT", if compact { "1" } else { "0" })
        .stdin(Stdio::null());
    setup(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start daemon: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(s) = UnixStream::connect(&sock) {
            return Ok(s);
        }
        match child.try_wait() {
            Ok(Some(st)) => return Err(format!("daemon exited early ({st}); see output above")),
            Ok(None) => {}
            Err(e) => return Err(format!("waiting on daemon: {e}")),
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Err("timed out waiting for daemon socket".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}



pub fn connect_or_spawn_daemon(ruby: &Path, preload: &str) -> Result<UnixStream, String> {
    // Daemon generico: sem app, sem compactacao (heap pequeno; o default on
    // e do daemon de app — `[run] compact` nao faz sentido aqui).
    connect_or_spawn_daemon_in(ruby, &daemon_dir_for(ruby), preload, &[], |_| {}, false)
}



pub fn connect_or_spawn_app_daemon(ruby: &Path, app: &AppConfig) -> Result<UnixStream, String> {
    connect_or_spawn_app_daemon_in(ruby, app, &app_runtime_dir(app, ruby), &[])
}



/// Daemon da app em modo teste (RAILS_ENV=test no boot, socket proprio).
pub fn connect_or_spawn_test_daemon(ruby: &Path, app: &AppConfig) -> Result<UnixStream, String> {
    connect_or_spawn_app_daemon_in(ruby, app, &app_test_runtime_dir(app, ruby), &[
        ("RAILS_ENV", "test"),
        ("RACK_ENV", "test"),
    ])
}



pub fn connect_or_spawn_app_daemon_in(
    ruby: &Path,
    app: &AppConfig,
    dir: &Path,
    envs: &[(&str, &str)],
) -> Result<UnixStream, String> {
    let compact = app_compact(app)?;
    let yjit = app_yjit(app)?;
    let warmup = app_warmup(app)?;
    let preload = app.preload.as_ref().map(|p| p.display().to_string());
    // Fase N: --yjit e flag real do interpretador (ruby_options no modo
    // embutido; `ruby --yjit ...` no legado) — o JIT liga antes do preload/
    // warmup, entao o codigo quente compilado no boot e herdado pelos forks.
    let mut flags: Vec<&str> = vec!["-rbundler/setup"];
    if yjit {
        flags.push("--yjit");
    }
    // Daemon da app: boota com o Gemfile ativo (-rbundler/setup ANTES do
    // script — flag real) e cwd na raiz da app, e o entrypoint e carregado
    // no boot (CALISTO_APP_PRELOAD), seguido do warmup (CALISTO_APP_WARMUP)
    // e da compactacao (CALISTO_COMPACT).
    connect_or_spawn_daemon_in(ruby, dir, "", &flags, |cmd| {
        cmd.current_dir(&app.root);
        if let Some(p) = &preload {
            cmd.env("CALISTO_APP_PRELOAD", p);
        }
        if let Some(w) = &warmup {
            cmd.env("CALISTO_APP_WARMUP", w);
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
    }, compact)
}



/// Dir de runtime de status/stop/doctor: o daemon da app quando o cwd esta
/// Dir de runtime de status/stop/doctor: o daemon da app quando o cwd esta
/// numa app com calisto.toml; senao o generico (por versao, Fase I). Parse
/// quebrado ou versao ausente -> warning com o dir generico.
pub fn current_runtime_dir() -> PathBuf {
    let ruby = match resolve_ruby() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("calisto: warning: {e}");
            return runtime_dir();
        }
    };
    match load_app_config() {
        Ok(app) => match app_daemon(&app) {
            Some(a) => app_runtime_dir(a, &ruby),
            None => daemon_dir_for(&ruby),
        },
        Err(e) => {
            eprintln!("calisto: warning: {e}");
            daemon_dir_for(&ruby)
        }
    }
}



pub fn stop_daemon_at(dir: &Path) {
    // O BYE e respondido ANTES do shutdown (unlink do socket) — um stop que
    // retorna com o socket ainda no ar e uma corrida para quem observa o
    // resultado (os testes assertam a remocao). Alem disso, o daemon legado
    // pode descartar a conexao do STOP (recovery de fd invalido no select)
    // sem processar o comando. Retry + espera: stop significa "daemon
    // realmente parado e socket removido".
    for _ in 0..5 {
        if let Some(mut s) = daemon_connect_at(dir) {
            if send_cmd(&mut s, "STOP", &[]).is_ok() {
                let _ = read_line(&mut s);
            }
        }
        let deadline = Instant::now() + Duration::from_millis(400);
        while dir.join("calisto.sock").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if !dir.join("calisto.sock").exists() {
            return;
        }
    }
    // Daemon morto/morto demais: remove o socket stale (o proximo run faz o
    // stale-socket recovery e rebinda sem conflito). Seguro: acabamos de
    // falhar em conectar 5x.
    let _ = fs::remove_file(dir.join("calisto.sock"));
    let _ = fs::remove_file(dir.join("calisto.pid"));
}
