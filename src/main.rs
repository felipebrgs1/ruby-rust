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
        Some("exec") => cmd_exec(&argv[1..]),
        Some("repl") => cmd_repl(&argv[1..]),
        Some("build") => cmd_build(&argv[1..]),
        Some("init") => cmd_init(&argv[1..]),
        Some("upgrade") => cmd_upgrade(&argv[1..]),
        Some("completions") => cmd_completions(&argv[1..]),
        Some("add") => cmd_bundle_wrapper("add", &argv[1..]),
        Some("remove") => cmd_bundle_wrapper("remove", &argv[1..]),
        Some("lock") => cmd_bundle_wrapper("lock", &argv[1..]),
        // interno (Fase L): spawnado pelo proprio cliente quando o ruby
        // resolvido tem libruby.so — roda o daemon com a VM embutida
        Some("daemon") => cmd_daemon(&argv[1..]),
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

/// Dir que contem `vendor/` (subindo do proprio executavel, mesma busca do
/// ruby_path antigo) — a base dos rubies empacotados.
fn vendor_root() -> Option<PathBuf> {
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

fn vendor_ruby(vendor: &Option<PathBuf>, version: &str) -> Option<PathBuf> {
    let cand = vendor.as_ref()?.join(format!("ruby-{version}/bin/ruby"));
    cand.is_file().then_some(cand)
}

/// Versao pedida pela primeira linha do `.ruby-version` (rbenv): prefixo
/// `ruby-` tolerado, linhas vazias ignoradas.
fn ruby_version_from_file(file: &Path) -> Option<String> {
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
fn ruby_version_from_gemfile(file: &Path) -> Option<String> {
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
fn resolve_ruby() -> Result<PathBuf, String> {
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
fn ruby_or_err() -> Option<PathBuf> {
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
fn ruby_version_of(path: &Path) -> Option<String> {
    let name = path.parent()?.parent()?.file_name()?.to_str()?;
    let v = name.strip_prefix("ruby-")?;
    (v != PINNED_RUBY).then(|| v.to_string())
}

/// Runtime dir do daemon generico: por versao quando o ruby resolvido nao e o
/// pin default — daemons de VMs diferentes nao podem dividir o socket.
fn daemon_dir_for(ruby: &Path) -> PathBuf {
    match ruby_version_of(ruby) {
        Some(v) => runtime_dir().join(format!("ruby-{v}")),
        None => runtime_dir(),
    }
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
    flags: &[&str],
    setup: impl FnOnce(&mut Command),
) -> Result<UnixStream, String> {
    let sock = dir.join("calisto.sock");
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let rb = dir.join("calisto.rb");
    fs::write(&rb, DAEMON_RB).map_err(|e| format!("cannot write daemon script: {e}"))?;
    // Fase L: com libruby.so disponivel o daemon roda EMBUTIDO — o proprio
    // binario calisto vira o processo do daemon (VM in-process via dlopen),
    // sem spawnar o interpretador externo. CALISTO_NO_EMBED=1 força o modo
    // legado (spawn `ruby <daemon.rb>`) — ex.: rubies antigos sem .so, ou
    // debug. flags sao repassadas ao ruby_options em ambos os modos
    // (`-rbundler/setup` antes do script continua sendo flag de verdade).
    let embedded = env::var_os("CALISTO_NO_EMBED").is_none() && calisto_ruby::libruby_path(ruby).is_some();
    let mut cmd = if embedded {
        let exe = std::env::current_exe()
            .map_err(|e| format!("cannot resolve own executable: {e}"))?;
        let mut c = Command::new(exe);
        c.arg("daemon")
            .arg("--internal")
            .args(flags)
            .arg(&rb)
            .env("CALISTO_EMBED_RUBY", ruby);
        c
    } else {
        let mut c = Command::new(ruby);
        // flags ANTES do script: `ruby -r... <daemon.rb>` — depois do script seria
        // ARGV do daemon, nao flag do interpretador
        c.args(flags).arg(&rb);
        c
    };
    cmd.env("CALISTO_SOCKET", &sock)
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
    connect_or_spawn_daemon_in(ruby, &daemon_dir_for(ruby), preload, &[], |_| {})
}

fn connect_or_spawn_app_daemon(ruby: &Path, app: &AppConfig) -> Result<UnixStream, String> {
    connect_or_spawn_app_daemon_in(ruby, app, &app_runtime_dir(app, ruby), &[])
}

/// Daemon da app em modo teste (RAILS_ENV=test no boot, socket proprio).
fn connect_or_spawn_test_daemon(ruby: &Path, app: &AppConfig) -> Result<UnixStream, String> {
    connect_or_spawn_app_daemon_in(ruby, app, &app_test_runtime_dir(app, ruby), &[
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
    let preload = app.preload.as_ref().map(|p| p.display().to_string());
    // Daemon da app: boota com o Gemfile ativo (-rbundler/setup ANTES do
    // script — flag real) e cwd na raiz da app, e o entrypoint e carregado
    // no boot (CALISTO_APP_PRELOAD).
    connect_or_spawn_daemon_in(ruby, dir, "", &["-rbundler/setup"], |cmd| {
        cmd.current_dir(&app.root);
        if let Some(p) = &preload {
            cmd.env("CALISTO_APP_PRELOAD", p);
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
    })
}

/// Uso interno (Fase L): `calisto daemon --internal [flags...] <script>` —
/// roda o script com o CRuby EMBUTIDO (libruby dlopen'd pelo
/// calisto_ruby::Ruby::open), sem spawnar o interpretador externo.
/// Spawnado pelo proprio cliente quando o ruby resolvido tem libruby.so;
/// `CALISTO_EMBED_RUBY` aponta esse ruby (para achar a .so). O restante do
/// argv e repassado como argv do `ruby` (flags como `-rbundler/setup`
/// primeiro, script por ultimo) — ruby_options + ruby_run_node dao $0,
/// ARGV, at_exit e exit code iguais aos do modo legado.
fn cmd_daemon(args: &[String]) -> i32 {
    if args.first().map(String::as_str) != Some("--internal") {
        eprintln!("calisto daemon: uso interno: calisto daemon --internal [flags...] <script>");
        return 1;
    }
    let Some(ruby) = env::var_os("CALISTO_EMBED_RUBY") else {
        eprintln!("calisto daemon: CALISTO_EMBED_RUBY nao definido (uso interno)");
        return 1;
    };
    match calisto_ruby::Ruby::open(Path::new(&ruby)) {
        Err(e) => {
            eprintln!("calisto daemon: {e}");
            1
        }
        Ok(vm) => vm.run_script(&args[1..]),
    }
}

// ---- commands -----------------------------------------------------------------

fn cmd_run(args: &[String]) -> i32 {
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

// ---- Fase B: preload de app (calisto.toml) --------------------------------

/// Configuracao por app via `calisto.toml`. Subset minimo de TOML:
/// comentarios `#`, secoes `[run]`/`[scripts]` e `chave = "valor"` (sem
/// escapes). `preload` e opcional: um calisto.toml so com `[scripts]` (Fase H)
/// nao vira daemon da app — scripts rodam no daemon generico.
#[derive(Debug, Clone)]
struct AppConfig {
    /// dir do calisto.toml (raiz da app; o daemon roda com este cwd)
    root: PathBuf,
    /// entrypoint a pre-carregar no daemon (ex.: config/environment.rb)
    preload: Option<PathBuf>,
    /// `[scripts]` nome -> comando (ordem do arquivo; Fase H)
    scripts: Vec<(String, String)>,
}

impl AppConfig {
    /// argv do comando de `[scripts.NAME]` (tokenizado shell-like, sem
    /// escapes/expansao). Ok(None) = script nao definido; Err = comando
    /// invalido (vazio ou aspas desbalanceadas).
    fn script_command(&self, name: &str) -> Result<Option<Vec<String>>, String> {
        let Some((_, cmd)) = self.scripts.iter().find(|(n, _)| n == name) else {
            return Ok(None);
        };
        let argv = split_command(cmd)?;
        if argv.is_empty() {
            return Err(format!("calisto.toml: script '{name}' sem comando"));
        }
        Ok(Some(argv))
    }
}

/// Tokeniza o comando de um `[scripts]` (shell-like minimo): whitespace
/// separa palavras e aspas simples/duplas agrupam — sem escapes, sem
/// expansao de variaveis (subset do TOML que o parser ja usa).
fn split_command(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => quote = Some(c),
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            },
        }
    }
    if let Some(q) = quote {
        return Err(format!("calisto.toml: aspas '{q}' nao fechadas em \"{s}\""));
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

fn parse_calisto_toml(content: &str, base: &Path) -> Result<AppConfig, String> {
    let mut preload: Option<PathBuf> = None;
    let mut scripts: Vec<(String, String)> = Vec::new();
    let mut section = "run"; // chave sem secao = [run] (backwards compat)
    for (i, raw) in content.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let sec = line.trim_start_matches('[').trim_end_matches(']').trim();
            if sec != "run" && sec != "scripts" {
                return Err(format!("calisto.toml:{}: secao desconhecida '{sec}'", i + 1));
            }
            section = sec;
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("calisto.toml:{}: linha invalida: {raw}", i + 1));
        };
        let k = k.trim();
        let Some(value) = v.trim().strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
            return Err(format!("calisto.toml:{}: {k} precisa ser \"valor\"", i + 1));
        };
        match section {
            "run" => {
                if k != "preload" {
                    return Err(format!(
                        "calisto.toml:{}: chave desconhecida '{k}' (scripts vao na secao [scripts])",
                        i + 1
                    ));
                }
                if value.is_empty() {
                    return Err(format!("calisto.toml:{}: preload vazio", i + 1));
                }
                preload = Some(base.join(value));
            }
            _ => {
                // [scripts]: nome -> comando (validado na resolucao do run)
                scripts.push((k.to_string(), value.to_string()));
            }
        }
    }
    if let Some(p) = &preload {
        if !p.is_file() {
            return Err(format!(
                "calisto.toml: preload '{}' nao existe",
                p.display()
            ));
        }
    }
    Ok(AppConfig { root: base.to_path_buf(), preload, scripts })
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
fn app_runtime_dir_for(app: &AppConfig, salt: &str, ruby: &Path) -> PathBuf {
    let preload = app
        .preload
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let base = format!("{}\0{}\0{}", app.root.display(), preload, salt);
    // versao no hash: daemons da app de VMs diferentes convivem. O pin default
    // mantem o hash classico (sem quebrar sockets existentes).
    let key = match ruby_version_of(ruby) {
        Some(v) => format!("{base}\0{v}"),
        None => base,
    };
    runtime_dir().join("apps").join(format!("{:016x}", fnv1a(&key)))
}

fn app_runtime_dir(app: &AppConfig, ruby: &Path) -> PathBuf {
    app_runtime_dir_for(app, "", ruby)
}

/// Daemon de teste: igual ao da app, mas boota com RAILS_ENV=test/RACK_ENV=test
/// (o Rails fixa o env no boot; um fork do boot dev nunca enxergaria :test).
fn app_test_runtime_dir(app: &AppConfig, ruby: &Path) -> PathBuf {
    app_runtime_dir_for(app, "test", ruby)
}

/// App com daemon dedicado: so quando ha entrypoint para pre-carregar (Fase
/// B). Um calisto.toml so com `[scripts]` (Fase H) nao justifica daemon da
/// app — os comandos rodam no daemon generico (preload default).
fn app_daemon(app: &Option<AppConfig>) -> Option<&AppConfig> {
    app.as_ref().filter(|a| a.preload.is_some())
}

fn run_cold(ruby: &Path, script: &str, args: &[String], cwd: &Path) -> i32 {
    // -rbundler/setup: ativa o Gemfile do cwd (como `bundle exec ruby`);
    // no-op fora de bundle, mantendo a paridade com o daemon warm.
    match Command::new(ruby)
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
fn run_cold_eval(ruby: &Path, code: &str, args: &[String]) -> i32 {
    match Command::new(ruby)
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

fn run_fast_eval(ruby: &Path, code: &str, args: &[String], preload: &str) -> i32 {
    let mut stream = match connect_or_spawn_daemon(ruby, preload) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    eval_request(&mut stream, code, args)
}

fn eval_request(stream: &mut UnixStream, code: &str, args: &[String]) -> i32 {
    let cwd = env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    eval_request_full(stream, &cwd, &[], code, args)
}

/// `-e` no daemon da app (boot congelado, como o run normal).
fn run_fast_app_eval(ruby: &Path, code: &str, args: &[String], app: &AppConfig) -> i32 {
    let mut stream = match connect_or_spawn_app_daemon(ruby, app) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("calisto: {e}");
            return 1;
        }
    };
    eval_request(&mut stream, code, args)
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
    send_run_request(stream, "RUN", cwd, extra, script, args)
}

/// EVAL: como `run_request_full`, mas o subject e codigo inline — o daemon
/// evala com semantica de `ruby -e` ($0 = "-e", backtrace "-e:1", sem DATA).
fn eval_request_full(
    stream: &mut UnixStream,
    cwd: &str,
    extra: &[(&str, &str)],
    code: &str,
    args: &[String],
) -> i32 {
    send_run_request(stream, "EVAL", cwd, extra, code, args)
}

fn send_run_request(
    stream: &mut UnixStream,
    op: &str,
    cwd: &str,
    extra: &[(&str, &str)],
    subject: &str,
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
    let mut fields = vec![b64(cwd), b64(&env_blob), b64(subject)];
    fields.extend(args.iter().map(|a| b64(a)));
    let bytes = build_cmd(op, &fields);
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

// ---- Fase E: calisto task ----------------------------------------------------

/// Preload padrao para daemons nao-app: vazio com Gemfile (bundler ativa as
/// gems; preload colidiria), senao CALISTO_PRELOAD ou o default.
fn run_preload(app: &Option<AppConfig>) -> String {
    if app_daemon(app).is_some() {
        String::new() // app: o entrypoint e o preload (Fase B)
    } else if has_gemfile() {
        String::new()
    } else {
        env::var("CALISTO_PRELOAD")
            .map_or_else(|_| DEFAULT_PRELOAD.to_string(), |v| normalize_preload(&v))
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

    let Some(ruby) = ruby_or_err() else {
        return 1;
    };
    let dir = match app_daemon(&app) {
        Some(a) => app_runtime_dir(a, &ruby),
        None => daemon_dir_for(&ruby),
    };
    fs::create_dir_all(&dir).ok();
    let shim = dir.join("rake.rb");
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

// ---- Fase G: calisto exec ----------------------------------------------------

const EXEC_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto: `calisto exec <bin>` — resolve como `bundle exec` e roda
# o binario no daemon quente (child do fork, Gemfile ja ativo). Resolucao:
#   1. argumento que e um caminho de arquivo executavel (ex.: ./bin/rails)
#   2. spec do bundle ativo (Gem.loaded_specs) cujo executavel e o nome — nao
#      depende de binstub (`bundle binstubs` desnecessario)
#   3. PATH (binarios de sistema)
# Binario ruby (shebang ruby) e carregado in-process, como o kernel_load do
# bundler: $0 = caminho, ARGV = args, `load` — sem depender de shebang no PATH
# nem de re-exec. Binario nativo e `exec` direto.
name = ARGV.shift
abort \"calisto exec: uso: calisto exec <bin> [args...]\" if name.nil? || name.empty?

def calisto_exec_resolve(name)
  # 1. caminho de arquivo (como Bundler.which): relativo/absoluto
  if name.include?(File::SEPARATOR)
    path = File.expand_path(name)
    return path if File.file?(path) && File.executable?(path)
  end
  # 2. specs ativadas pelo bundle (Bundler.setup ja rodou no child do fork).
  #    Default gems aparecem 2x (default + instalada) — dedup por nome; so e
  #    ambiguidade de verdade quando gems DIFERENTES fornecem o executavel.
  specs = Gem.loaded_specs.values.select { |s| s.executables.include?(name) }
  specs = specs.uniq(&:name)
  if specs.size == 1
    return Gem.bin_path(specs.first.name, name)
  elsif specs.size >= 2
    return specs # ambiguidade real: 2+ gems diferentes fornecem o executavel
  end
  # 0 specs: cai no PATH (binarios de sistema, como `bundle exec` fora de bundle)
  # 3. PATH
  ENV.fetch(\"PATH\", \"\").split(File::PATH_SEPARATOR).each do |dir|
    next if dir.empty?
    path = File.join(dir, name)
    return path if File.file?(path) && File.executable?(path)
  end
  nil
end

def calisto_exec_ruby?(file)
  first = File.open(file, \"rb\") { |f| f.read(64).to_s }
  first.start_with?(\"#!/usr/bin/env ruby\", \"#!/usr/bin/env jruby\",
                   \"#!/usr/bin/env truffleruby\", \"#!#{Gem.ruby}\")
end

found = calisto_exec_resolve(name)
case found
when nil
  warn \"calisto exec: comando nao encontrado: #{name}\"
  warn \"calisto exec: instale a gem que o fornece no Gemfile (bundle add ...) ou use o PATH\"
  exit 127
when Array
  # divergencia do bundle exec (que pega o primeiro do PATH): ambiguidade e
  # erro claro com os candidatos, em vez de ordem arbitraria
  warn \"calisto exec: '#{name}' ambiguo: #{found.map(&:name).join(', ')}\"
  exit 1
end

args = ARGV
if calisto_exec_ruby?(found)
  # kernel_load do bundler: in-process, sem shebang/re-exec
  $0 = found
  ARGV.replace(args)
  load found
else
  begin
    exec found, *args
  rescue Errno::EACCES, Errno::ENOEXEC
    warn \"calisto exec: nao executavel: #{name} (#{found})\"
    exit 126
  rescue Errno::ENOENT
    warn \"calisto exec: comando nao encontrado: #{name}\"
    exit 127
  end
end
";

/// Roda um argv (bin + args) como `calisto exec` no daemon quente: o shim
/// `exec.rb` resolve o bin (caminho de arquivo -> spec do bundle ativo ->
/// PATH) e carrega in-process (kernel_load do bundler). Usado por `calisto
/// exec` e pelos scripts de `[scripts]` do calisto.toml (Fase H). `cold` roda
/// o shim no interpretador direto com cwd na raiz (paridade com --cold).
fn exec_argv(ruby: &Path, app: &Option<AppConfig>, cold: bool, argv: &[String]) -> i32 {
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
fn cmd_exec(args: &[String]) -> i32 {
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

// ---- Fase G: calisto repl ----------------------------------------------------

const REPL_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto: `calisto repl` — IRB no daemon quente, no contexto da
# app pre-carregada (fork do boot congelado). Args sao repassados ao IRB
# (IRB.setup faz parse de ARGV, como o binario `irb`).
require \"irb\"
IRB.start
";

/// `calisto repl [args...]` — IRB interativo como child do fork: no daemon da
/// app (calisto.toml) o REPL herda o boot pre-carregado (console de app); no
/// daemon generico, stdlib preloaded. Fica em foreground; Ctrl-C/kill no
/// cliente derruba o child via client-death kill (como `calisto serve`).
fn cmd_repl(args: &[String]) -> i32 {
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
    run_request_full(&mut stream, &root.to_string_lossy(), &[], &shim.to_string_lossy(), args)
}

// ---- Fase J: calisto init ----------------------------------------------------

const INIT_CALISTO_TOML: &str = "# calisto.toml — config da app calisto (subset minimo de TOML).
# Scripts rodam com `calisto run <nome>`; um arquivo com o mesmo nome sempre
# vence (ex.: `calisto run hello.rb` roda o arquivo, nao o script).
# Sem [run] preload o daemon e o generico (preload stdlib). Quando a app tiver
# um boot pesado (Rails etc.), adicione:
#   [run]
#   preload = \"config/environment.rb\"
# e o daemon da app congela o boot (cada comando roda como fork do boot).
[scripts]
start = \"./hello.rb\"
";

const INIT_HELLO_RB: &str = "#!/usr/bin/env ruby
# frozen_string_literal: true
# Ola do calisto! Edite este arquivo e rode `calisto run` (ou `calisto run
# hello.rb`) — o daemon quente roda no interpretador pre-carregado.
puts \"Hello from calisto!\"
";

const INIT_GITIGNORE: &str = "/.bundle/
/vendor/bundle/
";

/// `calisto init [name] [--force]` — scaffold de app (como `bun init`):
/// calisto.toml com `[scripts] start = \"./hello.rb\"`, hello.rb e .gitignore.
/// Sem nome usa o diretorio atual; `calisto init <name>` cria `<name>/`
/// (erro se for arquivo). Nunca sobrescreve arquivo existente sem --force.
/// O app gerado roda com `calisto run` (bare = o script `start`).
fn cmd_init(args: &[String]) -> i32 {
    let mut name: Option<&str> = None;
    let mut force = false;
    for a in args {
        match a.as_str() {
            "--force" | "-f" => force = true,
            s if s.starts_with('-') => {
                eprintln!("calisto: flag desconhecida '{s}' (calisto init [name] [--force])");
                return 1;
            }
            s => {
                if name.is_some() {
                    eprintln!("calisto: argumento inesperado '{s}'");
                    return 1;
                }
                name = Some(s);
            }
        }
    }
    let target = match name {
        Some(n) => {
            let p = PathBuf::from(n);
            if p.is_file() {
                eprintln!("calisto: init: '{n}' e um arquivo, nao um diretorio");
                return 1;
            }
            p
        }
        None => PathBuf::from("."),
    };
    if let Err(e) = fs::create_dir_all(&target) {
        eprintln!("calisto: init: cannot create {}: {e}", target.display());
        return 1;
    }
    // hello.rb precisa de shebang + +x: o shim do exec resolve executaveis
    // (caminho de arquivo -> spec do bundle -> PATH) e detecta binario ruby
    // pelo shebang para `load` in-process — o script do scaffold nao depende
    // de ruby no PATH.
    for (file, content, exec) in [
        ("calisto.toml", INIT_CALISTO_TOML, false),
        ("hello.rb", INIT_HELLO_RB, true),
        (".gitignore", INIT_GITIGNORE, false),
    ] {
        let path = target.join(file);
        if path.exists() && !force {
            eprintln!(
                "calisto: init: {} ja existe (use --force para sobrescrever)",
                path.display()
            );
            return 1;
        }
        if let Err(e) = fs::write(&path, content) {
            eprintln!("calisto: init: cannot write {}: {e}", path.display());
            return 1;
        }
        if exec {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
        }
    }
    let rel = if name.is_some() {
        format!("./{}", target.display())
    } else {
        ".".to_string()
    };
    println!("Initialized calisto app in {rel}");
    println!("next: cd {rel} && calisto run");
    0
}

// ---- Fase J: calisto upgrade -------------------------------------------------

/// Versoes com sha256 conhecido no scripts/build-ruby.sh (espelho do case do
/// script). O CLI valida ANTES de spawnar: uma versao desconhecida falharia
/// so dentro do script com "unbound variable" (set -u), sem sha pra verificar.
const KNOWN_RUBY_VERSIONS: &[&str] = &["3.4.10", "3.4.4"];

/// `calisto upgrade [version]` — roda scripts/build-ruby.sh no checkout:
/// sem versao rebuilda o pin (vendor/current); com versao constroi
/// vendor/ruby-<v> (Fase I — disponibiliza o ruby que o .ruby-version/Gemfile
/// pede). Idempotente: o script pula rubies ja construidos (verifica o sha do
/// tarball baixado antes). `CALISTO_BUILD_SCRIPT` (testes) aponta outro
/// script; o caminho real e resolvido junto ao vendor (subindo do binario).
fn cmd_upgrade(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("calisto: upgrade precisa de no maximo um argumento: calisto upgrade [version]");
        return 1;
    }
    if let Some(v) = args.first() {
        if !KNOWN_RUBY_VERSIONS.contains(&v.as_str()) {
            eprintln!(
                "calisto: upgrade: versao {v} sem sha256 conhecido ({}); \
                 rode RUBY_SHA256=<sha> scripts/build-ruby.sh manualmente",
                KNOWN_RUBY_VERSIONS.join("/")
            );
            return 1;
        }
    }
    let script = match env::var_os("CALISTO_BUILD_SCRIPT") {
        Some(p) => PathBuf::from(p),
        None => match vendor_root() {
            Some(vendor) => vendor.join("../scripts/build-ruby.sh"),
            None => {
                eprintln!("calisto: upgrade: vendor/ nao encontrado (rode do checkout do calisto)");
                return 1;
            }
        },
    };
    if !script.is_file() {
        eprintln!(
            "calisto: upgrade: {} nao encontrado (rode scripts/build-ruby.sh manualmente)",
            script.display()
        );
        return 1;
    }
    let mut cmd = Command::new("sh");
    cmd.arg(&script).stdin(Stdio::null());
    if let Some(v) = args.first() {
        cmd.env("RUBY_VERSION", v);
    }
    match cmd.status() {
        // stdio herdado: o build (10-15min) mostra o progresso ao vivo
        Ok(st) => st.code().unwrap_or(1),
        Err(e) => {
            eprintln!("calisto: upgrade: cannot run {}: {e}", script.display());
            1
        }
    }
}

// ---- Fase J: calisto completions ---------------------------------------------

const BASH_COMPLETION: &str = r#"# bash completion for calisto
# Instale com: calisto completions bash > /etc/bash_completion.d/calisto
_calisto() {
    local cur
    cur="${COMP_WORDS[COMP_CWORD]}"
    local commands="run test task serve exec repl build init upgrade completions add remove lock status stop doctor help"
    if (( COMP_CWORD == 1 )); then
        COMPREPLY=( $(compgen -W "${commands}" -- "${cur}") )
        return 0
    fi
    case "${COMP_WORDS[1]}" in
        run)
            if [[ "${cur}" == -* ]]; then
                COMPREPLY=( $(compgen -W "--cold --time --preload -e --eval" -- "${cur}") )
            else
                COMPREPLY=( $(compgen -f -X '!*.rb' -- "${cur}") )
            fi
            ;;
        test)
            if [[ "${cur}" == -* ]]; then
                COMPREPLY=( $(compgen -W "--watch" -- "${cur}") )
            else
                COMPREPLY=( $(compgen -f -- "${cur}") )
            fi
            ;;
        serve)
            [[ "${cur}" == -* ]] && COMPREPLY=( $(compgen -W "-p --port -o --host" -- "${cur}") )
            ;;
        build)
            if [[ "${cur}" == -* ]]; then
                COMPREPLY=( $(compgen -W "--compile -o --out --root" -- "${cur}") )
            else
                COMPREPLY=( $(compgen -f -X '!*.rb' -- "${cur}") )
            fi
            ;;
        init)
            if [[ "${cur}" == -* ]]; then
                COMPREPLY=( $(compgen -W "--force -f" -- "${cur}") )
            else
                COMPREPLY=( $(compgen -d -- "${cur}") )
            fi
            ;;
        upgrade)
            COMPREPLY=( $(compgen -W "3.4.10 3.4.4" -- "${cur}") )
            ;;
        completions)
            COMPREPLY=( $(compgen -W "bash zsh" -- "${cur}") )
            ;;
    esac
}
complete -F _calisto calisto
"#;

const ZSH_COMPLETION: &str = r#"#compdef calisto
# zsh completion for calisto
# Instale com: calisto completions zsh > ~/.zfunc/_calisto
_calisto() {
    local -a commands
    commands=(
        'run:executa script/script do calisto.toml no daemon quente'
        'test:roda a suite (minitest/rspec) no daemon quente'
        'task:rake no daemon quente'
        'serve:sobe a Rack app do config.ru'
        'exec:binario de uma gem no contexto da app'
        'repl:IRB no contexto da app'
        'build:empacota a app num arquivo unico'
        'init:scaffold de app (calisto.toml + hello.rb)'
        'upgrade:rebuild do pin / build de versao'
        'completions:gera completions (bash/zsh)'
        'add:adiciona gem ao Gemfile (bundle add)'
        'remove:remove gem do Gemfile (bundle remove)'
        'lock:atualiza o Gemfile.lock (bundle lock)'
        'status:estado do daemon'
        'stop:para o daemon'
        'doctor:diagnostico do ambiente'
        'help:ajuda'
    )
    if (( CURRENT == 2 )); then
        _describe 'command' commands
        return
    fi
    case "${words[2]}" in
        run)
            if [[ "$PREFIX" == -* ]]; then
                _arguments '--cold' '--time' '--preload[lista de stdlib]' '-e[codigo inline]' '--eval' '*:arquivo:_files -g "*.rb"'
            else
                _files -g '*.rb'
            fi
            ;;
        test) _arguments '--watch[re-roda ao salvar]' '*:arquivo:_files' ;;
        serve) _arguments '-p[porta]' '--port[porta]' '-o[host]' '--host[host]' ;;
        build) _arguments '--compile[embute gems pure-Ruby]' '-o[saida]' '--out[saida]' '--root[raiz do projeto]' '*:arquivo:_files -g "*.rb"' ;;
        init) _arguments '--force[sobrescreve arquivos]' '-f[sobrescreve arquivos]' '*:diretorio:_files -/' ;;
        upgrade) _values 'versao' 3.4.10 3.4.4 ;;
        completions) _values 'shell' bash zsh ;;
    esac
}
compdef _calisto calisto
"#;

/// `calisto completions <bash|zsh>` — imprime o script de completions do
/// shell em stdout (redirecione para o arquivo de completions). Sem shell ou
/// shell desconhecido: erro de uso.
fn cmd_completions(args: &[String]) -> i32 {
    match args {
        [shell] if shell == "bash" => {
            print!("{BASH_COMPLETION}");
            0
        }
        [shell] if shell == "zsh" => {
            print!("{ZSH_COMPLETION}");
            0
        }
        [other] => {
            eprintln!("calisto: completions: shell desconhecido '{other}' (bash|zsh)");
            1
        }
        _ => {
            eprintln!("calisto: uso: calisto completions <bash|zsh>");
            1
        }
    }
}

// ---- Fase K: deps (calisto add/remove/lock) ---------------------------------

/// `calisto add|remove|lock` — wrapper fino do bundle (decisao da Fase A:
/// nada de instalador proprio), com o ruby da versao certa (Fase I) e cwd na
/// raiz do projeto (walk-up do Gemfile, como o resto do calisto). Args passam
/// direto ao `bundle <sub>`. `CALISTO_BUNDLE` (testes) troca o binario do
/// bundle; o client exporta `CALISTO_BUNDLE_RUBY` (ruby resolvido) e prefixa
/// o PATH com o bin dir do ruby (trap do restart do bundler: lock que pina
/// outro bundler re-executa via shebang e precisa de ruby no PATH).
fn cmd_bundle_wrapper(sub: &str, args: &[String]) -> i32 {
    let gemfile = env::var_os("BUNDLE_GEMFILE")
        .map(PathBuf::from)
        .or_else(|| find_in_parents("Gemfile"));
    let Some(gemfile) = gemfile else {
        eprintln!(
            "calisto: {sub}: nenhum Gemfile encontrado (subindo do cwd); \
             crie um Gemfile ou rode `bundle init`"
        );
        return 1;
    };
    let Some(root) = gemfile.parent().map(Path::to_path_buf) else {
        eprintln!("calisto: {sub}: BUNDLE_GEMFILE invalido: {}", gemfile.display());
        return 1;
    };
    let Some(ruby) = ruby_or_err() else {
        return 1;
    };
    let bin = ruby.parent().unwrap_or(Path::new("."));
    let path = format!("{}:{}", bin.display(), env::var("PATH").unwrap_or_default());
    let mut cmd = match env::var_os("CALISTO_BUNDLE") {
        Some(b) => {
            let mut c = Command::new(b);
            c.arg(sub);
            c
        }
        None => {
            // `ruby -S bundle`: roda o bundler do MESMO ruby (versao da app)
            // sem depender de shebang; -S procura no PATH (prefixado acima).
            let mut c = Command::new(&ruby);
            c.arg("-S").arg("bundle").arg(sub);
            c
        }
    };
    cmd.args(args)
        .current_dir(&root)
        .env("BUNDLE_GEMFILE", &gemfile)
        .env("CALISTO_BUNDLE_RUBY", &ruby)
        .env("PATH", path)
        .stdin(Stdio::inherit());
    match cmd.status() {
        // stdio herdado: o bundle mostra o progresso ao vivo
        Ok(st) => st.code().unwrap_or(1),
        Err(e) => {
            eprintln!("calisto: {sub}: cannot run bundle: {e}");
            1
        }
    }
}

fn cmd_build(args: &[String]) -> i32 {
    let mut out = PathBuf::from("bundle.rb");
    let mut root: Option<PathBuf> = None;
    let mut entry: Option<PathBuf> = None;
    let mut compile = false;
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
            "--compile" => compile = true,
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
    let Some(ruby) = ruby_or_err() else {
        return 1;
    };
    match calisto_build::bundle(&ruby, &entry, &out, &root, compile) {
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
/// Dir de runtime de status/stop/doctor: o daemon da app quando o cwd esta
/// numa app com calisto.toml; senao o generico (por versao, Fase I). Parse
/// quebrado ou versao ausente -> warning com o dir generico.
fn current_runtime_dir() -> PathBuf {
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
        if let Ok(ruby) = resolve_ruby() {
            stop_daemon_at(&app_test_runtime_dir(&app, &ruby)); // test daemon tambem
        }
    }
    println!("daemon: {}", if had { "stopped" } else { "not running" });
    0
}

fn cmd_doctor() -> i32 {
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
    0
}

fn print_help() {
    println!(
        "calisto - a Bun-like runtime for Ruby (pinned CRuby + fork-based fast startup)

USAGE:
  calisto run [--cold] [--time] [--preload LIST] [-e 'code' | <script.rb> | <script>] [args...]
  calisto test [--watch] [file|dir...]
  calisto task <args...>
  calisto serve [-p PORT] [-o HOST]
  calisto exec <bin> [args...]
  calisto repl [args...]
  calisto build <app.rb> [-o out.rb] [--root DIR]
  calisto init [name] [--force]
  calisto upgrade [version]
  calisto completions <bash|zsh>
  calisto add <gem...> | remove <gem...> | lock
  calisto status | stop | doctor | help

  run     executes <script.rb> on the pinned CRuby. Default: warm daemon that
          forks a child per run (fast startup). --cold spawns the interpreter
          directly for baseline comparison.
          -e 'code' (ou --eval) roda codigo inline com semantica de `ruby -e`
          ($0 = \"-e\", backtrace \"-e:1\", ARGV = args restantes; multiplos -e
          sao concatenados com newline, como o ruby). Tambem aceita --cold.
          --preload LIST overrides the stdlib the daemon preloads (\"0\" disables;
          default: {DEFAULT_PRELOAD}).
          A Gemfile do diretorio atual (buscando para cima) e ativada como em
          `bundle exec ruby`; instale as gems com `bundle install` normal.
          Com um calisto.toml no diretorio atual (walk up) o daemon vira o
          daemon da app (socket dedicado) e pre-carrega o entrypoint de
          [run].preload no boot — boot congelado, cada comando roda como fork.
          Um nome que nao e arquivo resolve para [scripts.NAME] do calisto.toml
          (Fase H): `calisto run dev` roda o comando de `dev = \"bin/rails
          server\"` no daemon (como `calisto exec`, com o Gemfile ativo), com
          os args do CLI no final. Arquivo existente sempre vence; calisto.toml
          so com [scripts] (sem [run].preload) usa o daemon generico.
          `calisto run` sem script roda o comando `start` do calisto.toml
          quando existe (convencao npm/bun — o `calisto init` gera).
          O ruby usado (Fase I, multi-versoes) vem de: CALISTO_RUBY (override),
          .ruby-version (walk up, como rbenv) ou a diretiva `ruby \"x.y.z\"` do
          Gemfile; a versao pedida precisa estar em vendor/ruby-<v>/ (rode
          RUBY_VERSION=<v> scripts/build-ruby.sh) — senao e erro claro. Sem
          pedido, o pin default vendor/current ({PINNED_RUBY}). Daemons sao
          isolados por versao (socket proprio).
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
  exec    roda o binario de uma gem no daemon quente, no contexto da app
          (ex.: `calisto exec sidekiq`, `calisto exec rubocop`). Resolucao
          como `bundle exec`, sem depender de binstub: caminho de arquivo
          (./bin/rails), executavel de uma spec do bundle ativo, ou PATH.
          Binario ruby e carregado in-process (kernel_load do bundler);
          nativo e exec direto. Ambiguo (2+ gems com o mesmo executavel) e
          erro com os candidatos; nao encontrado -> 127.
  repl    IRB no daemon quente, no contexto da app pre-carregada (calisto.toml)
          ou da stdlib preloaded (sem app). Args sao repassados ao IRB. Fica
          em foreground; kill no cliente derruba o child.
  build   empacota <app.rb> e seus requires (arquivos do projeto, stdlib-only)
          num arquivo unico self-contained. Arquivos fora da raiz (stdlib)
          nao sao embutidos. --root define a raiz do projeto (default: o
          diretorio do entrypoint).
          --compile embute as gems pure-Ruby do Gemfile.lock (Fase F): o
          bundle roda sem bundle install — GEM_HOME/GEM_PATH vazios. C
          extensions nao embutem (warning; o require cai no bundle real).
          Autoloads das gems sao cobertos (pre-carga em rodadas).
  init    scaffold de app (como `bun init`): calisto.toml com
          [scripts] start = \"./hello.rb\", hello.rb e .gitignore. Sem nome usa
          o diretorio atual; `calisto init <name>` cria <name>/. Nunca
          sobrescreve arquivo existente sem --force. O app gerado roda com
          `calisto run` (bare = o script start).
  upgrade rebuild do pin do CRuby (scripts/build-ruby.sh; idempotente — rubies
          ja construidos sao pulados). `calisto upgrade <version>` constroi
          vendor/ruby-<v> para apps com .ruby-version/Gemfile (versoes com sha
          conhecido: 3.4.10/3.4.4; outras: RUBY_SHA256=<sha> manual).
  completions imprime o script de completions do shell em stdout (bash/zsh) —
          ex.: `calisto completions bash > /etc/bash_completion.d/calisto`.
  add/remove/lock
          wrappers finos do `bundle add/remove/lock` (decisao da Fase A: nada
          de instalador proprio) com o ruby da versao certa (Fase I) e cwd na
          raiz do projeto (walk-up do Gemfile, como o resto do calisto). Args
          passam direto ao bundle (ex.: `calisto add sinatra --group web`);
          sem Gemfile, erro claro sugerindo `bundle init`.
  status  shows whether the warm daemon is running
  stop    stops the warm daemon
  doctor  prints environment, pinned ruby version and daemon state

CONFIG:
  CALISTO_RUBY        path to a ruby binary (default: vendor/current/bin/ruby)
  CALISTO_PRELOAD     comma-separated stdlib preload list
  CALISTO_RUNTIME_DIR daemon socket/pid location (default: $XDG_RUNTIME_DIR/calisto)

NOTE: calisto run is equivalent to `bundle exec ruby <script>` with -e/-E VM
flags (alem de -e) ainda nao suportados; fora de Gemfile, identico a
`ruby <script>`.
Linux only (fork)."
    );
}
