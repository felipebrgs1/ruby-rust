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
    // os pesados ficaram LAZY (dropados do default por causa do cold):
    // o child carrega no require on demand, o daemon nao paga ~43ms de boot
    assert!(s.contains("yaml=no"), "yaml e lazy no default: {s}");
    assert!(s.contains("net_http=no"), "net/http e lazy no default: {s}");
    assert!(s.contains("csv=no"), "csv e lazy no default: {s}");
    stop(&dir);
}

#[test]
fn default_lazy_modules_load_on_demand_in_child() {
    // O modulo pesado nao preloaded funciona: o child `require`a e carrega
    // (custo unico no child, CoW — o daemon nao polui).
    let dir = runtime_dir("preload-lazy");
    let out = run(
        &dir,
        &["run", "-e", "require \"net/http\"; require \"yaml\"; puts Net::HTTP::VERSION"],
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!String::from_utf8_lossy(&out.stdout).trim().is_empty());
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
