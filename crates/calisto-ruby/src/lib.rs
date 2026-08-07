//! calisto-ruby — CRuby embutido via dlopen (Fase L do roadmap).
//!
//! O calisto vira o runtime: em vez de `spawn vendor/current/bin/ruby
//! server.rb`, o daemon carrega `libruby.so.<v>` do ruby resolvido (Fase I)
//! e controla a VM in-process. Zero deps: FFI hand-rolled via dlopen/dlsym
//! (libc), como o resto do repo.
//!
//! Modos de uso:
//! - `Ruby::open` + `boot` + chamadas protegidas (`require`/`load`/`eval`/
//!   `funcall_protected`) — o daemon embutido (L.4): a VM boota uma vez, o
//!   accept loop vive em Rust e cada child de fork roda o bootstrap Ruby sob
//!   `rb_protect`. Nenhuma chamada entra na VM sem proteção de exceção
//!   (longjmp sem rb_protect = abort/corrupção).
//!
//! Todos os simbolos sao resolvidos por dlsym apos o dlopen — nunca extern
//! link-time (o binario calisto nao ganha dependencia de link da libruby).
//! RTLD_GLOBAL e obrigatorio: C extensions carregadas depois (children,
//! bundle) resolvem os simbolos `rb_*` contra esta libruby — no modo legado
//! quem exportava era o executavel ruby. `ruby_init_loadpath` deriva o
//! loadpath da localizacao da propria libruby via dladdr (LOAD_RELATIVE no
//! Linux), entao o stdlib e encontrado mesmo com o executavel sendo o
//! calisto, nao o ruby.

#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)] // Qnil/Qfalse/Qtrue: constantes do ruby.h

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub type VALUE = usize;
pub type ID = usize;

pub const Qnil: VALUE = 0x04;
pub const Qfalse: VALUE = 0x00;
pub const Qtrue: VALUE = 0x14;

/// INT2FIX (valores pequenos): `(n << 1) | 1`, layout fixo do ruby.
pub fn fixnum(n: i32) -> VALUE {
    ((n as VALUE) << 1) | 1
}

const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

// API publica do CRuby 3.4 (ruby.h / ruby/intern.h) — estavel por serie 3.4.x.
type RbProtectFn = unsafe extern "C" fn(
    Option<unsafe extern "C" fn(VALUE) -> VALUE>,
    VALUE,
    *mut c_int,
) -> VALUE;
type RbFuncallvFn = unsafe extern "C" fn(VALUE, ID, c_int, *const VALUE) -> VALUE;

struct RubyFns {
    ruby_sysinit: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
    ruby_init_stack: unsafe extern "C" fn(*mut c_void),
    ruby_init: unsafe extern "C" fn() -> c_int,
    ruby_options: unsafe extern "C" fn(c_int, *mut *mut c_char) -> *mut c_void,
    ruby_cleanup: unsafe extern "C" fn(c_int) -> c_int,
    rb_thread_atfork: unsafe extern "C" fn(),
    rb_protect: RbProtectFn,
    rb_eval_string_protect: unsafe extern "C" fn(*const c_char, *mut c_int) -> VALUE,
    rb_errinfo: unsafe extern "C" fn() -> VALUE,
    rb_set_errinfo: unsafe extern "C" fn(VALUE),
    rb_intern: unsafe extern "C" fn(*const c_char) -> ID,
    rb_funcallv: RbFuncallvFn,
    rb_gv_get: unsafe extern "C" fn(*const c_char) -> VALUE,
    rb_gv_set: unsafe extern "C" fn(*const c_char, VALUE) -> VALUE,
    rb_ary_new: unsafe extern "C" fn() -> VALUE,
    rb_ary_push: unsafe extern "C" fn(VALUE, VALUE) -> VALUE,
    rb_utf8_str_new_cstr: unsafe extern "C" fn(*const c_char) -> VALUE,
    rb_string_value_cstr: unsafe extern "C" fn(*mut VALUE) -> *mut c_char,
    rb_obj_classname: unsafe extern "C" fn(VALUE) -> *const c_char,
    rb_obj_is_kind_of: unsafe extern "C" fn(VALUE, VALUE) -> VALUE,
    rb_path2class: unsafe extern "C" fn(*const c_char) -> VALUE,
    rb_const_get: unsafe extern "C" fn(VALUE, ID) -> VALUE,
    rb_num2int: unsafe extern "C" fn(VALUE) -> c_int,
    rb_parser_new: unsafe extern "C" fn() -> VALUE,
    rb_parser_set_context: unsafe extern "C" fn(VALUE, VALUE, c_int) -> VALUE,
    rb_parser_compile_string_path: unsafe extern "C" fn(VALUE, VALUE, VALUE, c_int) -> VALUE,
    rb_iseq_new_main: unsafe extern "C" fn(VALUE, VALUE, VALUE, VALUE, c_int) -> VALUE,
    rb_iseq_eval_main: unsafe extern "C" fn(VALUE) -> VALUE,
}

fn last_dlerror() -> String {
    let p = unsafe { dlerror() };
    if p.is_null() {
        "dlerror: unknown error".into()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// Resolve um simbolo da libruby pelo nome (com NUL). Erro claro se faltar:
/// libruby de versao/serie incompativel com a API esperada.
unsafe fn load_sym<T>(handle: *mut c_void, name: &'static [u8]) -> Result<T, String> {
    let sym = dlsym(handle, name.as_ptr() as *const c_char);
    if sym.is_null() {
        return Err(format!(
            "simbolo '{}' nao encontrado na libruby: {}",
            String::from_utf8_lossy(&name[..name.len() - 1]),
            last_dlerror()
        ));
    }
    Ok(std::mem::transmute_copy(&sym))
}

/// Caminho da libruby do ruby resolvido, se houver (`<bin>/../lib/libruby.so*`).
/// O cliente usa para decidir entre daemon embutido e legado (spawn do ruby).
/// Prefere o SONAME mais especifico (`libruby.so.3.4.10` > `libruby.so.3.4` >
/// `libruby.so`) — o build com `--enable-shared` da Fase L instala os tres.
pub fn libruby_path(ruby: &Path) -> Option<PathBuf> {
    let lib_dir = ruby.parent()?.parent()?.join("lib");
    let mut best: Option<PathBuf> = None;
    for entry in std::fs::read_dir(&lib_dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("libruby.so") {
            let better = match &best {
                Some(b) => {
                    let b_len = b.file_name().map(|f| f.len()).unwrap_or(0);
                    name.len() > b_len
                }
                None => true,
            };
            if better {
                best = Some(entry.path());
            }
        }
    }
    best
}

// ---- trampolines do rb_protect ----------------------------------------------
// rb_protect precisa de fn pointer estatico; o contexto (fn da VM + args)
// vive num Mutex global — o daemon e single-threaded no ponto de uso.

struct FnCallCtx {
    f: RbFuncallvFn,
    recv: VALUE,
    mid: ID,
    argc: c_int,
    argv: usize, // *const VALUE (Mutex exige Send)
}
static FN_CALL_CTX: Mutex<Option<FnCallCtx>> = Mutex::new(None);

unsafe extern "C" fn tramp_funcall(_: VALUE) -> VALUE {
    let ctx = FN_CALL_CTX.lock().unwrap().take().expect("funcall ctx");
    (ctx.f)(ctx.recv, ctx.mid, ctx.argc, ctx.argv as *const VALUE)
}

/// Chamada C generica com ate 8 args VALUE (cadeia do iseq do `-e`). O
/// trampoline declara 8 params — callees com menos args ignoram os extras
/// (ABI SysV: args em registradores).
type RawCallFn = unsafe extern "C" fn(VALUE, VALUE, VALUE, VALUE, VALUE, VALUE, VALUE, VALUE) -> VALUE;

struct CallCtx {
    f: usize, // RawCallFn (fn ptr como usize — Mutex exige Send)
    args: [VALUE; 8],
}
static CALL_CTX: Mutex<Option<CallCtx>> = Mutex::new(None);

unsafe extern "C" fn tramp_call(_: VALUE) -> VALUE {
    let ctx = CALL_CTX.lock().unwrap().take().expect("call ctx");
    let f: RawCallFn = std::mem::transmute(ctx.f);
    f(ctx.args[0], ctx.args[1], ctx.args[2], ctx.args[3], ctx.args[4], ctx.args[5], ctx.args[6], ctx.args[7])
}

/// VM CRuby embutida (handle dlopen da libruby + tabela de simbolos).
pub struct Ruby {
    _handle: *mut c_void,   // mantida viva; nunca descarregada (daemon)
    _argv0: CString,        // ruby_sysinit guarda o ponteiro (origarg)
    fns: RubyFns,
    c_object: VALUE,        // rb_cObject (global da VM)
    system_exit_class: VALUE,
}

impl Ruby {
    /// Abre (dlopen RTLD_NOW | RTLD_GLOBAL) a libruby do ruby resolvido.
    pub fn open(ruby: &Path) -> Result<Ruby, String> {
        let so = libruby_path(ruby)
            .ok_or_else(|| format!("libruby.so nao encontrada para {}", ruby.display()))?;
        let cpath = CString::new(so.to_string_lossy().as_bytes())
            .map_err(|_| format!("caminho invalido: {}", so.display()))?;
        let handle = unsafe { dlopen(cpath.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
        if handle.is_null() {
            return Err(format!("dlopen {}: {}", so.display(), last_dlerror()));
        }
        let fns = unsafe {
            RubyFns {
                ruby_sysinit: load_sym(handle, b"ruby_sysinit\0")?,
                ruby_init_stack: load_sym(handle, b"ruby_init_stack\0")?,
                ruby_init: load_sym(handle, b"ruby_init\0")?,
                ruby_options: load_sym(handle, b"ruby_options\0")?,
                ruby_cleanup: load_sym(handle, b"ruby_cleanup\0")?,
                rb_thread_atfork: load_sym(handle, b"rb_thread_atfork\0")?,
                rb_protect: load_sym(handle, b"rb_protect\0")?,
                rb_eval_string_protect: load_sym(handle, b"rb_eval_string_protect\0")?,
                rb_errinfo: load_sym(handle, b"rb_errinfo\0")?,
                rb_set_errinfo: load_sym(handle, b"rb_set_errinfo\0")?,
                rb_intern: load_sym(handle, b"rb_intern\0")?,
                rb_funcallv: load_sym(handle, b"rb_funcallv\0")?,
                rb_gv_get: load_sym(handle, b"rb_gv_get\0")?,
                rb_gv_set: load_sym(handle, b"rb_gv_set\0")?,
                rb_ary_new: load_sym(handle, b"rb_ary_new\0")?,
                rb_ary_push: load_sym(handle, b"rb_ary_push\0")?,
                rb_utf8_str_new_cstr: load_sym(handle, b"rb_utf8_str_new_cstr\0")?,
                rb_string_value_cstr: load_sym(handle, b"rb_string_value_cstr\0")?,
                rb_obj_classname: load_sym(handle, b"rb_obj_classname\0")?,
                rb_obj_is_kind_of: load_sym(handle, b"rb_obj_is_kind_of\0")?,
                rb_path2class: load_sym(handle, b"rb_path2class\0")?,
                rb_const_get: load_sym(handle, b"rb_const_get\0")?,
                rb_num2int: load_sym(handle, b"rb_num2int\0")?,
                rb_parser_new: load_sym(handle, b"rb_parser_new\0")?,
                rb_parser_set_context: load_sym(handle, b"rb_parser_set_context\0")?,
                rb_parser_compile_string_path: load_sym(handle, b"rb_parser_compile_string_path\0")?,
                rb_iseq_new_main: load_sym(handle, b"rb_iseq_new_main\0")?,
                rb_iseq_eval_main: load_sym(handle, b"rb_iseq_eval_main\0")?,
            }
        };
        Ok(Ruby {
            _handle: handle,
            _argv0: CString::new("ruby").expect("literal"),
            fns,
            c_object: Qnil,
            // system_exit_class/c_object so existem apos o boot (BSS da
            // libruby inicializado pelo ruby_init; rb_path2class antes = crash)
            system_exit_class: Qnil,
        })
    }

    /// Boot da VM (main.c do CRuby): sysinit -> init_stack -> init ->
    /// `ruby_options(["-e", ""])` — roda o process_options COMPLETO do CLI
    /// (loadpath via dladdr da propria libruby, Init_enc, Init_ext, builtin
    /// inits e prelude/rubygems) sem executar codigo de usuario (o node
    /// retornado nao e rodado). Sem o prelude, `require "bundler/setup"` e
    /// a ativacao de default gems nao funcionam; sem Init_ext/builtin
    /// inits, classes core (ex.: metodos de Time) ficam quebradas — os
    /// simbolos desses inits nao sao exportados, entao o caminho manual
    /// nao replica o boot.
    pub fn boot(&mut self) -> Result<(), String> {
        unsafe {
            let mut argc: c_int = 1;
            let mut argv_ptr = [self._argv0.as_ptr() as *mut c_char].as_mut_ptr();
            (self.fns.ruby_sysinit)(&mut argc, &mut argv_ptr);
            let mut stack_marker = 0usize;
            (self.fns.ruby_init_stack)(&mut stack_marker as *mut usize as *mut c_void);
            (self.fns.ruby_init)();
        }
        let c_e = CString::new("-e").expect("literal");
        let c_empty = CString::new("").expect("literal");
        let argv = [
            self._argv0.as_ptr() as *mut c_char,
            c_e.as_ptr() as *mut c_char,
            c_empty.as_ptr() as *mut c_char,
        ];
        let node = unsafe { (self.fns.ruby_options)(3, argv.as_ptr() as *mut *mut c_char) };
        if node.is_null() {
            return Err("boot: ruby_options retornou nulo".into());
        }
        if (node as usize) & 1 == 1 {
            // node de erro: INT2FIX(exitcode)
            return Err(format!("boot: ruby_options falhou (exit {})", (node as usize) >> 1));
        }
        // classes/globais da VM so existem apos o init (BSS da libruby
        // inicializado pelo ruby_init; rb_path2class antes = crash)
        let c_system_exit = CString::new("SystemExit").expect("literal");
        self.system_exit_class = unsafe { (self.fns.rb_path2class)(c_system_exit.as_ptr()) };
        let c_object_ptr = unsafe {
            dlsym(self._handle, b"rb_cObject\0".as_ptr() as *const c_char) as *const VALUE
        };
        if !c_object_ptr.is_null() {
            self.c_object = unsafe { *c_object_ptr };
        }
        Ok(())
    }

    /// `require name` protegido — via Kernel#require (hook do rubygems
    /// ativado), NAO rb_require C-level: default gems em $LOAD_PATH
    /// (json etc.) e bundled gems (base64/csv) precisam da ativacao do
    /// rubygems. Err(VALUE) = excecao pendente.
    pub fn require(&self, name: &str) -> Result<VALUE, VALUE> {
        let s = self.str(name);
        self.funcall_protected(Qnil, "require", &[s])
    }

    /// `load path` protegido — via Kernel#load (rb_f_load): rb_find_file
    /// ($LOAD_PATH) e, nao achado, o CWD com o caminho ORIGINAL (backtrace
    /// relativo, como o daemon legado). rb_load C-level nao tem o fallback
    /// do CWD (LoadError para paths relativos com slash).
    pub fn load(&self, path: &str) -> Result<VALUE, VALUE> {
        let s = self.str(path);
        self.funcall_protected(Qnil, "load", &[s])
    }

    /// `eval code` protegido (filename "eval", como rb_eval_string).
    pub fn eval(&self, code: &str) -> Result<VALUE, VALUE> {
        let ccode = CString::new(code).unwrap_or_else(|_| CString::new("").unwrap());
        let mut state = 0;
        let v = unsafe { (self.fns.rb_eval_string_protect)(ccode.as_ptr(), &mut state) };
        if state != 0 { Err(unsafe { (self.fns.rb_errinfo)() }) } else { Ok(v) }
    }

    /// Chamada de metodo sem protecao (so para metodos que nao levantam).
    pub fn funcall(&self, recv: VALUE, method: &str, args: &[VALUE]) -> VALUE {
        let mid = self.intern(method);
        unsafe { (self.fns.rb_funcallv)(recv, mid, args.len() as c_int, args.as_ptr()) }
    }

    /// Chamada de metodo protegida (pode levantar).
    pub fn funcall_protected(&self, recv: VALUE, method: &str, args: &[VALUE]) -> Result<VALUE, VALUE> {
        let mid = self.intern(method);
        *FN_CALL_CTX.lock().unwrap() = Some(FnCallCtx {
            f: self.fns.rb_funcallv,
            recv,
            mid,
            argc: args.len() as c_int,
            argv: args.as_ptr() as usize,
        });
        let mut state = 0;
        let v = unsafe { (self.fns.rb_protect)(Some(tramp_funcall), 0, &mut state) };
        if state != 0 { Err(unsafe { (self.fns.rb_errinfo)() }) } else { Ok(v) }
    }

    /// Chamada C generica protegida (ate 8 args VALUE). Err = excecao.
    pub fn protected_call(&self, f: usize, args: &[VALUE]) -> Result<VALUE, VALUE> {
        let mut arr = [Qnil; 8];
        for (i, a) in args.iter().enumerate().take(8) {
            arr[i] = *a;
        }
        *CALL_CTX.lock().unwrap() = Some(CallCtx { f, args: arr });
        let mut state = 0;
        let v = unsafe { (self.fns.rb_protect)(Some(tramp_call), 0, &mut state) };
        if state != 0 { Err(unsafe { (self.fns.rb_errinfo)() }) } else { Ok(v) }
    }

    /// Roda `code` como `ruby -e` (cadeia do process_options do CLI):
    /// rb_parser_new -> compile_string_path("-e") -> rb_iseq_new_main ->
    /// rb_iseq_eval_main. Sem frames de eval (backtrace so com "-e:N",
    /// como o CLI) e sem precisar de TOPLEVEL_BINDING. Err = excecao
    /// (inclui SyntaxError da compilacao).
    pub fn eval_main_iseq(&self, code: &str) -> Result<VALUE, VALUE> {
        let parser = unsafe { (self.fns.rb_parser_new)() };
        // contexto top-level (process_script do CLI: rb_parser_set_context
        // com parent=NULL e top_level=TRUE — sem isso o parse do -e sai com
        // arvore inconsistente e o compile crasha). Params `int`/ponteiro
        // (parent, top_level, line, opt) entram como VALUE bruto
        // (0 = NULL/false, 1 = true) — Qnil (4) como ponteiro nao e NULL!
        let _ = self.protected_call(
            self.fns.rb_parser_set_context as usize,
            &[parser, 0, 1],
        )?;
        let src = self.str(code);
        let fname = self.str("-e");
        let ast = self.protected_call(
            self.fns.rb_parser_compile_string_path as usize,
            &[parser, fname, src, 1],
        )?;
        let iseq = self.protected_call(
            self.fns.rb_iseq_new_main as usize,
            // (ast, path, realpath, parent, opt) — parent e ponteiro:
            // 0 (NULL), nunca Qnil (4) — o branch `else if (parent)` do
            // iseq_new_with_opt le ISEQ_BODY(parent) e crasha com 4
            &[ast, fname, Qnil, 0, 1],
        )?;
        // SEM rb_ast_dispose explicito: o ast_data_type registra
        // dfree = rb_ast_dispose — o GC libera o node buffer sozinho
        // (o CLI dispoe por higiene de processo longo; o child do daemon
        // roda um eval e morre). Dispose manual duplicaria a libertacao.
        self.protected_call(self.fns.rb_iseq_eval_main as usize, &[iseq])
    }

    pub fn intern(&self, name: &str) -> ID {
        let cname = CString::new(name).unwrap_or_else(|_| CString::new("").unwrap());
        unsafe { (self.fns.rb_intern)(cname.as_ptr()) }
    }

    /// Constante de Object (ex.: TOPLEVEL_BINDING).
    pub fn const_get(&self, name: &str) -> VALUE {
        unsafe { (self.fns.rb_const_get)(self.c_object, self.intern(name)) }
    }

    pub fn errinfo(&self) -> VALUE {
        unsafe { (self.fns.rb_errinfo)() }
    }

    pub fn set_errinfo(&self, e: VALUE) {
        unsafe { (self.fns.rb_set_errinfo)(e) };
    }

    pub fn str(&self, s: &str) -> VALUE {
        let c = CString::new(s).unwrap_or_else(|_| CString::new("").unwrap());
        unsafe { (self.fns.rb_utf8_str_new_cstr)(c.as_ptr()) }
    }

    /// Array de strings Ruby (para ARGV.replace).
    pub fn ary(&self, items: &[&str]) -> VALUE {
        let a = unsafe { (self.fns.rb_ary_new)() };
        for it in items {
            let s = self.str(it);
            unsafe { (self.fns.rb_ary_push)(a, s) };
        }
        a
    }

    pub fn set_gv(&self, name: &str, v: VALUE) {
        let c = CString::new(name).unwrap_or_else(|_| CString::new("").unwrap());
        unsafe { (self.fns.rb_gv_set)(c.as_ptr(), v) };
    }

    pub fn get_gv(&self, name: &str) -> VALUE {
        let c = CString::new(name).unwrap_or_else(|_| CString::new("").unwrap());
        unsafe { (self.fns.rb_gv_get)(c.as_ptr()) }
    }

    pub fn classname(&self, obj: VALUE) -> String {
        let p = unsafe { (self.fns.rb_obj_classname)(obj) };
        if p.is_null() { "<unknown>".into() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
    }

    /// `e.message` (sem levantar).
    pub fn message(&self, e: VALUE) -> String {
        let msg = self.funcall(e, "message", &[]);
        let mut v = msg;
        let p = unsafe { (self.fns.rb_string_value_cstr)(&mut v) };
        if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
    }

    /// "Class: message" — formato dos avisos do daemon legado.
    pub fn error_summary(&self, e: VALUE) -> String {
        format!("{}: {}", self.classname(e), self.message(e))
    }

    /// SystemExit? (is_a? — cobre subclasses, como o rescue SystemExit).
    pub fn is_system_exit(&self, e: VALUE) -> bool {
        unsafe { (self.fns.rb_obj_is_kind_of)(e, self.system_exit_class) == Qtrue }
    }

    /// Status do SystemExit (exit sem argumento = 0).
    pub fn system_exit_status(&self, e: VALUE) -> i32 {
        let st = self.funcall(e, "status", &[]);
        if st == Qnil { 0 } else { unsafe { (self.fns.rb_num2int)(st) } }
    }

    /// Shutdown ordenado da VM: roda at_exit hooks + finalizers, devolve o
    /// status (como `exit n` do ruby).
    pub fn cleanup(&self, status: i32) -> i32 {
        unsafe { (self.fns.ruby_cleanup)(status) }
    }

    /// Fix do VM state APOS fork no child (como o Process.fork do ruby faz
    /// via rb_thread_atfork): a timer thread da VM nao sobrevive ao fork e o
    /// ruby_cleanup/GC penduram tentando join nela. OBRIGATORIO no child
    /// antes de qualquer chamada a VM.
    pub fn thread_atfork(&self) {
        unsafe { (self.fns.rb_thread_atfork)() };
    }
}
