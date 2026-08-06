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

fn daemon_connect() -> Option<UnixStream> {
    UnixStream::connect(runtime_dir().join("calisto.sock")).ok()
}

fn connect_or_spawn_daemon(ruby: &Path, preload: &str) -> Result<UnixStream, String> {
    let dir = runtime_dir();
    let sock = dir.join("calisto.sock");
    if let Ok(s) = UnixStream::connect(&sock) {
        return Ok(s);
    }
    let rb = dir.join("calisto.rb");
    fs::write(&rb, DAEMON_RB).map_err(|e| format!("cannot write daemon script: {e}"))?;
    let mut child = Command::new(ruby)
        .arg(&rb)
        .env("CALISTO_SOCKET", &sock)
        .env("CALISTO_PIDFILE", dir.join("calisto.pid"))
        .env("CALISTO_PRELOAD", preload)
        .stdin(Stdio::null())
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

    if !Path::new(script).is_file() {
        eprintln!("calisto: cannot open {script}: no such file");
        return 1;
    }

    let ruby = ruby_path();
    let preload = match &preload_opt {
        Some(v) => normalize_preload(v),
        None => env::var("CALISTO_PRELOAD")
            .map_or_else(|_| DEFAULT_PRELOAD.to_string(), |v| normalize_preload(&v)),
    };

    let t0 = Instant::now();
    let code = if cold {
        run_cold(&ruby, script, script_args)
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

fn run_cold(ruby: &Path, script: &str, args: &[String]) -> i32 {
    match Command::new(ruby).arg(script).args(args).status() {
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
    let cwd = env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let env_blob = env::vars()
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "CALISTO_RUNTIME_DIR" | "CALISTO_SOCKET" | "CALISTO_PIDFILE" | "CALISTO_PRELOAD" | "CALISTO_RUBY"
            )
        })
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\u{1e}");
    let mut fields = vec![b64(&cwd), b64(&env_blob), b64(script)];
    fields.extend(args.iter().map(|a| b64(a)));
    let bytes = build_cmd("RUN", &fields);
    if let Err(e) = send_with_fds(&mut stream, &bytes, &[0, 1, 2]) {
        eprintln!("calisto: cannot talk to daemon: {e} (run 'calisto status')");
        return 1;
    }
    match read_line(&mut stream) {
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

fn cmd_status() -> i32 {
    let dir = runtime_dir();
    match daemon_connect() {
        Some(mut s) => {
            let ok = send_cmd(&mut s, "PING", &[]).is_ok()
                && read_line(&mut s).map(|l| l == "OK").unwrap_or(false);
            if ok {
                let pid = fs::read_to_string(dir.join("daemon.pid")).unwrap_or_default();
                println!("daemon: running (pid {})", pid.trim());
            } else {
                println!("daemon: socket present but unresponsive (stale)");
            }
            0
        }
        None => {
            println!("daemon: not running");
            0
        }
    }
}

fn cmd_stop() -> i32 {
    match daemon_connect() {
        Some(mut s) => {
            if send_cmd(&mut s, "STOP", &[]).is_ok() {
                let _ = read_line(&mut s);
            }
            println!("daemon: stopped");
            0
        }
        None => {
            println!("daemon: not running");
            0
        }
    }
}

fn cmd_doctor() -> i32 {
    let ruby = ruby_path();
    println!("calisto doctor");
    println!("  pinned ruby: {}", ruby.display());
    let _ = Command::new(&ruby).arg("-v").status();
    println!(
        "  preload: {}",
        env::var("CALISTO_PRELOAD").unwrap_or_else(|_| DEFAULT_PRELOAD.to_string())
    );
    cmd_status();
    0
}

fn print_help() {
    println!(
        "calisto - a Bun-like runtime for Ruby (pinned CRuby + fork-based fast startup)

USAGE:
  calisto run [--cold] [--time] [--preload LIST] <script.rb> [args...]
  calisto status | stop | doctor | help

  run     executes <script.rb> on the pinned CRuby. Default: warm daemon that
          forks a child per run (fast startup). --cold spawns the interpreter
          directly for baseline comparison.
          --preload LIST overrides the stdlib the daemon preloads (\"0\" disables;
          default: {DEFAULT_PRELOAD}).
  status  shows whether the warm daemon is running
  stop    stops the warm daemon
  doctor  prints environment, pinned ruby version and daemon state

CONFIG:
  CALISTO_RUBY        path to a ruby binary (default: vendor/current/bin/ruby)
  CALISTO_PRELOAD     comma-separated stdlib preload list
  CALISTO_RUNTIME_DIR daemon socket/pid location (default: $XDG_RUNTIME_DIR/calisto)

NOTE: calisto run is equivalent to `ruby <script>` with no VM flags (-e/-E/...).
Linux only (fork)."
    );
}
