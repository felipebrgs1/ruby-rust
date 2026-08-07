//! calisto — deps
//!
//! calisto add/remove/lock (wrapper do bundle).
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — commands/deps (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use crate::appconfig::*;
use crate::runtime::*;







// ---- Fase K: deps (calisto add/remove/lock) ---------------------------------

/// `calisto add|remove|lock` — wrapper fino do bundle (decisao da Fase A:
/// nada de instalador proprio), com o ruby da versao certa (Fase I) e cwd na
/// raiz do projeto (walk-up do Gemfile, como o resto do calisto). Args passam
/// direto ao `bundle <sub>`. `CALISTO_BUNDLE` (testes) troca o binario do
/// bundle; o client exporta `CALISTO_BUNDLE_RUBY` (ruby resolvido) e prefixa
/// o PATH com o bin dir do ruby (trap do restart do bundler: lock que pina
/// outro bundler re-executa via shebang e precisa de ruby no PATH).
pub fn cmd_bundle_wrapper(sub: &str, args: &[String]) -> i32 {
    let gemfile = env::var_os("BUNDLE_GEMFILE")
        .map(PathBuf::from)
        .or_else(|| find_in_parents("Gemfile"));
    let Some(gemfile) = gemfile else {
        eprintln!(
            "calisto: {sub}: nenhum Gemfile encontrado (subindo do cwd); \
             crie um Gemfile ou rode `bundle init`"
        );
        return 1;
    };
    let Some(root) = gemfile.parent().map(Path::to_path_buf) else {
        eprintln!("calisto: {sub}: BUNDLE_GEMFILE invalido: {}", gemfile.display());
        return 1;
    };
    let Some(ruby) = ruby_or_err() else {
        return 1;
    };
    let bin = ruby.parent().unwrap_or(Path::new("."));
    let path = format!("{}:{}", bin.display(), env::var("PATH").unwrap_or_default());
    let mut cmd = match env::var_os("CALISTO_BUNDLE") {
        Some(b) => {
            let mut c = Command::new(b);
            c.arg(sub);
            c
        }
        None => {
            // `ruby -S bundle`: roda o bundler do MESMO ruby (versao da app)
            // sem depender de shebang; -S procura no PATH (prefixado acima).
            let mut c = Command::new(&ruby);
            c.arg("-S").arg("bundle").arg(sub);
            c
        }
    };
    cmd.args(args)
        .current_dir(&root)
        .env("BUNDLE_GEMFILE", &gemfile)
        .env("CALISTO_BUNDLE_RUBY", &ruby)
        .env("PATH", path)
        .stdin(Stdio::inherit());
    match cmd.status() {
        // stdio herdado: o bundle mostra o progresso ao vivo
        Ok(st) => st.code().unwrap_or(1),
        Err(e) => {
            eprintln!("calisto: {sub}: cannot run bundle: {e}");
            1
        }
    }
}
