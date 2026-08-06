//! Preload do stdlib: default (batteries), desabilitado e custom.

mod common;

use common::*;

fn preload_out(dir: &std::path::Path, args: &[&str]) -> String {
    let out = run(dir, args);
    assert!(out.status.success(), "run falhou: {:?}", out.stderr);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn default_preload_loads_stdlib() {
    let dir = runtime_dir("preload-default");
    let s = preload_out(&dir, &["run", fixture("preload.rb").to_str().unwrap()]);
    assert!(s.contains("json=yes"), "default deve preloadar json: {s}");
    assert!(s.contains("yaml=yes"), "default deve preloadar yaml (psych): {s}");
    stop(&dir);
}

#[test]
fn preload_zero_disables() {
    let dir = runtime_dir("preload-off");
    let s = preload_out(&dir, &["run", "--preload", "0", fixture("preload.rb").to_str().unwrap()]);
    assert!(s.contains("json=no"), "preload 0 deve desligar tudo: {s}");
    assert!(s.contains("yaml=no"));
    stop(&dir);
}

#[test]
fn custom_preload_list() {
    let dir = runtime_dir("preload-custom");
    let s = preload_out(
        &dir,
        &["run", "--preload", "json", fixture("preload.rb").to_str().unwrap()],
    );
    assert!(s.contains("json=yes"), "{s}");
    assert!(s.contains("yaml=no"), "so json, nao yaml: {s}");
    stop(&dir);
}
