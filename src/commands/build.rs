//! calisto — build
//!
//! calisto build (bundler Ripper).
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — commands/build (extraido de src/main.rs na reorg do CLI).

use std::path::{Path, PathBuf};
use crate::runtime::*;







pub fn cmd_build(args: &[String]) -> i32 {
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
