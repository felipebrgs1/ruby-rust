//! calisto — protocol
//!
//! protocolo de rede do daemon (RESP-style + SCM_RIGHTS) e requests do cliente.
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — protocol (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::os::fd::{AsRawFd, RawFd};
use std::process::{ExitStatus};
use std::time::{Duration};
use std::ffi::{c_int, c_void};
use std::mem::size_of;
use crate::base64::*;
use crate::daemon::{MSG_DONTWAIT, SIGKILL, SIGTERM, EAGAIN};


unsafe extern "C" {
    fn sendmsg(fd: i32, msg: *const MsgHdr, flags: i32) -> isize;
}
unsafe extern "C" {
    fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
    fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
    fn recvmsg(fd: c_int, msg: *mut MsgHdr, flags: c_int) -> isize;
    fn kill(pid: i32, sig: i32) -> i32;
    fn waitpid(pid: i32, status: *mut c_int, options: i32) -> i32;
}




/// Primeiro recvmsg de uma conexao: dados + fds SCM_RIGHTS (stdio do
/// cliente) — espelho do RequestReader#fill (scm_rights: true) do server.rb.


#[repr(C)]
pub struct MsgHdr {
    msg_name: *mut c_void,
    msg_namelen: u32, // socklen_t
    msg_iov: *mut Iovec,
    msg_iovlen: usize,
    msg_control: *mut c_void,
    msg_controllen: usize,
    msg_flags: i32,
}


#[repr(C)]
pub struct Iovec {
    iov_base: *mut c_void,
    iov_len: usize,
}


#[repr(C)]
pub struct Cmsghdr {
    cmsg_len: usize,
    cmsg_level: i32,
    cmsg_type: i32,
}



pub const SOL_SOCKET: i32 = 1;


pub const SCM_RIGHTS: i32 = 1;



pub fn align8(n: usize) -> usize {
    (n + 7) & !7
}



pub fn send_with_fds(stream: &mut UnixStream, data: &[u8], fds: &[RawFd]) -> io::Result<()> {
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



pub fn b64(s: &str) -> String {
    b64_encode(s.as_bytes())
}



// ---- wire protocol -------------------------------------------------------------

pub fn send_cmd(stream: &mut UnixStream, op: &str, fields: &[String]) -> io::Result<()> {
    write!(stream, "{op} {}\r\n", fields.len())?;
    for f in fields {
        write!(stream, "${}\r\n", f.len())?;
        stream.write_all(f.as_bytes())?;
    }
    Ok(())
}



pub fn build_cmd(op: &str, fields: &[String]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(format!("{op} {}\r\n", fields.len()).as_bytes());
    for f in fields {
        buf.extend_from_slice(format!("${}\r\n", f.len()).as_bytes());
        buf.extend_from_slice(f.as_bytes());
    }
    buf
}



pub fn read_line(stream: &mut UnixStream) -> io::Result<String> {
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


pub fn recv_with_fds(fd: RawFd) -> io::Result<(Vec<u8>, Vec<RawFd>)> {
    let mut data = vec![0u8; 65_536];
    let mut iov = Iovec {
        iov_base: data.as_mut_ptr() as *mut c_void,
        iov_len: data.len(),
    };
    let mut control = vec![0u8; 128];
    let mut msg = MsgHdr {
        msg_name: std::ptr::null_mut(),
        msg_namelen: 0,
        msg_iov: &mut iov,
        msg_iovlen: 1,
        msg_control: control.as_mut_ptr() as *mut c_void,
        msg_controllen: control.len(),
        msg_flags: 0,
    };
    let n = unsafe { recvmsg(fd, &mut msg, MSG_DONTWAIT) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    data.truncate(n as usize);
    let mut fds = Vec::new();
    let mut off = 0usize;
    while off + size_of::<Cmsghdr>() <= msg.msg_controllen {
        let cmsg = unsafe { &*(control.as_ptr().add(off) as *const Cmsghdr) };
        if cmsg.cmsg_level == SOL_SOCKET && cmsg.cmsg_type == SCM_RIGHTS {
            let data_off = align8(size_of::<Cmsghdr>());
            let nfds = (cmsg.cmsg_len - data_off) / size_of::<i32>();
            for i in 0..nfds {
                let p = unsafe {
                    std::ptr::read(control.as_ptr().add(off + data_off + i * 4) as *const i32)
                };
                fds.push(p);
            }
        }
        off += align8(cmsg.cmsg_len);
    }
    Ok((data, fds))
}



pub struct DaemonClient {
    pub fd: RawFd,
    pub buf: Vec<u8>,
    pub fds: Vec<RawFd>, // stdio do cliente via SCM_RIGHTS (1o recvmsg)
    pub first: bool,
    pub pid: Option<i32>,
}



pub fn respond(fd: RawFd, msg: &str) {
    let bytes = msg.as_bytes();
    let mut off = 0;
    while off < bytes.len() {
        let n = unsafe { write(fd, bytes[off..].as_ptr() as *const c_void, bytes.len() - off) };
        if n <= 0 {
            return; // EPIPE/ECONNRESET: cliente morto (rescue do respond)
        }
        off += n as usize;
    }
}



pub enum CommandErr {
    Partial, // comando incompleto: aguarda mais dados no proximo tick
    Eof,
    Bad(String),
}



pub fn find_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\r\n")
}



/// Espelho do RequestReader#fill: 1o recvmsg com SCM_RIGHTS, depois reads
/// planos; EAGAIN -> Partial.
pub fn client_fill(c: &mut DaemonClient) -> Result<(), CommandErr> {
    if c.first {
        c.first = false;
        match recv_with_fds(c.fd) {
            Ok((data, fds)) => {
                c.fds = fds;
                c.buf.extend_from_slice(&data);
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(CommandErr::Partial),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Err(CommandErr::Partial),
            Err(_) => Err(CommandErr::Eof), // 0 bytes ou erro: "eof" do server.rb
        }
    } else {
        let mut data = [0u8; 65_536];
        let n = unsafe { recv(c.fd, data.as_mut_ptr() as *mut c_void, data.len(), MSG_DONTWAIT) };
        if n > 0 {
            c.buf.extend_from_slice(&data[..n as usize]);
            Ok(())
        } else if n == 0 {
            Err(CommandErr::Eof)
        } else {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(EAGAIN) || e.kind() == io::ErrorKind::Interrupted {
                Err(CommandErr::Partial)
            } else {
                Err(CommandErr::Eof)
            }
        }
    }
}



/// Espelho do read_command: `OP N\r\n` + N campos `$len\r\n<data>`.
pub fn client_read_command(c: &mut DaemonClient) -> Result<(String, Vec<Vec<u8>>), CommandErr> {
    loop {
        if let Some(pos) = find_crlf(&c.buf) {
            let head = String::from_utf8_lossy(&c.buf[..pos]).into_owned();
            c.buf.drain(..pos + 2);
            let (op, count) = match head.split_once(' ') {
                Some(x) => x,
                None => return Err(CommandErr::Bad(format!("bad command: {:?}", head))),
            };
            let count: usize = match count.parse() {
                Ok(n) => n,
                Err(_) => return Err(CommandErr::Bad(format!("bad command: {:?}", head))),
            };
            let mut fields: Vec<Vec<u8>> = Vec::with_capacity(count);
            for _ in 0..count {
                loop {
                    if let Some(pos) = find_crlf(&c.buf) {
                        let line = String::from_utf8_lossy(&c.buf[..pos]).into_owned();
                        c.buf.drain(..pos + 2);
                        let len: usize = match line.strip_prefix('$').and_then(|l| l.parse().ok()) {
                            Some(n) => n,
                            None => return Err(CommandErr::Bad(format!("bad command: {:?}", head))),
                        };
                        if c.buf.len() < len {
                            match client_fill(c) {
                                Ok(()) => continue,
                                Err(e) => return Err(e),
                            }
                        }
                        let data: Vec<u8> = c.buf.drain(..len).collect();
                        fields.push(data);
                        break;
                    } else {
                        match client_fill(c) {
                            Ok(()) => continue,
                            Err(e) => return Err(e),
                        }
                    }
                }
            }
            return Ok((op.to_string(), fields));
        } else {
            match client_fill(c) {
                Ok(()) => continue,
                Err(e) => return Err(e),
            }
        }
    }
}



/// TERM -> (200ms) -> KILL -> wait bloqueante (kill_child do server.rb).
pub fn kill_child(pid: i32) -> Option<i32> {
    unsafe { kill(pid, SIGTERM) };
    std::thread::sleep(Duration::from_millis(200));
    unsafe { kill(pid, SIGKILL) };
    let mut status = 0;
    let r = unsafe { waitpid(pid, &mut status, 0) };
    if r == pid { Some(status) } else { None }
}



/// exitstatus || (128 + termsig) — como o Ruby decode do wait status.
pub fn wait_status_code(status: i32) -> i32 {
    if status & 0x7f == 0 {
        (status >> 8) & 0xff
    } else {
        128 + (status & 0x7f)
    }
}



pub fn eval_request(stream: &mut UnixStream, code: &str, args: &[String]) -> i32 {
    let cwd = env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    eval_request_full(stream, &cwd, &[], code, args)
}



pub fn run_request(stream: &mut UnixStream, script: &str, args: &[String]) -> i32 {
    let cwd = env::current_dir()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    run_request_full(stream, &cwd, &[], script, args)
}



/// Variante com cwd e env extras explicitos (usada por `calisto test`: cwd na
/// raiz do projeto, RAILS_ENV=test e CALISTO_LOAD_PATH injetados no child).
pub fn run_request_full(
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
pub fn eval_request_full(
    stream: &mut UnixStream,
    cwd: &str,
    extra: &[(&str, &str)],
    code: &str,
    args: &[String],
) -> i32 {
    send_run_request(stream, "EVAL", cwd, extra, code, args)
}



pub fn send_run_request(
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
                "CALISTO_RUNTIME_DIR"
                    | "CALISTO_SOCKET"
                    | "CALISTO_PIDFILE"
                    | "CALISTO_PRELOAD"
                    | "CALISTO_COMPACT"
                    | "CALISTO_YJIT"
                    | "CALISTO_WARMUP"
                    | "CALISTO_RUBY"
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



pub fn exit_code(st: ExitStatus) -> i32 {
    st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0))
}
