//! Fase R — paridade de CLI do interpretador: flags ruby do `run`.
//!
//! -I/-r/-w/-W/-c/-E aplicadas no child do fork (via CALISTO_RUN_FLAGS no
//! env_blob — sem mudanca de protocolo) e no --cold (argv do ruby direto).
//! Invariante central: cold_and_warm_agree com cada flag — se o ruby puro e
//! o calisto divergirem, e bug do calisto. `-v`/`--version` no topo e no run.

use std::fs;
use std::path::PathBuf;

mod common;

use common::{run, run_opt, runtime_dir, RunOpts};

/// Diretorio de teste com lib + scripts (escrito pelo proprio teste — as
/// fixtures ficam no temp dir do runtime, isoladas por teste).
fn setup(dir: &PathBuf) {
    fs::create_dir_all(dir.join("lib")).unwrap();
    fs::write(
        dir.join("lib/mylib.rb"),
        "# frozen_string_literal: true\nputs \"LOADED-mylib\"\nmodule MyLib\n  def self.greet = \"from-mylib\"\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("need.rb"),
        "# frozen_string_literal: true\nrequire \"mylib\"\nputs MyLib.greet\n",
    )
    .unwrap();
    fs::write(
        dir.join("warnme.rb"),
        "# frozen_string_literal: true\nx = 1\nputs \"hi\"\n",
    )
    .unwrap();
    fs::write(dir.join("shh.rb"), "# frozen_string_literal: true\nwarn \"shh\"\n").unwrap();
    fs::write(dir.join("ok.rb"), "# frozen_string_literal: true\nputs 1\n").unwrap();
    fs::write(dir.join("bad.rb"), "# frozen_string_literal: true\ndef broken(\n").unwrap();
    fs::write(
        dir.join("end.rb"),
        "# frozen_string_literal: true\nputs 1\n__END__\ndata here\n",
    )
    .unwrap();
}

#[test]
fn i_flag_loads_from_lib_dir() {
    let dir = runtime_dir("riflags");
    setup(&dir);
    let script = dir.join("need.rb");
    let s = script.to_str().unwrap();
    let warm = run(&dir, &["run", "-I", dir.join("lib").to_str().unwrap(), s]);
    let cold = run(&dir, &["run", "--cold", "-I", dir.join("lib").to_str().unwrap(), s]);
    assert!(warm.status.success() && cold.status.success());
    assert_eq!(warm.stdout, cold.stdout);
    assert_eq!(warm.stdout, b"LOADED-mylib\nfrom-mylib\n");
}

#[test]
fn i_attached_form_matches_ruby() {
    let dir = runtime_dir("riattached");
    setup(&dir);
    // ruby aceita -Ilib anexado (como -Idir) — mesmo parse no calisto
    let out = run(
        &dir,
        &["run", &format!("-I{}", dir.join("lib").display()), dir.join("need.rb").to_str().unwrap()],
    );
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("from-mylib"));
}

#[test]
fn r_flag_requires_lib() {
    let dir = runtime_dir("rflag");
    let warm = run(&dir, &["run", "-r", "json", "-e", "puts JSON.parse(%q({\"a\":1}))[\"a\"]"]);
    let cold = run(&dir, &["run", "--cold", "-r", "json", "-e", "puts JSON.parse(%q({\"a\":1}))[\"a\"]"]);
    assert!(warm.status.success() && cold.status.success());
    assert_eq!(warm.stdout, cold.stdout);
    assert_eq!(warm.stdout, b"1\n");
}

#[test]
fn r_with_i_combines() {
    let dir = runtime_dir("rwithi");
    setup(&dir);
    let lib = dir.join("lib").to_str().unwrap().to_string();
    let warm = run(&dir, &["run", "-I", &lib, "-r", "mylib", "-e", "puts MyLib.greet"]);
    let cold = run(&dir, &["run", "--cold", "-I", &lib, "-r", "mylib", "-e", "puts MyLib.greet"]);
    assert!(warm.status.success() && cold.status.success());
    assert_eq!(warm.stdout, cold.stdout);
    assert_eq!(warm.stdout, b"LOADED-mylib\nfrom-mylib\n");
}

#[test]
fn r_missing_lib_fails_like_ruby() {
    let dir = runtime_dir("rmissing");
    let warm = run(&dir, &["run", "-r", "calisto_no_such_lib_xyz", "-e", "puts 1"]);
    let cold = run(&dir, &["run", "--cold", "-r", "calisto_no_such_lib_xyz", "-e", "puts 1"]);
    assert_eq!(warm.status.code(), Some(1));
    assert_eq!(cold.status.code(), Some(1));
    let err = String::from_utf8_lossy(&warm.stderr);
    assert!(err.contains("calisto_no_such_lib_xyz"), "LoadError deve citar a lib: {err}");
}

#[test]
fn w_flag_shows_warnings() {
    let dir = runtime_dir("wflag");
    setup(&dir);
    let script = dir.join("warnme.rb");
    let s = script.to_str().unwrap();
    let with = run(&dir, &["run", "-w", s]);
    let without = run(&dir, &["run", s]);
    assert!(with.status.success() && without.status.success());
    let werr = String::from_utf8_lossy(&with.stderr);
    assert!(werr.contains("unused variable"), "-w deve ligar warnings: {werr}");
    assert!(!String::from_utf8_lossy(&without.stderr).contains("unused variable"));
    // paridade cold/warm com -w
    let cold = run(&dir, &["run", "--cold", "-w", s]);
    assert!(cold.status.success());
    assert_eq!(cold.stdout, with.stdout);
}

#[test]
fn W0_silences_and_W2_shows() {
    let dir = runtime_dir("Wflag");
    setup(&dir);
    let script = dir.join("shh.rb");
    let s = script.to_str().unwrap();
    let w0 = run(&dir, &["run", "-W0", s]);
    let w2 = run(&dir, &["run", "-W2", s]);
    assert!(w0.status.success() && w2.status.success());
    assert!(w0.stderr.is_empty(), "-W0 deve silenciar warn: {:?}", w0.stderr);
    assert!(String::from_utf8_lossy(&w2.stderr).contains("shh"));
    // paridade cold/warm
    let cold = run(&dir, &["run", "--cold", "-W0", s]);
    assert!(cold.stderr.is_empty());
}

#[test]
fn c_syntax_check_ok() {
    let dir = runtime_dir("cflagok");
    setup(&dir);
    let script = dir.join("ok.rb");
    let s = script.to_str().unwrap();
    let warm = run(&dir, &["run", "-c", s]);
    let cold = run(&dir, &["run", "--cold", "-c", s]);
    assert_eq!(warm.status.code(), Some(0));
    assert_eq!(cold.status.code(), Some(0));
    assert_eq!(warm.stdout, b"Syntax OK\n");
    assert_eq!(warm.stdout, cold.stdout);
    // -c com -e
    let e = run(&dir, &["run", "-c", "-e", "puts 1"]);
    assert_eq!(e.status.code(), Some(0));
    assert_eq!(e.stdout, b"Syntax OK\n");
    // -c nao executa o script
    let noexec = run(&dir, &["run", "-c", "-e", "exit 42"]);
    assert_eq!(noexec.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&noexec.stdout).contains("42"));
}

#[test]
fn c_syntax_check_error() {
    let dir = runtime_dir("cflagerr");
    setup(&dir);
    let script = dir.join("bad.rb");
    let s = script.to_str().unwrap();
    let warm = run(&dir, &["run", "-c", s]);
    let cold = run(&dir, &["run", "--cold", "-c", s]);
    assert_eq!(warm.status.code(), Some(1));
    assert_eq!(cold.status.code(), Some(1));
    let werr = String::from_utf8_lossy(&warm.stderr);
    assert!(werr.contains("syntax errors found"), "msg de sintaxe: {werr}");
    assert!(werr.contains("bad.rb"), "arquivo citado: {werr}");
    // -c -e com erro
    let e = run(&dir, &["run", "-c", "-e", "1 +"]);
    assert_eq!(e.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&e.stderr).contains("syntax errors found"));
}

#[test]
fn c_syntax_check_accepts_end_marker() {
    let dir = runtime_dir("cend");
    setup(&dir);
    let script = dir.join("end.rb");
    let s = script.to_str().unwrap();
    let warm = run(&dir, &["run", "-c", s]);
    let cold = run(&dir, &["run", "--cold", "-c", s]);
    assert_eq!(warm.status.code(), Some(0));
    assert_eq!(cold.status.code(), Some(0));
    assert_eq!(warm.stdout, b"Syntax OK\n");
}

#[test]
fn E_sets_default_encoding() {
    let dir = runtime_dir("Eflag");
    let warm = run(&dir, &["run", "-E", "UTF-8", "-e", "puts Encoding.default_external.name"]);
    let cold = run(&dir, &["run", "--cold", "-E", "UTF-8", "-e", "puts Encoding.default_external.name"]);
    assert!(warm.status.success() && cold.status.success());
    assert_eq!(warm.stdout, b"UTF-8\n");
    assert_eq!(warm.stdout, cold.stdout);
    // ext:int — default_internal tambem
    let both = run(
        &dir,
        &["run", "-E", "UTF-8:UTF-16", "-e", "print [Encoding.default_external.name, Encoding.default_internal.name].join(\",\")"],
    );
    assert!(both.status.success());
    assert_eq!(both.stdout, b"UTF-8,UTF-16");
}

#[test]
fn E_unknown_encoding_fails() {
    let dir = runtime_dir("Ebogus");
    let warm = run(&dir, &["run", "-E", "bogus-enc", "-e", "puts 1"]);
    let cold = run(&dir, &["run", "--cold", "-E", "bogus-enc", "-e", "puts 1"]);
    assert_eq!(warm.status.code(), Some(1));
    assert_eq!(cold.status.code(), Some(1));
    let err = String::from_utf8_lossy(&warm.stderr);
    assert!(err.contains("unknown encoding name"), "msg de encoding: {err}");
}

#[test]
fn double_dash_ends_flag_parsing() {
    let dir = runtime_dir("ddash");
    setup(&dir);
    // script com nome que comeca com "-" so roda com `--` (como o ruby)
    fs::write(dir.join("-weird.rb"), "# frozen_string_literal: true\nputs \"weird-ok\"\n").unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "--", "-weird.rb"],
            env: &[],
            stdin: None,
            cwd: Some(&dir),
            timeout: 30,
        },
    );
    assert!(out.status.success());
    assert_eq!(out.stdout, b"weird-ok\n");
}

#[test]
fn version_flags_print_ruby_description() {
    let dir = runtime_dir("version");
    let v = run(&dir, &["--version"]);
    assert!(v.status.success());
    let out = String::from_utf8_lossy(&v.stdout);
    assert!(out.contains("calisto"), "versao do calisto: {out}");
    assert!(out.contains("ruby 3.4"), "descricao da VM: {out}");

    let sv = run(&dir, &["-v"]);
    assert!(sv.status.success());
    assert!(String::from_utf8_lossy(&sv.stdout).contains("ruby 3.4"));

    // `calisto run -v` == `ruby -v` (paridade): descricao sozinha, exit 0,
    // sem rodar script
    let rv = run(&dir, &["run", "-v"]);
    assert!(rv.status.success());
    let rout = String::from_utf8_lossy(&rv.stdout);
    assert!(rout.contains("ruby 3.4"), "run -v: {rout}");
    assert!(!rout.contains("calisto "), "run -v nao inclui a versao do calisto: {rout}");
    // -v ganha de um script que rodaria
    let rv2 = run(&dir, &["run", "-v", "-e", "exit 42"]);
    assert_eq!(rv2.status.code(), Some(0));
}

#[test]
fn ruby_flags_rejected_for_toml_scripts() {
    let dir = runtime_dir("scriptsflags");
    fs::write(dir.join("calisto.toml"), "[scripts]\ndev = \"echo dev\"\n").unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "-I", "lib", "dev"],
            env: &[],
            stdin: None,
            cwd: Some(&dir),
            timeout: 30,
        },
    );
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("nao se aplicam a scripts"),
        "erro claro para flags + script do calisto.toml: {err}"
    );
}

#[test]
fn flags_with_app_daemon() {
    // flags tambem valem no daemon da app (boot congelado) — o child aplica
    // depois do Bundler.setup, como no daemon generico
    let dir = runtime_dir("appflags");
    setup(&dir);
    fs::write(
        dir.join("calisto.toml"),
        format!("[run]\npreload = \"entry.rb\"\n"),
    )
    .unwrap();
    fs::write(dir.join("entry.rb"), "# frozen_string_literal: true\nputs \"booted\"\n").unwrap();
    let lib = dir.join("lib").to_str().unwrap().to_string();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "-I", &lib, "-r", "mylib", "-e", "puts MyLib.greet"],
            env: &[],
            stdin: None,
            cwd: Some(&dir),
            timeout: 30,
        },
    );
    assert!(out.status.success());
    assert_eq!(out.stdout, b"booted\nLOADED-mylib\nfrom-mylib\n");
}
