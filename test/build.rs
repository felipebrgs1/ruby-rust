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
