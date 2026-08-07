//! `calisto build`: bundle self-contained + paridade com os fontes originais.

mod common;

use common::*;
use std::path::{Path, PathBuf};
use std::process::Command;

fn vendor_ruby() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/current/bin/ruby")
}

fn ruby_run(ruby: &Path, script: &Path, args: &[&str]) -> std::process::Output {
    Command::new(ruby)
        .arg(script)
        .args(args)
        .output()
        .expect("rodar ruby")
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

/// Copia fixtures para um temp dir, roda `calisto build`, renomeia os fontes
/// (prova que o bundle e self-contained) e roda o bundle com o ruby pinado.
fn build_and_run_bundled(entry_name: &str, args: &[&str]) -> (std::process::Output, PathBuf) {
    let dir = runtime_dir("build");
    let src = dir.join("src");
    copy_tree(&fixture("buildapp"), &src);

    let entry = src.join(entry_name);
    let out = dir.join("out.rb");
    let build = run(&dir, &["build", entry.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(build.status.success(), "build falhou: {:?}", build.stderr);
    assert!(String::from_utf8_lossy(&build.stdout).contains("bundled"));
    assert!(out.is_file(), "bundle nao foi gerado");

    // fontes "somem" depois do build
    std::fs::rename(&src, &dir.join("gone")).unwrap();

    let bundled = ruby_run(&vendor_ruby(), &out, args);
    (bundled, out)
}

#[test]
fn build_require_relative_matches_original() {
    let dir = runtime_dir("build-parity");
    let src = dir.join("src");
    copy_tree(&fixture("buildapp"), &src);
    let entry = src.join("app_main.rb");

    // referencia: ruby roda nos fontes originais
    let original = ruby_run(&vendor_ruby(), &entry, &[]);
    assert!(original.status.success());

    // bundle: build, fontes renomeados, rodar o bundle
    let out = dir.join("out.rb");
    let build = run(&dir, &["build", entry.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(build.status.success(), "build falhou: {:?}", build.stderr);
    std::fs::rename(&src, &dir.join("gone")).unwrap();
    let bundled = ruby_run(&vendor_ruby(), &out, &[]);

    assert!(bundled.status.success(), "bundle falhou: {:?}", bundled.stderr);
    assert_eq!(bundled.stdout, original.stdout, "bundle deve produzir saida identica aos fontes");
}

#[test]
fn build_require_by_name_is_bundled() {
    let (bundled, _) = build_and_run_bundled("app_req.rb", &[]);
    assert!(bundled.status.success(), "bundle falhou: {:?}", bundled.stderr);
    assert!(String::from_utf8_lossy(&bundled.stdout).contains("msg: ola do bundle"));
}

#[test]
fn build_data_section_is_emulated() {
    let (bundled, _) = build_and_run_bundled("data_main.rb", &[]);
    assert!(bundled.status.success(), "bundle falhou: {:?}", bundled.stderr);
    assert_eq!(String::from_utf8_lossy(&bundled.stdout).trim(), "data: dados embutidos");
}

#[test]
fn build_preserves_file_and_dir() {
    // __FILE__/__dir__ no bundle apontam para os caminhos ORIGINAIS (mesmo
    // com os fontes renomeados) -- o eval usa o filename original.
    let (bundled, _) = build_and_run_bundled("app_main.rb", &[]);
    assert!(bundled.status.success(), "bundle falhou: {:?}", bundled.stderr);
    let s = String::from_utf8_lossy(&bundled.stdout);
    assert!(s.contains("msg: ola do bundle"));
    assert!(s.contains("app_main.rb"), "file deveria preservar o path original: {s}");
    assert!(s.contains("json: {\"a\":1}"), "stdlib json nao embutido, require delegado: {s}");
}

#[test]
fn build_bundle_runs_under_calisto() {
    // o bundle e um script ruby normal: roda tambem via calisto (warm)
    let (bundled, out) = build_and_run_bundled("data_main.rb", &[]);
    assert!(bundled.status.success());
    let via_calisto = run(&runtime_dir("build-run"), &["run", "--preload", "0", out.to_str().unwrap()]);
    assert!(via_calisto.status.success(), "bundle via calisto falhou: {:?}", via_calisto.stderr);
    assert_eq!(via_calisto.stdout, bundled.stdout);
}

#[test]
fn build_reports_bundled_count() {
    let dir = runtime_dir("build-count");
    let src = dir.join("src");
    copy_tree(&fixture("buildapp"), &src);
    let entry = src.join("app_main.rb");
    let out = dir.join("out.rb");
    let build = run(&dir, &["build", entry.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(build.status.success(), "build falhou: {:?}", build.stderr);
    // app_main.rb + lib/foo.rb embutidos; "json" (stdlib) fica de fora
    let s = String::from_utf8_lossy(&build.stdout);
    assert!(s.contains("bundled 2 arquivo(s)"), "{s}");
}

#[test]
fn compile_bundle_embeds_gems_and_runs_without_bundle() {
    // Fase F, marco: `calisto build --compile` embute as gems pure-Ruby do
    // Gemfile.lock (Sinatra + rack + rack-test + ...) e o bundle roda com
    // GEM_HOME/GEM_PATH vazios — sem rubygems/bundle no sistema.
    let dir = runtime_dir("compile");
    let app = fixture("sinatraapp");
    if !common::bundle_check(&app) {
        eprintln!("SKIP compile_bundle_embeds_gems_and_runs_without_bundle: rode `bundle install` em test/fixtures/sinatraapp");
        return;
    }
    let out = dir.join("compiled.rb");
    let build = run_opt(
        &dir,
        RunOpts {
            args: &["build", "--compile", "smoke.rb", "-o", out.to_str().unwrap(), "--root", "."],
            env: &[],
            stdin: None,
            cwd: Some(&app),
            timeout: 60,
        },
    );
    assert!(
        build.status.success(),
        "build --compile: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(String::from_utf8_lossy(&build.stdout).contains("bundled"));
    assert!(out.is_file(), "bundle nao foi gerado");

    let bundled = Command::new(vendor_ruby())
        .arg(&out)
        .env("GEM_HOME", dir.join("nogems"))
        .env("GEM_PATH", dir.join("nogems"))
        .output()
        .expect("rodar bundle sem gems");
    assert!(
        bundled.status.success(),
        "{}",
        String::from_utf8_lossy(&bundled.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&bundled.stdout).trim(),
        "HTTP 200: hello from sinatra"
    );
}

#[test]
fn compile_embeds_c_extensions_and_runs() {
    // Fase F, item C exts: gems com C extension embutem .rb + nativos (.so
    // extraido p/ tmpdir no runtime) — o bundle roda com GEM_PATH vazio
    // (puma requer 'puma/puma_http11', um .so, estaticamente).
    let dir = runtime_dir("compile-cext");
    let app = fixture("sinatraapp");
    if !common::bundle_check(&app) {
        eprintln!("SKIP compile_embeds_c_extensions_and_runs: rode `bundle install` em test/fixtures/sinatraapp");
        return;
    }
    let req = dir.join("puma_req.rb");
    std::fs::write(&req, "require \"puma\"\nputs Puma::Const::VERSION\n").unwrap();
    let out = dir.join("o.rb");
    let build = run_opt(
        &dir,
        RunOpts {
            args: &["build", "--compile", req.to_str().unwrap(), "-o", out.to_str().unwrap(), "--root", "."],
            env: &[],
            stdin: None,
            cwd: Some(&app),
            timeout: 60,
        },
    );
    assert!(
        build.status.success(),
        "build --compile: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let src = std::fs::read_to_string(&out).unwrap();
    assert!(src.contains("$calisto_native"), "bundle sem secao de nativos");
    let bundled = Command::new(vendor_ruby())
        .arg(&out)
        .env("GEM_HOME", dir.join("nogems"))
        .env("GEM_PATH", dir.join("nogems"))
        .output()
        .expect("rodar bundle sem gems");
    assert!(
        bundled.status.success(),
        "{}",
        String::from_utf8_lossy(&bundled.stderr)
    );
    let s = String::from_utf8_lossy(&bundled.stdout);
    assert!(s.trim().starts_with("8."), "Puma::Const::VERSION deveria ser 8.x: {s}");
}

#[test]
fn compile_embeds_sqlite3_c_extension() {
    // sqlite3: gem precompilada com .so no lib e require DINAMICO
    // ("sqlite3/#{RUBY_VERSION}/sqlite3_native") — coberto pelo pre-indice
    // dos nativos pelo nome canonico de require.
    let dir = runtime_dir("compile-sqlite");
    let app = fixture("railsapp");
    if !common::bundle_check(&app) {
        eprintln!("SKIP compile_embeds_sqlite3_c_extension: rode `bundle install` em test/fixtures/railsapp");
        return;
    }
    let req = dir.join("sqlite_req.rb");
    std::fs::write(&req, "require \"sqlite3\"\nputs SQLite3::SQLITE_VERSION\n").unwrap();
    let out = dir.join("o.rb");
    let build = run_opt(
        &dir,
        RunOpts {
            args: &["build", "--compile", req.to_str().unwrap(), "-o", out.to_str().unwrap(), "--root", "."],
            env: &[],
            stdin: None,
            cwd: Some(&app),
            timeout: 60,
        },
    );
    assert!(
        build.status.success(),
        "build --compile: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bundled = Command::new(vendor_ruby())
        .arg(&out)
        .env("GEM_HOME", dir.join("nogems"))
        .env("GEM_PATH", dir.join("nogems"))
        .output()
        .expect("rodar bundle sem gems");
    assert!(
        bundled.status.success(),
        "{}",
        String::from_utf8_lossy(&bundled.stderr)
    );
    let s = String::from_utf8_lossy(&bundled.stdout);
    assert!(s.trim().starts_with("3."), "SQLite3::SQLITE_VERSION: {s}");
}
