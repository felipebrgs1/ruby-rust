//! Fase D — escada de apps reais (golden tests gated em infra externa).
//!
//! Degrau 4: Maybe Finance (Rails 7.2 + Sidekiq + Postgres + Redis) — valida
//! o modelo fork-safe com jobs + threads: o worker Sidekiq roda como child do
//! fork do daemon da app pre-carregada, conecta no Redis e processa o job.
//!
//! Gated em 3 pré-requisitos (senão skipa com aviso):
//!   1. checkout do Maybe em test/fixtures/maybe (git clone --depth 1
//!      https://github.com/maybe-finance/maybe.git)
//!   2. bundle install (precisa de libpq/libyaml p/ pg/psych — o fixture usa
//!      vendor/pgprefix + vendor/src/yaml-0.2.5 do build-ruby.sh)
//!   3. postgres:16 + redis:7 via `docker compose -f compose.calisto.yml up -d`
//!      (o teste sobe sozinho quando há docker)

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use common::{bundle_check, calisto, fixture, run_opt, runtime_dir, RunOpts};

mod common;

fn app_run(dir: &Path, app: &str, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    run_opt(
        dir,
        RunOpts {
            args,
            env,
            stdin: None,
            cwd: Some(&fixture(app)),
            timeout: 60,
        },
    )
}

fn maybe_env(dir: &Path) -> Vec<(String, String)> {
    vec![
        ("SELF_HOSTED".into(), "true".into()),
        (
            "SECRET_KEY_BASE".into(),
            format!("calisto-maybe-test-{:016x}", std::process::id()),
        ),
        ("DB_HOST".into(), "127.0.0.1".into()),
        ("DB_PORT".into(), "5433".into()),
        ("POSTGRES_USER".into(), "maybe_user".into()),
        ("POSTGRES_PASSWORD".into(), "maybe_password".into()),
        ("POSTGRES_DB".into(), "maybe_development".into()),
        ("REDIS_URL".into(), "redis://127.0.0.1:6380/1".into()),
        ("PROBE_OUT".into(), dir.join("probe_out").to_string_lossy().into_owned()),
    ]
}

fn port_open(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

fn wait_port(port: u16, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if port_open(port) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

#[test]
fn maybe_sidekiq_job_in_forked_worker() {
    let dir = runtime_dir("maybe");
    let app = fixture("maybe");
    let env = maybe_env(&dir);
    let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    // gate 1: checkout do Maybe
    if !app.join("calisto.toml").is_file() {
        eprintln!(
            "SKIP maybe_sidekiq_job_in_forked_worker: checkout Maybe ausente — \
             `git clone --depth 1 https://github.com/maybe-finance/maybe.git test/fixtures/maybe`"
        );
        return;
    }
    // gate 2: bundle
    if !bundle_check(&app) {
        eprintln!(
            "SKIP maybe_sidekiq_job_in_forked_worker: rode `bundle install` em test/fixtures/maybe \
             (pg/psych precisam de libpq/libyaml)"
        );
        return;
    }
    // gate 3: postgres + redis (sobe o compose do fixture via docker)
    if !port_open(5433) || !port_open(6380) {
        let up = std::process::Command::new("docker")
            .args(["compose", "-f", "compose.calisto.yml", "up", "-d", "--wait"])
            .current_dir(&app)
            .output();
        match up {
            Ok(o) if o.status.success() => {}
            _ => {
                eprintln!(
                    "SKIP maybe_sidekiq_job_in_forked_worker: suba a infra — \
                     `docker compose -f test/fixtures/maybe/compose.calisto.yml up -d`"
                );
                return;
            }
        }
    }
    if !wait_port(5433, 30) || !wait_port(6380, 30) {
        eprintln!("SKIP maybe_sidekiq_job_in_forked_worker: postgres/redis nao subiram");
        return;
    }

    // db:prepare idempotente (boota o daemon da app no caminho)
    let prep = app_run(&dir, "maybe", &["run", "bin/rails", "db:prepare"], &env_refs);
    assert!(
        prep.status.success(),
        "db:prepare: {}",
        String::from_utf8_lossy(&prep.stderr)
    );

    // marco da escada: 2o comando (fork do boot congelado) <500ms
    let t0 = Instant::now();
    let boot = app_run(&dir, "maybe", &["run", "bin/rails", "runner", "puts Rails.env"], &env_refs);
    assert!(boot.status.success(), "{}", String::from_utf8_lossy(&boot.stderr));
    assert!(
        t0.elapsed().as_millis() < 500,
        "2o comando deve ser <500ms (boot de Rails >=2s): {}ms",
        t0.elapsed().as_millis()
    );
    assert_eq!(String::from_utf8_lossy(&boot.stdout).trim(), "development");

    // enfileira o job de prova no Redis (limpa a fila antes p/ determinismo)
    let push = app_run(
        &dir,
        "maybe",
        &[
            "run",
            "bin/rails",
            "runner",
            "Sidekiq::Queue.new(\"default\").clear; CalistoProbeJob.perform_later(\"probe-done-42\")",
        ],
        &env_refs,
    );
    assert!(push.status.success(), "{}", String::from_utf8_lossy(&push.stderr));

    // spawna o worker Sidekiq como child do fork e espera o job rodar
    let probe = dir.join("probe_out");
    let _ = std::fs::remove_file(&probe);
    let mut child = calisto(&dir)
        .args(["run", "bin/sidekiq"])
        .current_dir(&app)
        .envs(env.clone())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sidekiq");
    let deadline = Instant::now() + Duration::from_secs(40);
    let processed = loop {
        if let Ok(content) = std::fs::read_to_string(&probe) {
            if content.trim() == "probe-done-42" {
                break true;
            }
        }
        match child.try_wait().unwrap() {
            Some(st) => {
                let _ = child.wait();
                panic!("sidekiq morreu antes do job: {st}");
            }
            None => {}
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("job nao processado em 40s");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(processed, "job deve ser processado pelo worker forked");
    let _ = child.kill();
    let _ = child.wait();

    // smoke HTTP: dev server + GET /sessions/new (rota de login real) -> 200
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut server = calisto(&dir)
        .arg("run")
        .arg("bin/rails")
        .args(["server", "-p"])
        .arg(port.to_string())
        .args(["-b", "127.0.0.1"])
        .current_dir(&app)
        .envs(env.clone())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn rails server");
    use std::io::{Read, Write};
    let deadline = Instant::now() + Duration::from_secs(30);
    let resp = loop {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut s) => {
                s.write_all(b"GET /sessions/new HTTP/1.1\r\nHost: maybe.test\r\nConnection: close\r\n\r\n")
                    .unwrap();
                let mut r = String::new();
                s.read_to_string(&mut r).unwrap();
                break r;
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                let _ = server.kill();
                let _ = server.wait();
                panic!("rails server do Maybe nao subiu: {e}");
            }
        }
    };
    assert!(
        resp.contains(" 200 "),
        "GET /sessions/new deve responder 200: {}",
        resp.lines().next().unwrap_or("")
    );
    let _ = server.kill();
    let _ = server.wait();

    let _ = run_opt(
        &dir,
        RunOpts {
            args: &["stop"],
            env: &[],
            stdin: None,
            cwd: Some(&app),
            timeout: 10,
        },
    );
}
