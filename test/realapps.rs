//! Fase D — escada de apps reais (golden tests gated em infra externa).
//!
//! Degrau 4: Maybe Finance (Rails 7.2 + Sidekiq + Postgres + Redis) — valida
//! o modelo fork-safe com jobs + threads: o worker Sidekiq roda como child do
//! fork do daemon da app pre-carregada, conecta no Redis e processa o job.
//! O worker sobe via `calisto exec sidekiq` (Fase G: binario da gem resolvido
//! no bundle ativo, carregado in-process no daemon quente — marco da Fase G).
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

    // spawna o worker Sidekiq como child do fork via `calisto exec sidekiq`
    // (Fase G: resolve o binario da gem no bundle ativo e carrega in-process
    // no daemon quente — o `-r` e o require path que o Sidekiq 8 exige, como
    // o bin/sidekiq do fixture) e espera o job rodar
    let probe = dir.join("probe_out");
    let _ = std::fs::remove_file(&probe);
    let mut child = calisto(&dir)
        .args(["exec", "sidekiq", "-r", app.to_str().unwrap()])
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

// ---- degrau 5: Chatwoot -----------------------------------------------------

/// HTTP request via TcpStream (sem deps): retorna (status, headers, body).
fn http_request(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: Option<&str>) -> Option<(u16, String, String)> {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: calisto.test\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    req.push_str("\r\n");
    if let Some(b) = body {
        req.push_str(b);
    }
    s.write_all(req.as_bytes()).ok()?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).ok()?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status: u16 = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|v| v.parse().ok())?;
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    Some((status, head.to_string(), body.to_string()))
}

#[test]
fn chatwoot_backend_serves_api_and_cable() {
    // Degrau 5 da escada: Chatwoot (Rails 7.1, ~155 gems) — boot + login
    // (devise_token_auth) + conversas (API v1) + ActionCable WebSocket.
    // Gated em checkout + bundle + postgres/redis (compose 5434/6381).
    let dir = runtime_dir("chatwoot");
    let app = fixture("chatwoot");
    let env: Vec<(&str, &str)> = vec![
        ("POSTGRES_HOST", "127.0.0.1"),
        ("POSTGRES_PORT", "5434"),
        ("POSTGRES_USERNAME", "chatwoot"),
        ("POSTGRES_PASSWORD", "chatwoot_password"),
        ("POSTGRES_DATABASE", "chatwoot_dev"),
        ("REDIS_URL", "redis://127.0.0.1:6381"),
        ("SECRET_KEY_BASE", "calisto-chatwoot-test-secret-key-base-0123456789abcdef"),
        ("FRONTEND_URL", "http://localhost:3000"),
        ("ACTIVE_STORAGE_SERVICE", "local"),
    ];

    if !app.join("calisto.toml").is_file() {
        eprintln!(
            "SKIP chatwoot_backend_serves_api_and_cable: checkout Chatwoot ausente — \
             `git clone --depth 1 https://github.com/chatwoot/chatwoot.git test/fixtures/chatwoot`"
        );
        return;
    }
    if !bundle_check(&app) {
        eprintln!(
            "SKIP chatwoot_backend_serves_api_and_cable: rode `bundle install` em test/fixtures/chatwoot \
             (pg/psych precisam de libpq/libyaml; scout_apm removido — nao compila no 3.4.10)"
        );
        return;
    }
    if !port_open(5434) || !port_open(6381) {
        let up = std::process::Command::new("docker")
            .args(["compose", "-f", "compose.calisto.yml", "up", "-d", "--wait"])
            .current_dir(&app)
            .output();
        match up {
            Ok(o) if o.status.success() => {}
            _ => {
                eprintln!(
                    "SKIP chatwoot_backend_serves_api_and_cable: suba a infra — \
                     `docker compose -f test/fixtures/chatwoot/compose.calisto.yml up -d`"
                );
                return;
            }
        }
    }
    if !wait_port(5434, 30) || !wait_port(6381, 30) {
        eprintln!("SKIP chatwoot_backend_serves_api_and_cable: postgres/redis nao subiram");
        return;
    }

        let prep = app_run(&dir, "chatwoot", &["run", "bin/rails", "db:prepare"], &env);
    assert!(prep.status.success(), "db:prepare: {}", String::from_utf8_lossy(&prep.stderr));
    let count = app_run(&dir, "chatwoot", &["run", "bin/rails", "runner", "print User.count"], &env);
    assert!(count.status.success(), "{}", String::from_utf8_lossy(&count.stderr));
    if String::from_utf8_lossy(&count.stdout).trim().parse::<u32>().unwrap_or(1) == 0 {
        let seed = app_run(&dir, "chatwoot", &["run", "bin/rails", "db:seed"], &env);
        assert!(seed.status.success(), "db:seed: {}", String::from_utf8_lossy(&seed.stderr));
    }

        let t0 = Instant::now();
    let boot = app_run(&dir, "chatwoot", &["run", "bin/rails", "runner", "puts Rails.env"], &env);
    assert!(boot.status.success(), "{}", String::from_utf8_lossy(&boot.stderr));
    assert!(
        t0.elapsed().as_millis() < 500,
        "2o comando deve ser <500ms: {}ms",
        t0.elapsed().as_millis()
    );

    // dev server (child do fork) + login + conversas + ActionCable
        let _ = std::fs::remove_file(app.join("tmp/pids/server.pid"));
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
    let deadline = Instant::now() + Duration::from_secs(30);
    while !port_open(port) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
assert!(port_open(port), "rails server do Chatwoot nao subiu");
    
    // login (devise_token_auth)
    let (status, head, _) = http_request(
        port,
        "POST",
        "/auth/sign_in",
        &[("Content-Type", "application/json")],
        Some(r#"{"email":"john@acme.inc","password":"Password1!"}"#),
    )
    .expect("login request");
    assert_eq!(status, 200, "login deve dar 200: {head}");
    let token = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("access-token:"))
        .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
        .expect("access-token no header");
    let uid = "john@acme.inc";
    let client = head
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("client:"))
        .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
        .expect("client no header do login");

    // conversas (API v1 autenticada)
    let (status, head, body) = http_request(
        port,
        "GET",
        "/api/v1/accounts/1/conversations",
        &[
            ("access-token", token.as_str()),
            ("uid", uid),
            ("client", client.as_str()),
            ("Content-Type", "application/json"),
        ],
        None,
    )
    .expect("conversations request");
    assert_eq!(status, 200, "conversations deve dar 200: {head} {body}");

    // ActionCable WebSocket handshake
    use std::io::{Read, Write};
    let mut ws = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    ws.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let key = base64_key();
    ws.write_all(
        format!(
            "GET /cable HTTP/1.1\r\nHost: localhost:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\n\r\n"
        )
        .as_bytes(),
    )
    .unwrap();
    let mut buf = [0u8; 2048];
    let n = ws.read(&mut buf).unwrap(); // um chunk: o WS fica aberto (sem EOF)
    let resp = String::from_utf8_lossy(&buf[..n]);
    assert!(
        resp.contains("101"),
        "cable deve fazer upgrade (101): {}",
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

fn base64_key() -> String {
    // Sec-WebSocket-Key: 16 bytes aleatorios em base64 (sem deps)
    let mut key = String::new();
    const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for i in 0..22 {
        let b = ((std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos() as u64)
            .wrapping_mul(2654435761)
            .wrapping_add(i as u64 * 7919))
            % 256;
        key.push(B64[(b & 63) as usize] as char);
    }
    key.push('=');
    key.push('=');
    key
}

/// Fase M (marco): a compactacao pre-fork (GC.start + GC.compact pos-boot)
/// corta o **Private_Dirty** do child do Chatwoot em >=30% vs o baseline sem
/// compactacao — o numero que prova o CoW dos forks (o Pss e diluido pela
/// metade compartilhada, constante nos dois modos). Mede via `calisto doctor`
/// (smaps_rollup do daemon + child de probe): boot do daemon com
/// CALISTO_COMPACT=0 (baseline fragmentado), mede; stop; boot default
/// (compact on), mede. O Chatwoot pina 3.4.4, entao o daemon roda no modo
/// legado (server.rb) — o teste cobre o hook de compactacao do daemon legado
/// tambem. Gated como os outros goldens.
#[test]
fn chatwoot_compaction_cuts_probe_child_pss() {
    let dir = runtime_dir("chatwootcompact");
    let app = fixture("chatwoot");
    let env: Vec<(&str, &str)> = vec![
        ("POSTGRES_HOST", "127.0.0.1"),
        ("POSTGRES_PORT", "5434"),
        ("POSTGRES_USERNAME", "chatwoot"),
        ("POSTGRES_PASSWORD", "chatwoot_password"),
        ("POSTGRES_DATABASE", "chatwoot_dev"),
        ("REDIS_URL", "redis://127.0.0.1:6381"),
        ("SECRET_KEY_BASE", "calisto-chatwoot-compact-test-secret-key-base-0123456789abcdef"),
        ("FRONTEND_URL", "http://localhost:3000"),
        ("ACTIVE_STORAGE_SERVICE", "local"),
    ];

    // gates (os mesmos do chatwoot_backend_serves_api_and_cable)
    if !app.join("calisto.toml").is_file() {
        eprintln!(
            "SKIP chatwoot_compaction_cuts_probe_child_pss: checkout Chatwoot ausente — \
             `git clone --depth 1 https://github.com/chatwoot/chatwoot.git test/fixtures/chatwoot`"
        );
        return;
    }
    if !bundle_check(&app) {
        eprintln!(
            "SKIP chatwoot_compaction_cuts_probe_child_pss: rode `bundle install` em test/fixtures/chatwoot"
        );
        return;
    }
    if !port_open(5434) || !port_open(6381) {
        let up = std::process::Command::new("docker")
            .args(["compose", "-f", "compose.calisto.yml", "up", "-d", "--wait"])
            .current_dir(&app)
            .output();
        match up {
            Ok(o) if o.status.success() => {}
            _ => {
                eprintln!(
                    "SKIP chatwoot_compaction_cuts_probe_child_pss: suba a infra — \
                     `docker compose -f test/fixtures/chatwoot/compose.calisto.yml up -d`"
                );
                return;
            }
        }
    }
    if !wait_port(5434, 30) || !wait_port(6381, 30) {
        eprintln!("SKIP chatwoot_compaction_cuts_probe_child_pss: postgres/redis nao subiram");
        return;
    }

    // Pss/Private_Dirty do child de probe reportados pelo doctor (linha
    // "probe child memory:"). O marco e o Private_Dirty: o Pss inclui a
    // metade compartilhada (constante nos dois modos — a compactacao nao
    // encolhe o heap) e dilui o efeito; o Private_Dirty e o custo exclusivo
    // do child, o numero que prova o CoW.
    let probe_mem = |out: &std::process::Output, key: &str| -> f64 {
        let s = String::from_utf8_lossy(&out.stdout);
        let l = s
            .lines()
            .find(|l| l.contains("probe child memory:"))
            .unwrap_or_else(|| panic!("doctor deveria reportar o probe child: {s}"));
        let toks: Vec<&str> = l.split_whitespace().collect();
        let i = toks.iter().position(|t| *t == key).expect("chave na linha");
        toks[i + 1].parse().expect("valor numerico")
    };

    // baseline: daemon sem compactacao (heap fragmentado)
    let mut env_off = env.clone();
    env_off.push(("CALISTO_COMPACT", "0"));
    let boot = app_run(&dir, "chatwoot", &["run", "bin/rails", "runner", "puts :ok"], &env_off);
    assert!(boot.status.success(), "boot baseline: {}", String::from_utf8_lossy(&boot.stderr));
    let doc_off = run_opt(
        &dir,
        RunOpts { args: &["doctor"], env: &[], stdin: None, cwd: Some(&app), timeout: 60 },
    );
    assert!(doc_off.status.success(), "{}", String::from_utf8_lossy(&doc_off.stderr));
    let (baseline_pd, baseline_pss) = (probe_mem(&doc_off, "Private_Dirty"), probe_mem(&doc_off, "Pss"));
    assert!(baseline_pd > 0.0, "baseline de Private_Dirty invalido: {baseline_pd}");
    let _ = run_opt(
        &dir,
        RunOpts { args: &["stop"], env: &[], stdin: None, cwd: Some(&app), timeout: 10 },
    );

    // compactado: daemon default (compact on no daemon de app)
    let boot = app_run(&dir, "chatwoot", &["run", "bin/rails", "runner", "puts :ok"], &env);
    assert!(boot.status.success(), "boot compactado: {}", String::from_utf8_lossy(&boot.stderr));
    let doc_on = run_opt(
        &dir,
        RunOpts { args: &["doctor"], env: &[], stdin: None, cwd: Some(&app), timeout: 60 },
    );
    assert!(doc_on.status.success(), "{}", String::from_utf8_lossy(&doc_on.stderr));
    let (compacted_pd, compacted_pss) = (probe_mem(&doc_on, "Private_Dirty"), probe_mem(&doc_on, "Pss"));

    let _ = run_opt(
        &dir,
        RunOpts { args: &["stop"], env: &[], stdin: None, cwd: Some(&app), timeout: 10 },
    );
    eprintln!(
        "chatwoot probe child: Private_Dirty {baseline_pd:.1} -> {compacted_pd:.1} MiB ({:.0}% de corte); \
         Pss {baseline_pss:.1} -> {compacted_pss:.1} MiB ({:.0}%)",
        (1.0 - compacted_pd / baseline_pd) * 100.0,
        (1.0 - compacted_pss / baseline_pss) * 100.0
    );
    assert!(
        compacted_pd <= baseline_pd * 0.7,
        "compactacao deveria cortar >=30% do Private_Dirty do child (baseline {baseline_pd:.1} MiB -> {compacted_pd:.1} MiB)"
    );
    assert!(
        compacted_pss < baseline_pss,
        "Pss do child tambem deveria cair (baseline {baseline_pss:.1} MiB -> {compacted_pss:.1} MiB)"
    );
}
