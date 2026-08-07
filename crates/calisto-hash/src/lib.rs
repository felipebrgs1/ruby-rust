//! calisto-hash — APIs nativas `Calisto::Hash` (Fase P do roadmap).
//!
//! `Calisto::Hash.sha256` / `Calisto::Hash.blake3` implementados em Rust e
//! registrados na VM embutida do daemon (rb_define_singleton_method via
//! calisto-ruby). Sao o equivalente do `Bun.hash`/`Bun.CryptoHasher` —
//! provam o mecanismo de extensao nativa (metodos C no boot do daemon,
//! herdados pelos children do fork) sem gem nenhuma. Zero deps.
//!
//! Os metodos rodam como C extensions: assinatura `(argc, argv, self)` e
//! longjmp via rb_exc_raise em caso de erro (protegido pelo dispatch da VM).
//! O `Ruby` da VM vive num static — o daemon e single-VM (uma libruby por
//! processo), inicializado em `register()` no boot.

mod blake3;
mod sha256;
#[cfg(target_arch = "x86_64")]
mod sha256_ni;

use calisto_ruby::{Ruby, VALUE};
use std::ffi::{c_int, c_void};
use std::sync::Mutex;

static VM: Mutex<Option<usize>> = Mutex::new(None);

fn vm() -> &'static Ruby {
    let g = VM.lock().unwrap();
    unsafe { &*(g.expect("calisto-hash: register() nao chamado") as *const Ruby) }
}

/// Registra o modulo `Calisto::Hash` com os metodos nativos na VM. Chamado
/// no boot do daemon (daemon_main), antes do accept loop — os children do
/// fork herdam os metodos registrados.
pub fn register(vm: &Ruby) -> Result<(), String> {
    *VM.lock().unwrap() = Some(vm as *const Ruby as usize);
    let n = vm.native();
    unsafe {
        let calisto = (n.rb_define_module)(c"Calisto".as_ptr());
        let hash = (n.rb_define_module_under)(calisto, c"Hash".as_ptr());
        (n.rb_define_singleton_method)(hash, c"sha256".as_ptr(), hash_sha256 as *const c_void, -1);
        (n.rb_define_singleton_method)(hash, c"blake3".as_ptr(), hash_blake3 as *const c_void, -1);
    }
    Ok(())
}

/// Hexdigest lowercase de um digest (a saida publica das APIs).
fn hexdigest(vm: &Ruby, digest: &[u8]) -> VALUE {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 64];
    for (i, b) in digest.iter().enumerate() {
        out[i * 2] = HEX[(b >> 4) as usize];
        out[i * 2 + 1] = HEX[(b & 0xf) as usize];
    }
    vm.utf8_str(&out)
}

fn hash_one_arg(argc: c_int, argv: *mut VALUE, f: fn(&[u8]) -> [u8; 32]) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(
            vm.e_arg_error(),
            &format!("wrong number of arguments (given {argc}, expected 1)"),
        );
    }
    let mut v = unsafe { *argv };
    let (ptr, len) = vm.string_bytes(&mut v);
    let digest = f(unsafe { std::slice::from_raw_parts(ptr, len) });
    hexdigest(vm, &digest)
}

unsafe extern "C" fn hash_sha256(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    hash_one_arg(argc, argv, sha256::hash)
}

unsafe extern "C" fn hash_blake3(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    hash_one_arg(argc, argv, blake3::hash)
}
