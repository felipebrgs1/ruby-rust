//! Fase B: preload de app — boot congelado via `calisto.toml` + fork-safe.
//!
//! O daemon da app (socket dedicado por app, como Spring/Zeus) carrega o
//! entrypoint de [run].preload no boot; cada `calisto run` e um fork desse
//! boot. Fixture `preloadapp`: boot simulado de 2s + contador — prova que o
//! boot roda UMA vez e o 2o run cai para <500ms (meta da Fase B).

use std::path::Path;
use std::time::Instant;

use common::{fixture, run_opt, runtime_dir, RunOpts};

mod common;

fn app_run(dir: &Path, app: &str, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    run_opt(
        dir,
        RunOpts {
            args,
            env,
            stdin: None,
            cwd: Some(&fixture(app)),
            timeout: 30,
        },
    )
}

fn stop_app(dir: &Path, app: &str) {
    let _ = run_opt(
        dir,
        RunOpts {
            args: &["stop"],
            env: &[],
            stdin: None,
            cwd: Some(&fixture(app)),
            timeout: 10,
        },
    );
}

#[test]
fn frozen_boot_pays_once_and_second_run_is_fast() {
    let dir = runtime_dir("appfrozen");
    let bc = dir.join("boot_count");
    let env = [("BOOT_COUNT_FILE", bc.to_str().unwrap())];

    // cold: baseline sem preload — script puro, o boot da app nao roda
    let cold = app_run(&dir, "preloadapp", &["run", "--cold", "main.rb"], &env);
    assert!(cold.status.success());
    assert!(
        !String::from_utf8_lossy(&cold.stdout).contains("app_env_loaded=true"),
        "cold nao deve ter o boot da app"
    );

    // 1o run warm: spawna o daemon da app e paga o boot (2s simulados)
    let t0 = Instant::now();
    let first = app_run(&dir, "preloadapp", &["run", "main.rb"], &env);
    assert!(first.status.success());
    let first_ms = t0.elapsed().as_millis();
    assert!(first_ms > 1500, "1o run deve pagar o boot: {first_ms}ms");
    assert_eq!(std::fs::read_to_string(&bc).unwrap().trim(), "1");

    // 2o run: fork do boot congelado — meta da Fase B (<500ms por comando)
    let t0 = Instant::now();
    let second = app_run(&dir, "preloadapp", &["run", "main.rb"], &env);
    assert!(second.status.success());
    let second_ms = t0.elapsed().as_millis();
    assert!(second_ms < 500, "2o run deve ser fork rapido: {second_ms}ms");
    assert_eq!(
        std::fs::read_to_string(&bc).unwrap().trim(),
        "1",
        "o boot nao pode re-rodar por comando"
    );
    assert!(String::from_utf8_lossy(&second.stdout).contains("app_env_loaded=true"));
    stop_app(&dir, "preloadapp");
}

#[test]
fn app_daemon_is_dedicated_and_isolated() {
    // O daemon da app tem socket proprio (apps/<hash>): convive com o daemon
    // generico sem interferencia — um script de fora da app nao re-roda o boot.
    let dir = runtime_dir("appisolated");
    let bc = dir.join("boot_count");
    let env = [("BOOT_COUNT_FILE", bc.to_str().unwrap())];

    let in_app = app_run(&dir, "preloadapp", &["run", "main.rb"], &env);
    assert!(in_app.status.success());
    assert_eq!(std::fs::read_to_string(&bc).unwrap().trim(), "1");

    // de fora da app (hello.rb, cwd sem calisto.toml): daemon generico, sem boot
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", fixture("hello.rb").to_str().unwrap()],
            env: &[],
            stdin: None,
            cwd: None,
            timeout: 30,
        },
    );
    assert!(out.status.success());
    assert_eq!(std::fs::read_to_string(&bc).unwrap().trim(), "1", "boot intacto");
    stop_app(&dir, "preloadapp");
}

#[test]
fn invalid_app_config_fails_clearly() {
    let dir = runtime_dir("appbad");
    let project = dir.join("badapp");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join("script.rb"), "puts :ok\n").unwrap();

    // entrypoint inexistente -> daemon aborta com mensagem do preload
    std::fs::write(
        project.join("calisto.toml"),
        "[run]\npreload = \"config/missing.rb\"\n",
    )
    .unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "script.rb"],
            env: &[],
            stdin: None,
            cwd: Some(&project),
            timeout: 30,
        },
    );
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("missing.rb") && err.contains("preload"),
        "erro deve apontar o entrypoint: {err}"
    );

    // sintaxe invalida -> erro de parse aponta linha
    std::fs::write(project.join("calisto.toml"), "[run]\nprelaod = \"x\"\n").unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "script.rb"],
            env: &[],
            stdin: None,
            cwd: Some(&project),
            timeout: 30,
        },
    );
    assert_ne!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("calisto.toml:2"),
        "erro de parse deve apontar a linha: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn rails_runner_frozen_boot() {
    // Golden test do marco: `rails runner` em app Rails 8 + sqlite3 com boot
    // congelado. Gated: exige `bundle install` previo no fixture (rede).
    let dir = runtime_dir("railsapp");
    let app = fixture("railsapp");
    if !rails_bundle_installed(&app) {
        eprintln!("SKIP rails_runner_frozen_boot: rode `bundle install` em test/fixtures/railsapp");
        return;
    }

    let runner = ["run", "bin/rails", "runner", "puts \"RAILS_OK\""];
    let first = app_run(&dir, "railsapp", &runner, &[]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );

    // marco: 2o comando <500ms (boot de Rails >=2s sem preload)
    let t0 = Instant::now();
    let second = app_run(&dir, "railsapp", &runner, &[]);
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    let ms = t0.elapsed().as_millis();
    assert!(ms < 500, "rails runner warm deve ser <500ms (meta Fase B): {ms}ms");
    assert_eq!(String::from_utf8_lossy(&second.stdout).trim(), "RAILS_OK");

    // fork-safe: query no sqlite reconecta no child (conexoes nao sao herdadas)
    let db = app_run(
        &dir,
        "railsapp",
        &[
            "run",
            "bin/rails",
            "runner",
            "puts ActiveRecord::Base.connection.select_value(\"SELECT 1\")",
        ],
        &[],
    );
    assert!(db.status.success(), "{}", String::from_utf8_lossy(&db.stderr));
    assert_eq!(String::from_utf8_lossy(&db.stdout).trim(), "1");
    stop_app(&dir, "railsapp");
}

/// Gems do fixture Rails instaladas? (bundle install com rede; senão skip).
fn rails_bundle_installed(app: &Path) -> bool {
    let vendor_bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/current/bin");
    std::process::Command::new(vendor_bin.join("bundle"))
        .env("PATH", format!("{}:{}", vendor_bin.display(), env!("PATH")))
        .arg("check")
        .current_dir(app)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn rails_dev_server_serves_http() {
    // Fase C: `bin/rails server` roda como child do fork do daemon da app;
    // GET /up -> 200 com o app ja bootado (<500ms do spawn a resposta).
    let dir = runtime_dir("railsserver");
    let app = fixture("railsapp");
    if !rails_bundle_installed(&app) {
        eprintln!("SKIP rails_dev_server_serves_http: rode `bundle install` em test/fixtures/railsapp");
        return;
    }

    // aquece o daemon da app (o boot em si ja e medido no runner test)
    let warm = app_run(&dir, "railsapp", &["run", "bin/rails", "runner", "puts 1"], &[]);
    assert!(warm.status.success(), "{}", String::from_utf8_lossy(&warm.stderr));

    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let t0 = Instant::now();
    let mut child = common::calisto(&dir)
        .arg("run")
        .arg("bin/rails")
        .args(["server", "-p"])
        .arg(port.to_string())
        .args(["-b", "127.0.0.1"])
        .current_dir(&app)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn rails server");

    use std::io::{Read, Write};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let body = loop {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut s) => {
                s.write_all(b"GET /up HTTP/1.1\r\nHost: calisto.test\r\nConnection: close\r\n\r\n")
                    .unwrap();
                let mut resp = String::new();
                s.read_to_string(&mut resp).unwrap();
                break resp;
            }
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20))
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("rails server nao subiu: {e}");
            }
        }
    };
    let ms = t0.elapsed().as_millis();
    assert!(ms < 500, "server deve responder em <500ms (meta Fase C): {ms}ms");
    assert!(
        body.contains("200") && body.contains("green"),
        "health /up deve responder 200 up: {body}"
    );

    let _ = child.kill();
    let _ = child.wait();
    stop_app(&dir, "railsapp");
}

#[test]
fn rails_console_runs_in_app_context() {
    // Fase C: console (IRB) roda no contexto da app pre-carregada via stdin.
    let dir = runtime_dir("railsconsole");
    let app = fixture("railsapp");
    if !rails_bundle_installed(&app) {
        eprintln!("SKIP rails_console_runs_in_app_context: rode `bundle install` em test/fixtures/railsapp");
        return;
    }

    use std::io::{Read, Write};
    let mut child = common::calisto(&dir)
        .args(["run", "bin/rails", "console"])
        .current_dir(&app)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn rails console");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"puts 1+1\nputs Rails.env\nexit\n")
        .unwrap();
    drop(child.stdin.take());

    // le o stdout com timeout (console que nao sai = fail, nao pendura)
    let mut stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        stdout.read_to_string(&mut out).unwrap();
        tx.send(out).unwrap();
    });
    let out = match rx.recv_timeout(std::time::Duration::from_secs(20)) {
        Ok(o) => o,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            panic!("console nao terminou em 20s");
        }
    };
    let status = child.wait().unwrap();
    assert!(status.success());
    assert!(
        out.contains("\n2\n") && out.contains("development"),
        "console deve rodar no contexto da app: {out}"
    );
    stop_app(&dir, "railsapp");
}
