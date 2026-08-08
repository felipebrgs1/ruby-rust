//! calisto-native — APIs nativas de codec de string (Fase T do roadmap).
//!
//! `Calisto::Base64`, `Calisto::URL` e `Calisto::HTML` implementados em Rust
//! e registrados na VM embutida do daemon (rb_define_singleton_method via
//! calisto-ruby) — o equivalente dos helpers de string do Bun (`Bun.escapeHTML`
//! etc.). Os equivalentes da stdlib Ruby sao PURE Ruby (base64 0.3, CGI,
//! ERB::Util) e quentes em apps web (JWT, query/forms, views) — o Rust ganha
//! ~3-10x. Zero deps; semantica de SAIDA identica a stdlib (paridade
//! cold/warm via shims com fallback puro).
//!
//! Semantica espelhada (verificada empiricamente contra o CRuby 3.4):
//!   - `Calisto::Base64`: espelho do gem base64 0.3 — encode64 (newline a
//!     cada 60 + final), strict_encode64, urlsafe_encode64(padding),
//!     decode64 (lenient: ignora lixo, `=` para tudo, grupo parcial final),
//!     strict_decode64 (ArgumentError "invalid base64"), urlsafe_decode64
//!     (strict apos tr -_ -> +/).
//!   - `Calisto::URL.escape/unescape`: espelho do CGI — escape mantem
//!     [a-zA-Z0-9_.-], espaco -> `+`, resto -> %XX maiusculo; unescape faz
//!     `+` -> espaco e %xx (case-insensitive) -> byte (invalido fica como
//!     esta).
//!   - `Calisto::HTML.escape`: espelho do ERB::Util.html_escape (CGI.
//!     escapeHTML) — & < > " ' -> &amp; &lt; &gt; &quot; &#39;.
//!
//! Os metodos rodam como C extensions: assinatura `(argc, argv, self)` e
//! longjmp via rb_exc_raise em caso de erro (protegido pelo dispatch da VM).
//! O `Ruby` da VM vive num static — o daemon e single-VM (uma libruby por
//! processo), inicializado em `register()` no boot.

pub mod base64;
pub mod html;
pub mod url;

use calisto_ruby::{Ruby, VALUE};
use std::ffi::c_int;
use std::os::raw::{c_char, c_long, c_void};
use std::sync::Mutex;

static VM: Mutex<Option<usize>> = Mutex::new(None);
static HASH_CLASS: Mutex<Option<VALUE>> = Mutex::new(None);

fn vm() -> &'static Ruby {
    let g = VM.lock().unwrap();
    unsafe { &*(g.expect("calisto-native: register() nao chamado") as *const Ruby) }
}

fn is_hash(v: VALUE) -> bool {
    let klass = HASH_CLASS.lock().unwrap().unwrap_or(calisto_ruby::Qnil);
    klass != calisto_ruby::Qnil && vm().is_kind_of(v, klass)
}

/// Registra os modulos `Calisto::Base64`, `Calisto::URL` e `Calisto::HTML`
/// com os metodos nativos na VM. Chamado no boot do daemon (daemon_main),
/// antes do accept loop — os children do fork herdam os metodos registrados.
pub fn register(vm: &Ruby) -> Result<(), String> {
    *VM.lock().unwrap() = Some(vm as *const Ruby as usize);
    // Classe Hash para distinguir kwargs (argc=-1 recebe keyword como Hash
    // posicional) do argumento posicional em `urlsafe_encode64(bin, padding)`.
    *HASH_CLASS.lock().unwrap() = Some(unsafe { (vm.native().rb_path2class)(c"Hash".as_ptr()) });
    let n = vm.native();
    unsafe {
        let calisto = (n.rb_define_module)(c"Calisto".as_ptr());

        let b64 = (n.rb_define_module_under)(calisto, c"Base64".as_ptr());
        (n.rb_define_singleton_method)(b64, c"encode64".as_ptr(), b64_encode64 as *const c_void, -1);
        (n.rb_define_singleton_method)(b64, c"decode64".as_ptr(), b64_decode64 as *const c_void, -1);
        (n.rb_define_singleton_method)(
            b64,
            c"strict_encode64".as_ptr(),
            b64_strict_encode64 as *const c_void,
            -1,
        );
        (n.rb_define_singleton_method)(
            b64,
            c"strict_decode64".as_ptr(),
            b64_strict_decode64 as *const c_void,
            -1,
        );
        (n.rb_define_singleton_method)(
            b64,
            c"urlsafe_encode64".as_ptr(),
            b64_urlsafe_encode64 as *const c_void,
            -1,
        );
        (n.rb_define_singleton_method)(
            b64,
            c"urlsafe_decode64".as_ptr(),
            b64_urlsafe_decode64 as *const c_void,
            -1,
        );

        let url = (n.rb_define_module_under)(calisto, c"URL".as_ptr());
        (n.rb_define_singleton_method)(url, c"escape".as_ptr(), url_escape as *const c_void, -1);
        (n.rb_define_singleton_method)(url, c"unescape".as_ptr(), url_unescape as *const c_void, -1);

        let html = (n.rb_define_module_under)(calisto, c"HTML".as_ptr());
        (n.rb_define_singleton_method)(html, c"escape".as_ptr(), html_escape as *const c_void, -1);
    }
    Ok(())
}

/// 1 argumento string; devolve (ptr, len) dos bytes.
fn one_string_arg(argc: c_int, argv: *mut VALUE) -> (*const u8, usize) {
    let vm = vm();
    if argc != 1 {
        vm.raise(
            vm.e_arg_error(),
            &format!("wrong number of arguments (given {argc}, expected 1)"),
        );
    }
    let mut v = unsafe { *argv };
    vm.string_bytes(&mut v)
}

// ---- Calisto::Base64 ----------------------------------------------------------

unsafe extern "C" fn b64_encode64(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    let (ptr, len) = one_string_arg(argc, argv);
    let out = base64::encode64(unsafe { std::slice::from_raw_parts(ptr, len) });
    vm.utf8_str(&out)
}

unsafe extern "C" fn b64_decode64(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    let (ptr, len) = one_string_arg(argc, argv);
    let out = base64::decode64(unsafe { std::slice::from_raw_parts(ptr, len) });
    binary_str(vm, &out)
}

unsafe extern "C" fn b64_strict_encode64(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    let (ptr, len) = one_string_arg(argc, argv);
    let out = base64::strict_encode64(unsafe { std::slice::from_raw_parts(ptr, len) });
    vm.utf8_str(&out)
}

unsafe extern "C" fn b64_strict_decode64(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    let (ptr, len) = one_string_arg(argc, argv);
    match base64::strict_decode64(unsafe { std::slice::from_raw_parts(ptr, len) }) {
        Ok(out) => binary_str(vm, &out),
        Err(()) => invalid_base64(vm),
    }
}

unsafe extern "C" fn b64_urlsafe_encode64(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    if argc < 1 || argc > 2 {
        vm.raise(
            vm.e_arg_error(),
            &format!("wrong number of arguments (given {argc}, expected 1..2)"),
        );
    }
    let mut v = unsafe { *argv };
    let (ptr, len) = vm.string_bytes(&mut v);
    // `padding` aceito como keyword (`padding: false` — vira Hash no
    // argc=-1) ou posicional (`false`) — espelho do kwarg do stdlib; tudo
    // que nao e `false` e truthy (semantica Ruby — nil inclusive).
    let mut padding = true;
    if argc == 2 {
        let p = unsafe { *argv.add(1) };
        if is_hash(p) {
            // kwargs chegam como Hash com chave SIMBOLO (:padding)
            let key = unsafe { (vm.native().rb_id2sym)(vm.intern("padding")) };
            padding = vm.funcall(p, "[]", &[key]) != calisto_ruby::Qfalse;
        } else {
            padding = p != calisto_ruby::Qfalse;
        }
    }
    let out = base64::urlsafe_encode64(
        unsafe { std::slice::from_raw_parts(ptr, len) },
        padding,
    );
    vm.utf8_str(&out)
}

unsafe extern "C" fn b64_urlsafe_decode64(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    let (ptr, len) = one_string_arg(argc, argv);
    match base64::urlsafe_decode64(unsafe { std::slice::from_raw_parts(ptr, len) }) {
        Ok(out) => binary_str(vm, &out),
        Err(()) => invalid_base64(vm),
    }
}

fn invalid_base64(vm: &Ruby) -> ! {
    vm.raise(vm.e_arg_error(), "invalid base64")
}

/// String binaria (ASCII-8BIT) — decode devolve bytes arbitrarios.
fn binary_str(vm: &Ruby, bytes: &[u8]) -> VALUE {
    unsafe { (vm.native().rb_str_new)(bytes.as_ptr() as *const c_char, bytes.len() as c_long) }
}

// ---- Calisto::URL --------------------------------------------------------------

unsafe extern "C" fn url_escape(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    let (ptr, len) = one_string_arg(argc, argv);
    let out = url::escape(unsafe { std::slice::from_raw_parts(ptr, len) });
    vm.utf8_str(&out)
}

unsafe extern "C" fn url_unescape(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    let (ptr, len) = one_string_arg(argc, argv);
    let out = url::unescape(unsafe { std::slice::from_raw_parts(ptr, len) });
    vm.utf8_str(&out)
}

// ---- Calisto::HTML ---------------------------------------------------------------

unsafe extern "C" fn html_escape(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    let (ptr, len) = one_string_arg(argc, argv);
    let out = html::escape(unsafe { std::slice::from_raw_parts(ptr, len) });
    vm.utf8_str(&out)
}
