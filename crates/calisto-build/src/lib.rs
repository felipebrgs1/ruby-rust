//! calisto-build: empacota apps Ruby stdlib-only num arquivo unico.
//!
//! O trabalho pesado roda dentro do CRuby pinado (build.rb, com Ripper =
//! o lexer real do Ruby): analisa requires estaticos, coleta os arquivos do
//! projeto sob a raiz, e emite um bundle self-contained com um loader que
//! intercepta `require`/`require_relative` em runtime.
//!
//! Limites da v1: requires dinamicos nao sao embutidos (warning no build);
//! assets (arquivos nao-Ruby) ficam externos; C extensions (.so) nao embutidas.

use std::path::Path;
use std::process::{Command, Stdio};

const BUILD_RB: &str = include_str!("build.rb");

pub struct BundleStats {
    pub files: usize,
}

/// Roda o bundler: `ruby build.rb <entry> <out> <root>`.
/// O bundler imprime `BUNDLED <n>` no stdout; warnings vao ao stderr ao vivo.
pub fn bundle(ruby: &Path, entry: &Path, out: &Path, root: &Path) -> Result<BundleStats, String> {
    let dir = std::env::temp_dir().join(format!("calisto-build-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create temp dir: {e}"))?;
    let rb = dir.join("build.rb");
    std::fs::write(&rb, BUILD_RB).map_err(|e| format!("cannot write bundler: {e}"))?;

    let child = Command::new(ruby)
        .arg(&rb)
        .arg(entry)
        .arg(out)
        .arg(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot run bundler with {}: {e}", ruby.display()))?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("cannot wait on bundler: {e}"))?;
    if !output.status.success() {
        return Err(format!("bundler falhou (exit {:?})", output.status.code()));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let files = stdout
        .lines()
        .find_map(|l| l.strip_prefix("BUNDLED "))
        .and_then(|n| n.trim().parse().ok())
        .ok_or_else(|| format!("saida inesperada do bundler: {stdout}"))?;
    Ok(BundleStats { files })
}
