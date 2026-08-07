use std::env;
use std::ffi::c_void;
use std::fs;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const DAEMON_RB: &str = include_str!("daemon/server.rb");
const DEFAULT_PRELOAD: &str = concat!(
    "json,yaml,erb,pathname,fileutils,time,date,digest,base64,uri,",
    "net/http,ostruct,set,csv,stringio,logger,socket"
);
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const PINNED_RUBY: &str = "3.4.10";

// ---- raw sendmsg with SCM_RIGHTS (fd passing over unix sockets) ---------------
// Enough Linux x86_64/aarch64 ABI: msghdr + cmsg laid out by hand, no libc dep.

#[repr(C)]
struct MsgHdr {
    msg_name: *mut c_void,
    msg_namelen: u32, // socklen_t
    msg_iov: *mut Iovec,
    msg_iovlen: usize,
    msg_control: *mut c_void,
    msg_controllen: usize,
    msg_flags: i32,
}

#[repr(C)]
struct Iovec {
    iov_base: *mut c_void,
    iov_len: usize,
}

#[repr(C)]
struct Cmsghdr {
    cmsg_len: usize,
    cmsg_level: i32,
    cmsg_type: i32,
}

const SOL_SOCKET: i32 = 1;
const SCM_RIGHTS: i32 = 1;

unsafe extern "C" {
    fn sendmsg(fd: i32, msg: *const MsgHdr, flags: i32) -> isize;
    fn signal(signum: i32, handler: usize) -> usize;
}

const SIGPIPE: i32 = 13;
const SIG_DFL: usize = 0;

/// Comportamento Unix padrao: morrer silenciosamente com SIGPIPE quando o
/// consumidor fecha o pipe (ex.: `calisto doctor | head`), em vez de panicar
/// com EPIPE (Rust ignora SIGPIPE por padrao).
fn reset_sigpipe() {
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}

fn align8(n: usize) -> usize {
    (n + 7) & !7
}

fn send_with_fds(stream: &mut UnixStream, data: &[u8], fds: &[RawFd]) -> io::Result<()> {
    let mut data = data.to_vec();
    let mut iov = Iovec {
        iov_base: data.as_mut_ptr() as *mut c_void,
        iov_len: data.len(),
    };
    let cmsg_len = align8(size_of::<Cmsghdr>()) + fds.len() * size_of::<i32>();
    let control_len = align8(cmsg_len);
    let mut control = vec![0u8; control_len];
    unsafe {
        let cmsg = control.as_mut_ptr() as *mut Cmsghdr;
        (*cmsg).cmsg_len = cmsg_len;
        (*cmsg).cmsg_level = SOL_SOCKET;
        (*cmsg).cmsg_type = SCM_RIGHTS;
        let data_off = align8(size_of::<Cmsghdr>());
        std::ptr::copy_nonoverlapping(
            fds.as_ptr() as *const u8,
            control.as_mut_ptr().add(data_off),
            fds.len() * size_of::<i32>(),
        );
        let msg = MsgHdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: &mut iov,
            msg_iovlen: 1,
            msg_control: control.as_mut_ptr() as *mut c_void,
            msg_controllen: control_len,
            msg_flags: 0,
        };
        if sendmsg(stream.as_raw_fd(), &msg, 0) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn main() {
    reset_sigpipe();
    let argv: Vec<String> = env::args().skip(1).collect();
    let code = match argv.first().map(String::as_str) {
        Some("run") => cmd_run(&argv[1..]),
        Some("test") => cmd_test(&argv[1..]),
        Some("task") => cmd_task(&argv[1..]),
        Some("serve") => cmd_serve(&argv[1..]),
        Some("build") => cmd_build(&argv[1..]),
        Some("status") => cmd_status(),
        Some("stop") => cmd_stop(),
        Some("doctor") => cmd_doctor(),
        Some("help" | "-h" | "--help") => {
            print_help();
            0
        }
        Some(other) => {
            eprintln!("calisto: unknown command '{other}'");
            print_help();
            1
        }
        None => {
            print_help();
            0
        }
    };
    std::process::exit(code);
}

// ---- utils -------------------------------------------------------------------

fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[((n >> 18) & 63) as usize] as char);
        out.push(B64[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            B64[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 { B64[(n & 63) as usize] as char } else { '=' });
    }
    out
}

fn b64(s: &str) -> String {
    b64_encode(s.as_bytes())
}

fn uid() -> u32 {
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

fn runtime_dir() -> PathBuf {
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

fn ruby_path() -> PathBuf {
    if let Ok(p) = env::var("CALISTO_RUBY") {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return pb;
        }
        eprintln!("calisto: warning: CALISTO_RUBY={p} is not a file; ignoring");
    }
    if let Ok(exe) = env::current_exe() {
        let mut dir = exe.parent().map(Path::to_path_buf);
        while let Some(d) = dir {
            let cand = d.join("vendor/current/bin/ruby");
            if cand.is_file() {
                return cand;
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }
    let rel = PathBuf::from("vendor/current/bin/ruby");
    if rel.is_file() {
        return rel;
    }
    eprintln!("calisto: warning: no pinned ruby found (run scripts/build-ruby.sh); falling back to PATH");
    PathBuf::from("ruby")
}

// ---- wire protocol -------------------------------------------------------------

fn send_cmd(stream: &mut UnixStream, op: &str, fields: &[String]) -> io::Result<()> {
    write!(stream, "{op} {}\r\n", fields.len())?;
    for f in fields {
        write!(stream, "${}\r\n", f.len())?;
        stream.write_all(f.as_bytes())?;
    }
    Ok(())
}

fn build_cmd(op: &str, fields: &[String]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(format!("{op} {}\r\n", fields.len()).as_bytes());
    for f in fields {
        buf.extend_from_slice(format!("${}\r\n", f.len()).as_bytes());
        buf.extend_from_slice(f.as_bytes());
    }
    buf
}

fn read_line(stream: &mut UnixStream) -> io::Result<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        stream.read_exact(&mut byte)?;
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n") {
            break;
        }
        if buf.len() > 4096 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "response too long"));
        }
    }
    buf.truncate(buf.len() - 2);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn daemon_connect_at(dir: &Path) -> Option<UnixStream> {
    UnixStream::connect(dir.join("calisto.sock")).ok()
}

fn connect_or_spawn_daemon_in(
    ruby: &Path,
    dir: &Path,
    preload: &str,
    setup: impl FnOnce(&mut Command),
) -> Result<UnixStream, String> {
    let sock = dir.join("calisto.sock");
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let rb = dir.join("calisto.rb");
    fs::write(&rb, DAEMON_RB).map_err(|e| format!("cannot write daemon script: {e}"))?;
    let mut cmd = Command::new(ruby);
    cmd.arg(&rb)
        .env("CALISTO_SOCKET", &sock)
        .env("CALISTO_PIDFILE", dir.join("calisto.pid"))
        .env("CALISTO_PRELOAD", preload)
        .stdin(Stdio::null());
    setup(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start daemon with {}: {e}", rb.display()))?;
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

fn connect_or_spawn_daemon(ruby: &Path, preload: &str) -> Result<UnixStream, String> {
    connect_or_spawn_daemon_in(ruby, &runtime_dir(), preload, |_| {})
}

fn connect_or_spawn_app_daemon(ruby: &Path, app: &AppConfig) -> Result<UnixStream, String> {
    connect_or_spawn_app_daemon_in(ruby, app, &app_runtime_dir(app), &[])
}

/// Daemon da app em modo teste (RAILS_ENV=test no boot, socket proprio).
fn connect_or_spawn_test_daemon(ruby: &Path, app: &AppConfig) -> Result<UnixStream, String> {
    connect_or_spawn_app_daemon_in(ruby, app, &app_test_runtime_dir(app), &[
        ("RAILS_ENV", "test"),
        ("RACK_ENV", "test"),
    ])
}

fn connect_or_spawn_app_daemon_in(
    ruby: &Path,
    app: &AppConfig,
    dir: &Path,
    envs: &[(&str, &str)],
) -> Result<UnixStream, String> {
    let preload = app.preload.display().to_string();
    // Daemon da app: boota com o Gemfile ativo (-rbundler/setup) e cwd na raiz
    // da app, e o entrypoint e carregado no boot (CALISTO_APP_PRELOAD).
    connect_or_spawn_daemon_in(ruby, dir, "", |cmd| {
        cmd.arg("-rbundler/setup")
            .current_dir(&app.root)
            .env("CALISTO_APP_PRELOAD", &preload);
        for (k, v) in envs {
            cmd.env(k, v);
        }
    })
}

// ---- commands -----------------------------------------------------------------

fn cmd_run(args: &[String]) -> i32 {
    let mut cold = false;
    let mut show_time = false;
    let mut preload_opt: Option<String> = None;
    let mut i = 0;
    while i < args.len() && args[i].starts_with("--") {
        match args[i].as_str() {
            "--cold" => cold = true,
            "--time" => show_time = true,
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
    let Some(script) = rest.first() else {
        eprintln!("calisto: run needs a script: calisto run [flags] script.rb [args...]");
        return 1;
    };
    let script_args = &rest[1..];

    load_dotenv(); // .env do cwd (walk up) entra no env do run/cold/daemon

    if !Path::new(script).is_file() {
        eprintln!("calisto: cannot open {script}: no such file");
        return 1;
    }

    check_ruby_version();

    let app = match load_app_config() {
        Ok(app) => app,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };

    let ruby = ruby_path();
    let preload = match &preload_opt {
        Some(v) => normalize_preload(v),
        None => run_preload(&app),
    };

    let t0 = Instant::now();
    let code = if cold {
        run_cold(&ruby, script, script_args)
    } else if let Some(app) = &app {
        run_fast_app(&ruby, script, script_args, app)
    } else {
        run_fast(&ruby, script, script_args, &preload)
    };
    if show_time {
        eprintln!("calisto: elapsed: {:?}", t0.elapsed());
    }
    code
}

fn normalize_preload(v: &str) -> String {
    if v == "0" || v == "none" {
        String::new()
    } else {
        v.to_string()
    }
}

/// Sobe do cwd ate a raiz procurando um arquivo (mesma busca do Bundler).
fn find_in_parents(name: &str) -> Option<PathBuf> {
    let cwd = env::current_dir().ok()?;
    let mut dir: Option<&Path> = Some(cwd.as_path());
    while let Some(d) = dir {
        let cand = d.join(name);
        if cand.is_file() {
            return Some(cand);
        }
        dir = d.parent();
    }
    None
}

/// .env (Fase E): carrega o primeiro `.env` subindo do cwd, sem sobrescrever
/// vars ja definidas (semantica dotenv). Roda no CLIENTE: o env resultante
/// propaga para o spawn do daemon (o boot da app ve DATABASE_URL etc.), para
/// o env_blob do RUN (o script ve) e para o modo --cold (paridade cold/warm
/// preservada — um parser so no daemon divergiria o cold).
fn load_dotenv() {
    let Some(file) = find_in_parents(".env") else { return };
    let Ok(content) = fs::read_to_string(&file) else { return };
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim();
        if k.is_empty() || env::var_os(k).is_some() {
            continue;
        }
        let mut v = v.trim().to_string();
        if v.len() >= 2 {
            let q = v.as_bytes()[0] as char;
            if (q == '"' || q == '\'') && v.ends_with(q) {
                v = v[1..v.len() - 1].to_string();
            }
        }
        env::set_var(k, v);
    }
}

/// Um Gemfile no cwd (ou BUNDLE_GEMFILE) significa que o Bundler.setup do
/// child vai ativar as gems do app — preload de stdlib nao pode coexistir:
/// se o Gemfile pina uma default gem ja preloaded (ex.: base64 0.2 vs 0.3 do
/// Sinatra 4), o Bundler aborta com "already activated". Com Gemfile, o
/// preload fica vazio e o bundler ativa o necessario (interpretador "fresco",
/// como o `bundle exec`).
fn has_gemfile() -> bool {
    if env::var_os("BUNDLE_GEMFILE").is_some() {
        return true;
    }
    find_in_parents("Gemfile").is_some() || find_in_parents("gems.rb").is_some()
}

/// Warn (sem abortar) se um `.ruby-version` encontrado subindo do cwd divergir
/// do pin unico do calisto. Mesma busca do Bundler/rbenv: cwd -> pais.
fn check_ruby_version() {
    let Some(file) = find_in_parents(".ruby-version") else { return };
    if let Ok(content) = fs::read_to_string(file) {
        let want = content.lines().next().unwrap_or("").trim();
        let want = want.strip_prefix("ruby-").unwrap_or(want).trim();
        if !want.is_empty() && want != PINNED_RUBY {
            eprintln!(
                "calisto: warning: .ruby-version pede {want}, mas o calisto usa o pin unico {PINNED_RUBY}"
            );
        }
    }
}

// ---- Fase B: preload de app (calisto.toml) --------------------------------

/// Configuracao por app via `calisto.toml`. Subset minimo de TOML:
/// comentarios `#`, secao `[run]` e `chave = "valor"` (sem escapes).
#[derive(Debug, Clone)]
struct AppConfig {
    /// dir do calisto.toml (raiz da app; o daemon roda com este cwd)
    root: PathBuf,
    /// entrypoint a pre-carregar no daemon (ex.: config/environment.rb)
    preload: PathBuf,
}

fn parse_calisto_toml(content: &str, base: &Path) -> Result<AppConfig, String> {
    let mut preload: Option<PathBuf> = None;
    for (i, raw) in content.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let sec = line.trim_start_matches('[').trim_end_matches(']').trim();
            if sec != "run" {
                return Err(format!("calisto.toml:{}: secao desconhecida '{sec}'", i + 1));
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("calisto.toml:{}: linha invalida: {raw}", i + 1));
        };
        if k.trim() != "preload" {
            return Err(format!("calisto.toml:{}: chave desconhecida '{}'", i + 1, k.trim()));
        }
        let v = v.trim();
        let Some(v) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
            return Err(format!("calisto.toml:{}: preload precisa ser \"caminho\"", i + 1));
        };
        if v.is_empty() {
            return Err(format!("calisto.toml:{}: preload vazio", i + 1));
        }
        preload = Some(base.join(v));
    }
    let Some(preload) = preload else {
        return Err("calisto.toml: falta [run] preload = \"entrypoint\"".into());
    };
    if !preload.is_file() {
        return Err(format!(
            "calisto.toml: preload '{}' nao existe",
            preload.display()
        ));
    }
    Ok(AppConfig { root: base.to_path_buf(), preload })
}

/// Detecta app do cwd (walk up, como Gemfile). Erro de parse e estrito no
/// `run`; status/stop/doctor tratam como sem-app com warning.
fn load_app_config() -> Result<Option<AppConfig>, String> {
    let Some(file) = find_in_parents("calisto.toml") else {
        return Ok(None);
    };
    let content = fs::read_to_string(&file)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let app = parse_calisto_toml(&content, file.parent().unwrap_or(Path::new(".")))?;
    Ok(Some(app))
}

/// FNV-1a 64 — hash estavel sem dep para isolar o daemon de cada app.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Daemon dedicado por app (como Spring/Zeus): o preload de app vive no boot
/// do daemon, entao ele so serve aquela app. O hash inclui o entrypoint para
/// mudancas de calisto.toml ganharem socket novo, e um sal para separar
/// ambientes (ex.: dev vs teste — o daemon de teste boota com RAILS_ENV=test).
fn app_runtime_dir_for(app: &AppConfig, salt: &str) -> PathBuf {
    let key = format!("{}\0{}\0{}", app.root.display(), app.preload.display(), salt);
    runtime_dir().join("apps").join(format!("{:016x}", fnv1a(&key)))
}

fn app_runtime_dir(app: &AppConfig) -> PathBuf {
    app_runtime_dir_for(app, "")
}

/// Daemon de teste: igual ao da app, mas boota com RAILS_ENV=test/RACK_ENV=test
/// (o Rails fixa o env no boot; um fork do boot dev nunca enxergaria :test).
fn app_test_runtime_dir(app: &AppConfig) -> PathBuf {
    app_runtime_dir_for(app, "test")
}

fn run_cold(ruby: &Path, script: &str, args: &[String]) -> i32 {
    // -rbundler/setup: ativa o Gemfile do cwd (como `bundle exec ruby`);
    // no-op fora de bundle, mantendo a paridade com o daemon warm.
    match Command::new(ruby)
        .arg("-rbundler/setup")
        .arg(script)
        .args(args)
        .status() {
        Ok(st) => exit_code(st),
        Err(e) => {
            eprintln!("calisto: cannot execute {}: {e}", ruby.display());
            1
        }
    }
}

fn run_fast(ruby: &Path, script: &str, args: &[String], preload: &str) -> i32 {
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
fn run_fast_app(ruby: &Path, script: &str, args: &[String], app: &AppConfig) -> i32 {
    let mut stream = match connect_or_spawn_app_daemon(ruby, app) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    run_request(&mut stream, script, args)
}

fn run_request(stream: &mut UnixStream, script: &str, args: &[String]) -> i32 {
    let cwd = env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    run_request_full(stream, &cwd, &[], script, args)
}

/// Variante com cwd e env extras explicitos (usada por `calisto test`: cwd na
/// raiz do projeto, RAILS_ENV=test e CALISTO_LOAD_PATH injetados no child).
fn run_request_full(
    stream: &mut UnixStream,
    cwd: &str,
    extra: &[(&str, &str)],
    script: &str,
    args: &[String],
) -> i32 {
    let mut env_pairs: Vec<String> = env::vars()
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "CALISTO_RUNTIME_DIR" | "CALISTO_SOCKET" | "CALISTO_PIDFILE" | "CALISTO_PRELOAD" | "CALISTO_RUBY"
            )
        })
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    for (k, v) in extra {
        // extra SEMPRE vence (incluindo .env): `calisto test` precisa de
        // RAILS_ENV=test mesmo se o .env do projeto setar development
        env_pairs.push(format!("{k}={v}"));
    }
    let env_blob = env_pairs.join("\u{1e}");
    let mut fields = vec![b64(cwd), b64(&env_blob), b64(script)];
    fields.extend(args.iter().map(|a| b64(a)));
    let bytes = build_cmd("RUN", &fields);
    if let Err(e) = send_with_fds(&mut *stream, &bytes, &[0, 1, 2]) {
        eprintln!("calisto: cannot talk to daemon: {e} (run 'calisto status')");
        return 1;
    }
    match read_line(stream) {
        Ok(line) => {
            if let Some(code) = line.strip_prefix("STATUS ") {
                code.trim().parse().unwrap_or(1)
            } else if let Some(msg) = line.strip_prefix("ERR ") {
                eprintln!("calisto: daemon error: {msg}");
                1
            } else {
                eprintln!("calisto: unexpected daemon response: {line}");
                1
            }
        }
        Err(e) => {
            eprintln!("calisto: daemon closed connection: {e}");
            1
        }
    }
}

fn exit_code(st: ExitStatus) -> i32 {
    st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0))
}

// ---- Fase E: calisto test ----------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum TestFramework {
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
fn walk_files(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) {
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
fn detect_framework(root: &Path) -> TestFramework {
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

fn test_file_snapshot(files: &[PathBuf]) -> Vec<(u64, u64)> {
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
fn cmd_test(args: &[String]) -> i32 {
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

    let ruby = ruby_path();
    let extra = [
        ("RAILS_ENV", "test"),
        ("RACK_ENV", "test"),
        ("CALISTO_LOAD_PATH", framework.dir()),
    ];

    let dir = match &app {
        Some(a) => {
            let dir = app_test_runtime_dir(a);
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
            runtime_dir()
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

// ---- Fase E: calisto task ----------------------------------------------------

/// Preload padrao para daemons nao-app: vazio com Gemfile (bundler ativa as
/// gems; preload colidiria), senao CALISTO_PRELOAD ou o default.
fn run_preload(app: &Option<AppConfig>) -> String {
    match app {
        Some(_) => String::new(), // app: o entrypoint e o preload (Fase B)
        None => {
            if has_gemfile() {
                String::new()
            } else {
                env::var("CALISTO_PRELOAD")
                    .map_or_else(|_| DEFAULT_PRELOAD.to_string(), |v| normalize_preload(&v))
            }
        }
    }
}

const RAKE_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto: equivale ao bin/rake do Rails (`load Gem.bin_path`),
# sem depender de binstub existir. O child roda com o Gemfile ativo.
begin
  load Gem.bin_path(\"rake\", \"rake\")
rescue Gem::GemNotFoundException => e
  warn \"calisto task: rake nao encontrado no bundle: #{e.message}\"
  exit 1
end
";

/// `calisto task <args...>` — rake no daemon quente (ex.: `calisto task
/// db:migrate`). Mesma semantica de `calisto run bin/rake <args>` no daemon
/// da app (dev); sem calisto.toml usa o daemon generico.
fn cmd_task(args: &[String]) -> i32 {
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

    let ruby = ruby_path();
    let dir = match &app {
        Some(a) => app_runtime_dir(a),
        None => runtime_dir(),
    };
    fs::create_dir_all(&dir).ok();
    let shim = dir.join("rake.rb");
    if !shim.is_file() {
        if let Err(e) = fs::write(&shim, RAKE_SHIM) {
            eprintln!("calisto: cannot write rake shim: {e}");
            return 1;
        }
    }

    let mut stream = match &app {
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
    run_request_full(&mut stream, &root.to_string_lossy(), &[], &shim.to_string_lossy(), args)
}

// ---- Fase E: calisto serve ---------------------------------------------------

const SERVE_LAUNCHER: &str = "# frozen_string_literal: true
# Gerado pelo calisto: serve o config.ru do cwd (Rack app) no daemon quente,
# como child do fork — o boot da app ja foi pago no daemon.
require \"rack\"
begin
  require \"rackup\"
rescue LoadError
  # rack 2: Rack::Server continua no proprio rack
end
port = Integer(ENV.fetch(\"CALISTO_SERVE_PORT\", \"3000\"))
host = ENV.fetch(\"CALISTO_SERVE_HOST\", \"127.0.0.1\")
config = File.join(Dir.pwd, \"config.ru\")
abort \"calisto serve: #{config} nao existe (rode na raiz do projeto)\" unless File.file?(config)
app, = Rack::Builder.parse_file(config)
if defined?(Rackup::Server)
  Rackup::Server.start(app: app, Host: host, Port: port,
                       environment: ENV.fetch(\"RACK_ENV\", \"development\"))
elsif Rack::Server.respond_to?(:start)
  Rack::Server.start(app: app, Host: host, Port: port)
else
  abort \"calisto serve: precisa de rackup (rack 3) ou rack 2 no Gemfile\"
end
";

/// `calisto serve [-p PORT] [-o HOST]` — sobe a Rack app do config.ru como
/// child do fork do daemon quente (rackup/rack com puma/webrick do bundle).
/// Fica em foreground; Ctrl-C/kill no cliente derruba o server via
/// client-death kill do daemon.
fn cmd_serve(args: &[String]) -> i32 {
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

    let ruby = ruby_path();
    let dir = match &app {
        Some(a) => app_runtime_dir(a),
        None => runtime_dir(),
    };
    fs::create_dir_all(&dir).ok();
    let launcher = dir.join("serve.rb");
    if !launcher.is_file() {
        if let Err(e) = fs::write(&launcher, SERVE_LAUNCHER) {
            eprintln!("calisto: cannot write serve launcher: {e}");
            return 1;
        }
    }

    let mut stream = match &app {
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

fn cmd_build(args: &[String]) -> i32 {
    let mut out = PathBuf::from("bundle.rb");
    let mut root: Option<PathBuf> = None;
    let mut entry: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                match args.get(i) {
                    Some(v) => out = PathBuf::from(v),
                    None => {
                        eprintln!("calisto: -o precisa de um valor");
                        return 1;
                    }
                }
            }
            "--root" => {
                i += 1;
                match args.get(i) {
                    Some(v) => root = Some(PathBuf::from(v)),
                    None => {
                        eprintln!("calisto: --root precisa de um valor");
                        return 1;
                    }
                }
            }
            s if s.starts_with('-') => {
                eprintln!("calisto: flag desconhecida '{s}'");
                return 1;
            }
            s => {
                if entry.is_none() {
                    entry = Some(PathBuf::from(s));
                } else {
                    eprintln!("calisto: argumento inesperado '{s}'");
                    return 1;
                }
            }
        }
        i += 1;
    }
    let Some(entry) = entry else {
        eprintln!("calisto: build precisa de um entrypoint: calisto build app.rb [-o out.rb]");
        return 1;
    };
    if !entry.is_file() {
        eprintln!("calisto: cannot open {}: no such file", entry.display());
        return 1;
    }
    let root = root.unwrap_or_else(|| {
        entry
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    });
    let ruby = ruby_path();
    match calisto_build::bundle(&ruby, &entry, &out, &root) {
        Ok(stats) => {
            println!("calisto: bundled {} arquivo(s) -> {}", stats.files, out.display());
            0
        }
        Err(e) => {
            eprintln!("calisto build: {e}");
            1
        }
    }
}

/// Dir de runtime de status/stop/doctor: o daemon da app quando o cwd esta
/// numa app com calisto.toml; senao o generico. Parse quebrado -> warning.
fn current_runtime_dir() -> PathBuf {
    match load_app_config() {
        Ok(Some(app)) => app_runtime_dir(&app),
        Ok(None) => runtime_dir(),
        Err(e) => {
            eprintln!("calisto: warning: {e}");
            runtime_dir()
        }
    }
}

fn cmd_status() -> i32 {
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
        let tdir = app_test_runtime_dir(&app);
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
    0
}

fn stop_daemon_at(dir: &Path) {
    if let Some(mut s) = daemon_connect_at(dir) {
        if send_cmd(&mut s, "STOP", &[]).is_ok() {
            let _ = read_line(&mut s);
        }
    }
}

fn cmd_stop() -> i32 {
    let dir = current_runtime_dir();
    let had = daemon_connect_at(&dir).is_some();
    stop_daemon_at(&dir);
    if let Ok(Some(app)) = load_app_config() {
        stop_daemon_at(&app_test_runtime_dir(&app)); // test daemon tambem
    }
    println!("daemon: {}", if had { "stopped" } else { "not running" });
    0
}

fn cmd_doctor() -> i32 {
    let ruby = ruby_path();
    println!("calisto doctor");
    println!("  pinned ruby: {}", ruby.display());
    let _ = Command::new(&ruby).arg("-v").status();
    match load_app_config() {
        Ok(Some(app)) => println!("  app preload: {}", app.preload.display()),
        _ => println!(
            "  preload: {}",
            env::var("CALISTO_PRELOAD").unwrap_or_else(|_| DEFAULT_PRELOAD.to_string())
        ),
    }
    cmd_status();
    0
}

fn print_help() {
    println!(
        "calisto - a Bun-like runtime for Ruby (pinned CRuby + fork-based fast startup)

USAGE:
  calisto run [--cold] [--time] [--preload LIST] <script.rb> [args...]
  calisto test [--watch] [file|dir...]
  calisto build <app.rb> [-o out.rb] [--root DIR]
  calisto status | stop | doctor | help

  run     executes <script.rb> on the pinned CRuby. Default: warm daemon that
          forks a child per run (fast startup). --cold spawns the interpreter
          directly for baseline comparison.
          --preload LIST overrides the stdlib the daemon preloads (\"0\" disables;
          default: {DEFAULT_PRELOAD}).
          A Gemfile do diretorio atual (buscando para cima) e ativada como em
          `bundle exec ruby`; instale as gems com `bundle install` normal.
          Com um calisto.toml no diretorio atual (walk up) o daemon vira o
          daemon da app (socket dedicado) e pre-carrega o entrypoint de
          [run].preload no boot — boot congelado, cada comando roda como fork.
  test    roda a suite de testes no daemon quente: detecta minitest
          (test/**/*_test.rb) ou rspec (spec/**/*_spec.rb, via .rspec) e roda
          cada arquivo como um fork — o boot da app (calisto.toml) e pago UMA
          vez num daemon de teste dedicado (RAILS_ENV=test, socket proprio);
          arquivos rodam em paralelo. --watch re-roda ao salvar. Exit != 0 se
          algum arquivo falhar. Args sao filtros (arquivos ou diretorios).
  task    roda rake no daemon quente: `calisto task db:migrate` == `calisto run
          bin/rake db:migrate` (equivalente ao bin/rake do Rails, sem exigir
          binstub). Usa o daemon da app (dev) quando ha calisto.toml.
  serve   sobe a Rack app do config.ru como child do fork do daemon quente
          (rackup/rack do bundle; ex.: `calisto serve -p 4567`). Fica em
          foreground; kill no cliente derruba o server.
  build   empacota <app.rb> e seus requires (arquivos do projeto, stdlib-only)
          num arquivo unico self-contained. Arquivos fora da raiz (stdlib)
          nao sao embutidos. --root define a raiz do projeto (default: o
          diretorio do entrypoint).
  status  shows whether the warm daemon is running
  stop    stops the warm daemon
  doctor  prints environment, pinned ruby version and daemon state

CONFIG:
  CALISTO_RUBY        path to a ruby binary (default: vendor/current/bin/ruby)
  CALISTO_PRELOAD     comma-separated stdlib preload list
  CALISTO_RUNTIME_DIR daemon socket/pid location (default: $XDG_RUNTIME_DIR/calisto)

NOTE: calisto run is equivalent to `bundle exec ruby <script>` with no VM flags
(-e/-E/...); fora de Gemfile, identico a `ruby <script>`.
Linux only (fork)."
    );
}
