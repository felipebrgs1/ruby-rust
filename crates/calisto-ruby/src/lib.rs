//! calisto-ruby — CRuby embutido via dlopen (Fase L do roadmap).
//!
//! O calisto vira o runtime: em vez de `spawn vendor/current/bin/ruby
//! server.rb`, o daemon carrega `libruby.so.<v>` do ruby resolvido (Fase I)
//! e roda o script com a VM in-process — mesma sequencia do `main.c` do
//! CRuby (`ruby_sysinit` -> `ruby_init_stack` -> `ruby_init` ->
//! `ruby_options` -> `ruby_run_node`), entao flags, $0/ARGV, at_exit e exit
//! codes sao os do `ruby` real. Zero deps: FFI hand-rolled via dlopen/dlsym
//! (libc), como o resto do repo.
//!
//! Todos os simbolos da VM sao resolvidos por dlsym apos o dlopen — nunca
//! declarados como extern link-time (nao ha link contra a libruby; o binario
//! calisto continua sem dependencia do ruby). RTLD_GLOBAL e obrigatorio: C
//! extensions carregadas depois (children, bundle) resolvem os simbolos
//! `rb_*` contra esta libruby — no modo legado quem exportava era o
//! executavel ruby. `ruby_init_loadpath` deriva o loadpath da localizacao da
//! propria libruby via dladdr (LOAD_RELATIVE no Linux), entao o stdlib e
//! encontrado mesmo com o executavel sendo o calisto, nao o ruby.

#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)] // Qnil/Qfalse/Qtrue: constantes do ruby.h

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};

pub type VALUE = usize;
pub type ID = usize;

pub const Qnil: VALUE = 0x04;
pub const Qfalse: VALUE = 0x00;
pub const Qtrue: VALUE = 0x14;

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
type RbFuncallFn = unsafe extern "C" fn(VALUE, ID, c_int, ...) -> VALUE;

struct RubyFns {
    ruby_sysinit: unsafe extern "C" fn(*mut c_int, *mut *mut *mut c_char),
    ruby_init_stack: unsafe extern "C" fn(*mut c_void),
    ruby_init: unsafe extern "C" fn() -> c_int,
    ruby_options: unsafe extern "C" fn(c_int, *mut *mut c_char) -> *mut c_void,
    ruby_run_node: unsafe extern "C" fn(*mut c_void) -> c_int,
    rb_protect: RbProtectFn,
    rb_eval_string_protect: unsafe extern "C" fn(*const c_char, *mut c_int) -> VALUE,
    rb_load_protect: unsafe extern "C" fn(VALUE, c_int, *mut c_int) -> VALUE,
    rb_errinfo: unsafe extern "C" fn() -> VALUE,
    rb_set_errinfo: unsafe extern "C" fn(VALUE),
    rb_intern: unsafe extern "C" fn(*const c_char) -> ID,
    rb_funcall: RbFuncallFn,
    rb_str_new_cstr: unsafe extern "C" fn(*const c_char) -> VALUE,
    rb_string_value_cstr: unsafe extern "C" fn(*mut VALUE) -> *mut c_char,
    rb_gc_start: unsafe extern "C" fn(),
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

/// VM CRuby embutida (handle dlopen da libruby + tabela de simbolos).
pub struct Ruby {
    _handle: *mut c_void, // mantida viva; nunca descarregada (daemon)
    fns: RubyFns,
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
                ruby_run_node: load_sym(handle, b"ruby_run_node\0")?,
                rb_protect: load_sym(handle, b"rb_protect\0")?,
                rb_eval_string_protect: load_sym(handle, b"rb_eval_string_protect\0")?,
                rb_load_protect: load_sym(handle, b"rb_load_protect\0")?,
                rb_errinfo: load_sym(handle, b"rb_errinfo\0")?,
                rb_set_errinfo: load_sym(handle, b"rb_set_errinfo\0")?,
                rb_intern: load_sym(handle, b"rb_intern\0")?,
                rb_funcall: load_sym(handle, b"rb_funcall\0")?,
                rb_str_new_cstr: load_sym(handle, b"rb_str_new_cstr\0")?,
                rb_string_value_cstr: load_sym(handle, b"rb_string_value_cstr\0")?,
                rb_gc_start: load_sym(handle, b"rb_gc_start\0")?,
            }
        };
        Ok(Ruby { _handle: handle, fns })
    }

    /// Roda `ruby <argv>` embutido (main.c do CRuby): flags, script, -e,
    /// $0/ARGV, at_exit e exit code iguais aos do `ruby` real. argv[0] e
    /// ignorado (reposto como "ruby"). Nao retorna ate o script terminar.
    pub fn run_script(&self, argv: &[String]) -> i32 {
        let mut args: Vec<CString> = Vec::with_capacity(argv.len() + 1);
        args.push(CString::new("ruby").expect("literal sem NUL"));
        for a in argv {
            // argv do daemon nunca tem NUL; defesa barata para nao abortar
            args.push(CString::new(a.as_str()).unwrap_or_else(|_| CString::new("").unwrap()));
        }
        let mut raw: Vec<*mut c_char> = args.iter_mut().map(|c| c.as_ptr() as *mut c_char).collect();
        unsafe {
            let mut argc = raw.len() as c_int;
            let mut argv_ptr = raw.as_mut_ptr();
            (self.fns.ruby_sysinit)(&mut argc, &mut argv_ptr);
            let mut stack_marker = 0usize;
            (self.fns.ruby_init_stack)(&mut stack_marker as *mut usize as *mut c_void);
            (self.fns.ruby_init)();
            (self.fns.ruby_run_node)((self.fns.ruby_options)(argc, argv_ptr))
        }
    }
}
