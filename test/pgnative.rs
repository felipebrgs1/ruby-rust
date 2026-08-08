//! Calisto::PG — camada de compatibilidade com a gem `pg` (nativo Rust
//! sobre libpq). Gated em `CALISTO_TEST_PG` (connstring de um postgres
//! vivo — ex.: `CALISTO_TEST_PG="host=127.0.0.1 port=5432 user=.. dbname=.."`);
//! sem a var o teste skipa com aviso (a suíte não depende de postgres).
//!
//! Paridade cold/warm: o dir `gems/` (com o shim `pg.rb`) SÓ é injetado no
//! daemon/child — em `--cold` o `require "pg"` resolve a gem real (que nos
//! goldens maybe/chatwoot vive no bundle da app). O teste
//! `pg_gems_dir_only_in_warm_load_path` cobre essa invariante de mecanismo;
//! a paridade semântica end-to-end é validada pelos goldens (realapps.rs).

mod common;

use common::{run, run_opt, RunOpts};

fn pg_conn() -> Option<String> {
    std::env::var("CALISTO_TEST_PG").ok().filter(|s| !s.is_empty())
}

#[test]
fn pg_native_exec_params_prepared_and_values() {
    let Some(conn) = pg_conn() else {
        eprintln!("skipping pgnative: CALISTO_TEST_PG nao definido (postgres vivo necessario)");
        return;
    };
    let dir = common::runtime_dir("pgnative");
    let script = common::fixture("pgcheck.rb");
    let script = script.to_str().unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", script],
            env: &[("CALISTO_TEST_PG", &conn)],
            stdin: None,
            cwd: None,
            timeout: 60,
        },
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "pgcheck falhou:\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(stdout.contains("PG_CHECK_OK"), "stdout={stdout}\nstderr={stderr}");
    // 2o run quente: mesmo daemon, mesmo resultado (boot do daemon ja pago)
    let out2 = run_opt(
        &dir,
        RunOpts {
            args: &["run", script],
            env: &[("CALISTO_TEST_PG", &conn)],
            stdin: None,
            cwd: None,
            timeout: 60,
        },
    );
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(
        out2.status.success() && stdout2.contains("PG_CHECK_OK"),
        "2o run falhou:\nstdout={stdout2}\nstderr={}",
        String::from_utf8_lossy(&out2.stderr)
    );
}

#[test]
fn pg_decoders_by_oid() {
    let Some(conn) = pg_conn() else {
        eprintln!("skipping pgnative: CALISTO_TEST_PG nao definido (postgres vivo necessario)");
        return;
    };
    let dir = common::runtime_dir("pgnative-decode");
    let script = common::fixture("pgdecoders.rb");
    let script = script.to_str().unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", script],
            env: &[("CALISTO_TEST_PG", &conn)],
            stdin: None,
            cwd: None,
            timeout: 60,
        },
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "pgdecoders falhou:\nstdout={stdout}\nstderr={stderr}"
    );
    assert!(stdout.contains("PG_DECODE_OK"), "stdout={stdout}\nstderr={stderr}");
}

#[test]
fn pg_gems_dir_only_in_warm_load_path() {
    let dir = common::runtime_dir("pgnative-lp");
    let probe = "puts $LOAD_PATH.any? { |p| p.end_with?(\"/gems\") }";
    // warm: o child injeta o dir gems/ (shim pg.rb sombreando a gem)
    let warm = run(&dir, &["run", "-e", probe]);
    let warm_out = String::from_utf8_lossy(&warm.stdout).trim().to_string();
    assert!(
        warm.status.success() && warm_out == "true",
        "warm deveria ter gems/ no load path: stdout={warm_out} stderr={}",
        String::from_utf8_lossy(&warm.stderr)
    );
    // cold: -I so injeta o dir nativo (calisto/*), NUNCA o gems/ — o
    // require \"pg\" resolve a gem real (paridade cold/warm)
    let cold = run(&dir, &["run", "--cold", "-e", probe]);
    let cold_out = String::from_utf8_lossy(&cold.stdout).trim().to_string();
    assert!(
        cold.status.success() && cold_out == "false",
        "cold nao deveria ter gems/ no load path: stdout={cold_out} stderr={}",
        String::from_utf8_lossy(&cold.stderr)
    );
}
