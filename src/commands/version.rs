//! calisto — version
//!
//! `calisto --version` / `-v` (Fase R): versao do calisto + a VM embutida no
//! formato do `ruby -v` (RUBY_DESCRIPTION do ruby resolvido — Fase I
//! multi-versoes: a versao do cwd, nao so o pin).

use std::process::Command;
use crate::runtime::ruby_or_err;

pub fn cmd_version() -> i32 {
    println!("calisto {}", env!("CARGO_PKG_VERSION"));
    if let Some(ruby) = ruby_or_err() {
        if let Ok(out) = Command::new(&ruby).arg("-v").output() {
            let desc = String::from_utf8_lossy(&out.stdout);
            let desc = desc.trim();
            if !desc.is_empty() {
                println!("{desc}");
            }
        }
    }
    0
}
