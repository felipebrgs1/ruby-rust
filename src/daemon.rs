//! calisto — daemon
//!
//! daemon embutido (Fase L): accept loop em Rust, sinais, stdio atfork.
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — daemon (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::fs;
use std::io::{self};
use std::path::{Path, PathBuf};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::fd::{AsRawFd, IntoRawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::ffi::{c_char, c_int, c_void};
use crate::base64::*;
use crate::child::*;
use crate::protocol::*;
use crate::shims::*;


unsafe extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}
unsafe extern "C" {
    fn flockfile(f: *mut c_void);
    fn funlockfile(f: *mut c_void);
}
unsafe extern "C" {
    fn fork() -> i32;
    fn waitpid(pid: i32, status: *mut c_int, options: i32) -> i32;
    fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn open(path: *const c_char, flags: c_int) -> c_int;
    fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
    fn poll(fds: *mut PollFd, nfds: usize, timeout: c_int) -> c_int;
    fn getpid() -> i32;
}




/// Registra (uma vez) os handlers de atfork do stdio — chamado no boot do
/// daemon, antes do accept loop. Best-effort: sem os FILE* exportados pela
/// libc, avisa e segue (o fork volta ao comportamento racy anterior).

/// Primeiro recvmsg de uma conexao: dados + fds SCM_RIGHTS (stdio do
/// cliente).


/// Uso interno (Fase S): `calisto daemon --internal [-r<gem>...]` — o daemon
/// EMBUTIDO: VM CRuby in-process (dlopen libruby) com o accept loop em Rust.
/// Spawnado pelo proprio cliente — unico modo desde a Fase S (o daemon
/// legado morreu); `CALISTO_EMBED_RUBY` aponta o ruby resolvido (para achar
/// a .so). Boot: require das flags `-r` (ex.: `-rbundler/setup`), preload de
/// `CALISTO_PRELOAD`, entrypoint de `CALISTO_APP_PRELOAD`; depois o loop
/// atende PING/RUN/EVAL/STOP com fork por request. Nao retorna ate o
/// shutdown.
// ---- fork-safety do stdio do glibc (Fase N) ----------------------------------
// O child do fork pode herdar um FILE lock do glibc (stdin/stdout/stderr)
// preso: o fork cai com o mutex locked (por outra thread — a timer thread da
// VM passa por caminhos de libc que tocam o stdio) e, como o child é a MESMA
// thread do forker, o lock fica "owned" para sempre -> o child deadlocka no
// primeiro uso de stdio (futex_wait no _lock do FILE; sintoma: cliente
// esperando STATUS para sempre, teste >30s). Solucao: pthread_atfork que
// trava os 3 FILEs padrao no PREPARE (fork acontece com eles held pela
// thread do fork) e destrava no CHILD/PARENT (funlockfile nao checa owner).
pub static mut STDIO_FILES: [*mut c_void; 3] = [std::ptr::null_mut(); 3];



/// Leitura/escrita sem referencias ao static mut (static_mut_refs): o array
/// so e escrito uma vez no registro (antes do accept loop) e lido pelos
/// handlers de atfork — sempre via ponteiro cru.
pub fn stdio_file(i: usize) -> *mut c_void {
    unsafe { (*std::ptr::addr_of!(STDIO_FILES))[i] }
}



pub unsafe extern "C" fn stdio_fork_prepare() {
    for i in 0..3 {
        let f = stdio_file(i);
        if !f.is_null() {
            flockfile(f);
        }
    }
}



pub unsafe extern "C" fn stdio_fork_release() {
    for i in 0..3 {
        let f = stdio_file(i);
        if !f.is_null() {
            funlockfile(f);
        }
    }
}


pub fn stdio_atfork_register() {
    unsafe {
        extern "C" {
            fn pthread_atfork(
                prepare: Option<unsafe extern "C" fn()>,
                parent: Option<unsafe extern "C" fn()>,
                child: Option<unsafe extern "C" fn()>,
            ) -> c_int;
            fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
        }
        for (i, name) in [
            b"_IO_2_1_stdin_\0".as_ptr(),
            b"_IO_2_1_stdout_\0".as_ptr(),
            b"_IO_2_1_stderr_\0".as_ptr(),
        ]
        .iter()
        .enumerate()
        {
            (*std::ptr::addr_of_mut!(STDIO_FILES))[i] =
                dlsym(std::ptr::null_mut(), *name as *const c_char);
        }
        if (0..3).any(|i| stdio_file(i).is_null()) {
            eprintln!(
                "calisto daemon: aviso: FILE* padrao nao resolvidos na libc — \
                 fork sem trava do stdio (child pode herdar FILE lock preso)"
            );
            return;
        }
        if pthread_atfork(
            Some(stdio_fork_prepare),
            Some(stdio_fork_release),
            Some(stdio_fork_release),
        ) != 0
        {
            eprintln!("calisto daemon: aviso: pthread_atfork falhou (stdio)");
        }
    }
}



pub fn cmd_daemon(args: &[String]) -> i32 {
    if args.first().map(String::as_str) != Some("--internal") {
        eprintln!("calisto daemon: uso interno: calisto daemon --internal [-r<gem>...]");
        return 1;
    }
    let Some(ruby) = env::var_os("CALISTO_EMBED_RUBY") else {
        eprintln!("calisto daemon: CALISTO_EMBED_RUBY nao definido (uso interno)");
        return 1;
    };
    let mut requires: Vec<String> = Vec::new();
    let mut yjit = false;
    for a in &args[1..] {
        if let Some(name) = a.strip_prefix("-r") {
            requires.push(name.to_string());
        } else if a == "--yjit" {
            yjit = true;
        } else {
            eprintln!("calisto daemon: flag interna desconhecida: {a}");
            return 1;
        }
    }
    let mut vm = match calisto_ruby::Ruby::open(Path::new(&ruby)) {
        Err(e) => {
            eprintln!("calisto daemon: {e}");
            return 1;
        }
        Ok(vm) => vm,
    };
    if let Err(e) = vm.boot(yjit) {
        eprintln!("calisto daemon: {e}");
        return 1;
    }
    stdio_atfork_register();
    match daemon_main(&vm, &requires) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("calisto daemon: {e}");
            1
        }
    }
}



// ---- daemon embutido — accept loop em Rust ------------------------------------
// Protocolo do daemon: preload, bind com stale-socket recovery, detach,
// traps, RequestReader SCM_RIGHTS, select multi-conexao, waitpid WNOHANG,
// client-death kill TERM->KILL, STOP derruba children, STATUS por child.

pub const SIGINT: i32 = 2;


pub const SIGHUP: i32 = 1;


pub const SIGTERM: i32 = 15;


pub const SIGKILL: i32 = 9;


pub const SIG_IGN: usize = 1;


pub const WNOHANG: i32 = 1;


pub const MSG_PEEK: i32 = 2;


pub const MSG_DONTWAIT: i32 = 0x40;


pub const POLLIN: i16 = 1;


pub const POLLHUP: i16 = 0x10;


pub const POLLERR: i16 = 0x8;


pub const O_RDWR: i32 = 2;


pub const EAGAIN: i32 = 11;


pub const EINTR: i32 = 4;


#[repr(C)]
pub struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}



/// Shutdown (STOP/TERM/HUP): derruba children devolvendo STATUS aos clientes,
/// remove socket/pidfile e encerra a VM (at_exit hooks, como o `exit 0`).
pub fn daemon_shutdown(
    vm: &calisto_ruby::Ruby,
    sock: &Path,
    pidfile: Option<&Path>,
    clients: &mut Vec<DaemonClient>,
) -> i32 {
    for c in clients.iter_mut() {
        if let Some(pid) = c.pid.take() {
            if let Some(status) = kill_child(pid) {
                respond(c.fd, &format!("STATUS {}\r\n", wait_status_code(status)));
            }
        }
    }
    let _ = fs::remove_file(sock);
    if let Some(pf) = pidfile {
        let _ = fs::remove_file(pf);
    }
    vm.cleanup(0)
}



pub static DAEMON_SHUTDOWN: AtomicBool = AtomicBool::new(false);



pub unsafe extern "C" fn on_daemon_term(_: c_int) {
    DAEMON_SHUTDOWN.store(true, Ordering::Relaxed);
}



/// Flag de shutdown como handler de sinal (sigaction-safe: so um store).
pub fn install_daemon_signal_handlers() {
    let term: usize = on_daemon_term as *const c_void as usize;
    unsafe {
        signal(SIGINT, SIG_IGN);
        signal(SIGTERM, term);
        signal(SIGHUP, term);
    }
}



/// Boot + accept loop do daemon embutido:
/// preload -> app preload -> bind (stale socket) -> pidfile -> detach ->
/// traps -> loop (select 10ms, accept, comandos, waitpid WNOHANG).
pub fn daemon_main(vm: &calisto_ruby::Ruby, requires: &[String]) -> Result<i32, String> {
    let sock = PathBuf::from(
        env::var("CALISTO_SOCKET").map_err(|_| "CALISTO_SOCKET nao definido".to_string())?,
    );
    // boot: flags -r (ex.: -rbundler/setup do daemon da app) antes do preload
    for name in requires {
        if let Err(e) = vm.require(name) {
            return Err(format!("require -r{name} falhou: {}", vm.error_summary(e)));
        }
    }
    // Fase P: APIs nativas calisto.* — registradas no boot (Rust ->
    // rb_define_method na VM embutida), antes do preload/compact para o
    // heap dos modulos entrar na compactacao. DEPOIS do loop de -r: o
    // Bundler.setup limpa o $LOAD_PATH e o unshift do dir nativo abaixo
    // precisa sobreviver ate o app preload/warmup (que rodam a seguir).
    // Os shims vivem no dir do socket (o child injeta o dir no $LOAD_PATH
    // apos o proprio Bundler.setup — child_main).
    let native_dir = sock
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    ensure_native_shims(&native_dir);
    let native_dir_s = native_dir.to_string_lossy().into_owned();
    vm.set_gv("$calisto_native_dir", vm.str(&native_dir_s));
    // o proprio daemon tambem precisa do dir no loadpath (app preload/warmup)
    let lp_snippet = r#"if (d = $calisto_native_dir) && !$LOAD_PATH.include?(d)
  $LOAD_PATH.unshift(d)
end"#;
    if let Err(e) = vm.eval(lp_snippet) {
        return Err(format!("native loadpath: {}", vm.error_summary(e)));
    }
    if let Err(e) = calisto_hash::register(vm) {
        return Err(format!("registro calisto/hash falhou: {e}"));
    }
    // Fase T: codecs de string (base64/url/html) — sem deps externas, o
    // registro e obrigatorio como o do hash (nao degrada).
    if let Err(e) = calisto_native::register(vm) {
        return Err(format!("registro calisto/base64,url,html falhou: {e}"));
    }
    if let Err(e) = calisto_sqlite::register(vm) {
        // sem libsqlite3 do sistema: degrada — o shim do require levanta
        // LoadError claro (o calisto segue util sem o sqlite nativo)
        eprintln!("calisto: warning: calisto/sqlite indisponivel: {e}");
    }
    let preload = env::var("CALISTO_PRELOAD").unwrap_or_default();
    for name in preload.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Err(e) = vm.require(name) {
            // espelho do daemon legado: avisa e segue (stderr ainda visivel)
            eprintln!("calisto: preload '{}' failed: {}", name, vm.error_summary(e));
        }
    }
    // app preload (Fase B): entrypoint no boot congelado + AR disconnect +
    // registro em $LOADED_FEATURES (initialize! duplo do Rails)
    if let Some(app) = env::var_os("CALISTO_APP_PRELOAD") {
        let app = app.to_string_lossy().into_owned();
        vm.set_gv("$calisto_app_preload", vm.str(&app));
        let snippet = r#"
begin
  load $calisto_app_preload
rescue SystemExit
  raise
rescue Exception => e
  warn "calisto: app preload falhou (#{$calisto_app_preload}): #{e.class}: #{e.message}"
  warn(e.backtrace.first(8).join("\n")) if e.backtrace
  exit 1
end
if defined?(ActiveRecord::Base) && ActiveRecord::Base.respond_to?(:connection_handler)
  ActiveRecord::Base.connection_handler.clear_all_connections!
end
$LOADED_FEATURES << File.expand_path($calisto_app_preload)
"#;
        match vm.eval(snippet) {
            Err(e) if vm.is_system_exit(e) => return Ok(vm.cleanup(vm.system_exit_status(e))),
            Err(e) => return Err(format!("app preload: {}", vm.error_summary(e))),
            Ok(_) => {}
        }
    }
    // Fase N.1: warmup declarativo (pos-boot, antes da compactacao). Script
    // da app que aquece o codigo quente no daemon — ex.: N requests contra a
    // Rack app em memoria (ActionDispatch::Integration::Session). Com yjit
    // ligado, o hot path compila AQUI e cada child nasce com o JIT pronto.
    // Responsabilidade da app: falha avisa e segue (daemon continua servindo,
    // so perde o pre-aquecimento).
    if let Some(w) = env::var_os("CALISTO_APP_WARMUP") {
        let w = w.to_string_lossy().into_owned();
        vm.set_gv("$calisto_app_warmup", vm.str(&w));
        let snippet = r#"
begin
  load $calisto_app_warmup
rescue SystemExit
  raise
rescue Exception => e
  warn "calisto: warmup falhou (#{$calisto_app_warmup}): #{e.class}: #{e.message}"
  warn(e.backtrace.first(8).join("\n")) if e.backtrace
end
"#;
        match vm.eval(snippet) {
            Err(e) if vm.is_system_exit(e) => return Ok(vm.cleanup(vm.system_exit_status(e))),
            Err(e) => eprintln!("calisto: warmup: {}", vm.error_summary(e)),
            Ok(_) => {}
        }
    }
    // Fase S.4: fork-safety do debug gem. O boot da app (Bundler.require do
    // Rails dev) roda `require "debug"` -> DEBUGGER__::SESSION inicia com
    // TracePoint :script_compiled + threads de UI. O fork do child mata as
    // threads; o 1o script compilado no child (bin/rails...) dispara o
    // TracePoint herdado e o child trava esperando comando de um UI morto.
    // Desativa a sessao pos-boot (deactivate desliga os TracePoints e o UI);
    // o debugger lazy (binding.break no child) continua funcionando — a
    // sessao inicia fresca no processo do child, onde as threads vivem.
    if let Err(e) = vm.eval(
        r#"
if defined?(DEBUGGER__::SESSION)
  begin
    DEBUGGER__::SESSION.deactivate
  rescue Exception => e
    warn "calisto: deactivate do debug session falhou: #{e.class}: #{e.message}"
  end
end
"#,
    ) {
        eprintln!("calisto: debug session: {}", vm.error_summary(e));
    }
    // Fase M.1: compactacao pre-fork (pos-boot, antes do bind). GC.start +
    // GC.compact densificam o heap -> os children (fork) nascem com quase
    // todas as paginas compartilhadas via CoW (o que o child escreve depois
    // custa paginas privadas). Best-effort como o preload: falha avisa e
    // segue — performance, nao semantica. As paginas de codigo do YJIT nao
    // sao heap de objetos — sobrevivem a compactacao (Fase N.3).
    if env::var("CALISTO_COMPACT").as_deref() == Ok("1") {
        if let Err(e) = vm.eval("GC.start\nGC.compact\n") {
            eprintln!("calisto: compact falhou: {}", vm.error_summary(e));
        }
    }
    // bind com stale-socket recovery (EADDRINUSE -> live? -> exit 0 | unlink)
    let pidfile = env::var_os("CALISTO_PIDFILE").map(PathBuf::from);
    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            if UnixStream::connect(&sock).is_ok() {
                return Ok(0); // outro daemon ja dono do socket
            }
            let _ = fs::remove_file(&sock);
            UnixListener::bind(&sock).map_err(|e| format!("bind {}: {e}", sock.display()))?
        }
        Err(e) => return Err(format!("bind {}: {e}", sock.display())),
    };
    listener.set_nonblocking(true).ok();
    let listener_fd = listener.as_raw_fd();
    if let Some(pf) = &pidfile {
        fs::write(pf, format!("{}\n", unsafe { getpid() }))
            .map_err(|e| format!("pidfile {}: {e}", pf.display()))?;
    }
    // detach do proprio stdio (sem isso o daemon segura o stdout pipe do
    // spawner — `calisto run ... | head` pendura). CALISTO_DAEMON_NO_DETACH
    // mantem o stderr do spawner (debug de boot/crash do daemon).
    if env::var_os("CALISTO_DAEMON_NO_DETACH").is_none() {
        unsafe {
            let devnull = open(b"/dev/null\0".as_ptr() as *const c_char, O_RDWR);
            if devnull >= 0 {
                dup2(devnull, 0);
                dup2(devnull, 1);
                dup2(devnull, 2);
                close(devnull);
            }
        }
    }
    // traps (pos-boot): INT sobrevive Ctrl-C; TERM/HUP -> shutdown.
    // Sobrescreve os handlers que a VM instalou no init.
    install_daemon_signal_handlers();
    // ---- accept loop multi-conexao (select 10ms + waitpid WNOHANG) ----
    let mut clients: Vec<DaemonClient> = Vec::new();
    loop {
        if DAEMON_SHUTDOWN.load(Ordering::Relaxed) {
            return Ok(daemon_shutdown(vm, &sock, pidfile.as_deref(), &mut clients));
        }
        let mut pfds = Vec::with_capacity(clients.len() + 1);
        pfds.push(PollFd { fd: listener_fd, events: POLLIN, revents: 0 });
        for c in &clients {
            pfds.push(PollFd { fd: c.fd, events: POLLIN, revents: 0 });
        }
        let n = unsafe { poll(pfds.as_mut_ptr(), pfds.len(), 10) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(EINTR) {
                continue;
            }
            return Err(format!("poll: {e}"));
        }
        // conexoes ativas: so os clientes que estavam no poll (`polled`
        // calculado ANTES do accept — novos clientes ficam fora do pfds e
        // esperam o proximo tick)
        let polled = clients.len();
        if pfds[0].revents & (POLLIN | POLLHUP | POLLERR) != 0 {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(true).ok();
                        clients.push(DaemonClient {
                            fd: stream.into_raw_fd(),
                            buf: Vec::new(),
                            fds: Vec::new(),
                            first: true,
                            pid: None,
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
        let mut i = 0;
        // `polled` limita o pfds (stale apos removals); `clients.len()`
        // protege o acesso apos um remove(i) (o vetor encolheu)
        while i < polled && i < clients.len() {
            let rev = pfds[i + 1].revents;
            if rev & (POLLIN | POLLHUP | POLLERR) == 0 {
                i += 1;
                continue;
            }
            if clients[i].pid.is_some() {
                // child rodando: readable so pode ser EOF (cliente morto) ou
                // dados espurios — o cliente espera STATUS e nao envia mais.
                let mut b = [0u8; 1];
                let r = unsafe { recv(clients[i].fd, b.as_mut_ptr() as *mut c_void, 1, MSG_PEEK | MSG_DONTWAIT) };
                let dead = if r == 0 {
                    true
                } else if r < 0 {
                    io::Error::last_os_error().raw_os_error() != Some(EAGAIN)
                } else {
                    false
                };
                if dead {
                    let pid = clients[i].pid.take().unwrap();
                    kill_child(pid);
                    let _ = unsafe { close(clients[i].fd) };
                    clients.remove(i);
                } else {
                    i += 1;
                }
                continue;
            }
            match client_read_command(&mut clients[i]) {
                Ok((op, fields)) => {
                    let fd = clients[i].fd;
                    match op.as_str() {
                        "PING" => respond(fd, "OK\r\n"),
                        "STOP" => {
                            respond(fd, "BYE\r\n");
                            return Ok(daemon_shutdown(vm, &sock, pidfile.as_deref(), &mut clients));
                        }
                        "RUN" | "EVAL" => {
                            let decoded: Vec<String> = fields.iter().map(|f| b64_decode(f)).collect();
                            // Protocolo de fork do CRuby (process.c
                            // before_fork_ruby/after_fork_ruby): para a timer
                            // thread da VM ANTES do fork e reinicia no parent.
                            // Sem isso, o fork pode cair com a timer thread
                            // segurando vm->ractor.sched.lock (tick de 10ms do
                            // timer_thread_set_timeout) — mutex que o
                            // rb_thread_atfork do child NAO re-inicializa —
                            // e o child deadlocka no primeiro acesso ao
                            // scheduler (cliente espera STATUS para sempre).
                            // Process.fork nunca sofre isso porque para/
                            // reinicia a timer thread; o child a recria no
                            // proprio rb_thread_atfork.
                            vm.stop_timer_thread();
                            let pid = unsafe { fork() };
                            if pid < 0 {
                                vm.start_timer_thread();
                                let e = io::Error::last_os_error();
                                respond(fd, &format!("ERR RuntimeError: fork: {e}\r\n"));
                                let _ = unsafe { close(fd) };
                                clients.remove(i);
                                continue;
                            }
                            if pid == 0 {
                                // child: espelho do child_enter + start_child
                                let child_fds = clients[i].fds.clone();
                                child_main(
                                    vm,
                                    op == "EVAL",
                                    &decoded[0],
                                    &decoded[1],
                                    &decoded[2],
                                    &decoded[3..],
                                    &child_fds,
                                    clients[i].fd,
                                    listener_fd,
                                );
                            }
                            vm.start_timer_thread();
                            // parent: fecha as copias dos fds do cliente
                            for f in clients[i].fds.drain(..) {
                                let _ = unsafe { close(f) };
                            }
                            clients[i].pid = Some(pid);
                        }
                        other => {
                            respond(fd, &format!("ERR unknown command: {:?}\r\n", other));
                            let _ = unsafe { close(fd) };
                            clients.remove(i);
                            continue;
                        }
                    }
                    i += 1;
                }
                Err(CommandErr::Partial) => i += 1,
                Err(CommandErr::Eof) => {
                    respond(clients[i].fd, "ERR RuntimeError: eof\r\n");
                    let _ = unsafe { close(clients[i].fd) };
                    clients.remove(i);
                }
                Err(CommandErr::Bad(msg)) => {
                    respond(clients[i].fd, &format!("ERR RuntimeError: {msg}\r\n"));
                    let _ = unsafe { close(clients[i].fd) };
                    clients.remove(i);
                }
            }
        }
        // children terminados -> STATUS para o cliente (se ainda vivo)
        let mut i = 0;
        while i < clients.len() {
            if let Some(pid) = clients[i].pid {
                let mut status = 0;
                let r = unsafe { waitpid(pid, &mut status, WNOHANG) };
                if r == pid {
                    respond(clients[i].fd, &format!("STATUS {}\r\n", wait_status_code(status)));
                    let _ = unsafe { close(clients[i].fd) };
                    clients.remove(i);
                    continue;
                } else if r < 0 {
                    // ECHILD etc.: defesa — o child nao existe mais
                    respond(clients[i].fd, "STATUS 0\r\n");
                    let _ = unsafe { close(clients[i].fd) };
                    clients.remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }
}
