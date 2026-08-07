//! calisto — child
//!
//! child do fork (RUN/EVAL): bootstrap da VM + load/eval.
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — child (extraido de src/main.rs na reorg do CLI).

use std::io::{self, Write};
use std::path::Path;
use std::os::fd::{RawFd};
use std::ffi::{c_char, c_int, CString};
use crate::daemon::*;


const SIG_DFL: usize = 0;

unsafe extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}
unsafe extern "C" {
    fn dup2(oldfd: c_int, newfd: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn chdir(path: *const c_char) -> c_int;
    fn clearenv() -> c_int;
    fn setenv(name: *const c_char, value: *const c_char, overwrite: c_int) -> c_int;
    fn _exit(code: c_int) -> !;
}




/// Primeiro recvmsg de uma conexao: dados + fds SCM_RIGHTS (stdio do
/// cliente) — espelho do RequestReader#fill (scm_rights: true) do server.rb.



/// Erro no child: espelho do report_child_error — full_message sem highlight
/// com os frames do proprio daemon embutido cortados (marcador "eval:" — os
/// snippets gerados pelo calisto rodam com filename "eval"). Se o report
/// falhar, deixa a VM imprimir (ruby_cleanup TAG_RAISE com o errinfo).
/// Nunca retorna.
pub fn child_error(vm: &calisto_ruby::Ruby, e: calisto_ruby::VALUE) -> ! {
    vm.set_errinfo(e);
    let snippet = r#"e = $!
bt = e.backtrace || []
cut = bt.index { |l| l.start_with?("eval:") || l =~ /\A[^:]+:in 'Kernel#load'\z/ } || bt.size
e.set_backtrace(bt[0...cut]) if cut < bt.size
warn e.full_message(highlight: false, order: :top)"#;
    if vm.eval(snippet).is_ok() {
        // errinfo pendente faria o ruby_cleanup re-imprimir o erro
        vm.set_errinfo(calisto_ruby::Qnil);
        let _ = vm.cleanup(0);
        std::process::exit(1);
    }
    // report falhou: VM imprime no formato padrao (TAG_RAISE = 6) e sai 1
    vm.set_errinfo(e);
    let st = vm.cleanup(6);
    std::process::exit(st);
}



/// Corpo do child (RUN/EVAL) — espelho do child_enter + start_child do
/// server.rb: traps default, stdio do cliente, hygiene de fds, cwd, env do
/// RUN (ENV.replace), bundler/setup, CALISTO_LOAD_PATH, $0/ARGV, setup_data,
/// load/eval com semantica de `ruby <script>`/`ruby -e`. Nunca retorna.
pub fn child_main(
    vm: &calisto_ruby::Ruby,
    is_eval: bool,
    cwd: &str,
    env_blob: &str,
    subject: &str,
    args: &[String],
    stdio_fds: &[RawFd],
    control_fd: RawFd,
    listener_fd: RawFd,
) -> ! {
    // obrigatorio: o VM state da timer thread nao sobrevive ao fork
    // (mesmo fix que o Process.fork do ruby aplica no child)
    vm.thread_atfork();
    unsafe {
        signal(SIGINT, SIG_DFL);
        signal(SIGTERM, SIG_DFL);
        signal(SIGHUP, SIG_DFL);
        // dup2 dos fds do cliente; nao fecha os originais (autoclose: false
        // do dup_into_stdio — o child morre logo, sem vazamento duradouro)
        for (idx, dst) in [(0usize, 0i32), (1, 1), (2, 2)] {
            if let Some(fd) = stdio_fds.get(idx) {
                dup2(*fd, dst);
            }
        }
        close(control_fd);
        close(listener_fd);
    }
    let ccwd = CString::new(cwd).unwrap_or_default();
    if unsafe { chdir(ccwd.as_ptr()) } != 0 {
        let e = io::Error::last_os_error();
        let _ = writeln!(io::stderr(), "calisto: chdir {}: {e}", cwd);
        unsafe { _exit(1) };
    }
    // ENV.replace(env_blob): pares "k=v" separados por \x1e
    unsafe { clearenv() };
    for pair in env_blob.split('\u{1e}') {
        if let Some((k, v)) = pair.split_once('=') {
            let ck = CString::new(k).unwrap_or_default();
            let cv = CString::new(v).unwrap_or_default();
            unsafe { setenv(ck.as_ptr(), cv.as_ptr(), 1) };
        }
    }
    // child_enter: ativacao do Gemfile do cwd (no-op fora de bundle)
    if let Err(e) = vm.require("bundler/setup") {
        child_error(vm, e);
    }
    // -I do child (calisto test) + dir nativo (Fase P: shims calisto/*).
    // Depois do Bundler.setup (limpa $LOAD_PATH).
    let loadpath_snippet = r#"if (lp = ENV["CALISTO_LOAD_PATH"])
  lp.split(":").reject(&:empty?).each do |p|
    $LOAD_PATH.unshift(p) unless $LOAD_PATH.include?(p)
  end
end
if (d = $calisto_native_dir)
  $LOAD_PATH.unshift(d) unless $LOAD_PATH.include?(d)
end"#;
    if let Err(e) = vm.eval(loadpath_snippet) {
        child_error(vm, e);
    }
    // $0/ARGV: $0 NAO pode ser setado com o setter original do CRuby — o
    // setter (set_arg0 -> setproctitle) reescreve argv/env in-place com um
    // argv_env_len calculado no boot que mistura argv da heap com env da
    // stack (valor gigante) e corrompe a heap do fork. install_arg0_slot
    // redefine o gvar com slot proprio (escrita direta, sem setter).
    vm.install_arg0_slot();
    vm.set_gv("$0", vm.str(if is_eval { "-e" } else { subject }));
    let arg_items: Vec<&str> = args.iter().map(String::as_str).collect();
    vm.funcall(vm.const_get("ARGV"), "replace", &[vm.ary(&arg_items)]);
    // sync como o dup_into_stdio (stdout/stderr do cliente)
    if let Err(e) = vm.eval("$stdout.sync = true if STDOUT.tty?\n$stderr.sync = true if STDERR.tty?") {
        child_error(vm, e);
    }
    // setup_data: DATA/__END__ para RUN com arquivo existente
    if !is_eval && Path::new(subject).is_file() {
        vm.set_gv("$calisto_script", vm.str(subject));
        let snippet = r#"if File.file?($calisto_script)
  src = File.binread($calisto_script)
  if src.include?("__END__")
    require "ripper"
    line = nil
    Ripper.lex(src).each { |(l, _c), event, _tok, _st| line = l if event == :on___end__ }
    if line
      pos = 0
      line.times do
        nl = src.index("\n", pos)
        pos = nl ? nl + 1 : src.bytesize
      end
      io = File.open($calisto_script, "rb")
      io.seek(pos)
      Object.const_set(:DATA, io)
    end
  end
end"#;
        if let Err(e) = vm.eval(snippet) {
            child_error(vm, e);
        }
    }
    // load / eval: mesma semantica do `ruby <script>` / `ruby -e`
    let res = if is_eval {
        vm.eval_main_iseq(subject)
    } else {
        vm.load(subject)
    };
    match res {
        Ok(_) => {
            // exit normal: cleanup(0) com errinfo limpo
            let st = vm.cleanup(0);
            std::process::exit(st);
        }
        Err(e) if vm.is_system_exit(e) => {
            // exit n do script: at_exit hooks UMA vez. O STATUS sai do
            // ERRINFO (SystemExit) — o param do ruby_cleanup e TAG type,
            // nao status (cleanup(42) viola TAG_FATAL e aborta 134).
            let st = vm.cleanup(0);
            std::process::exit(st);
        }
        Err(e) => child_error(vm, e),
    }
}
