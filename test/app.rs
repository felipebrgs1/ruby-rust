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

/// Fase M.1: compactacao pre-fork. O daemon da app roda GC.start + GC.compact
/// pos-boot por default (preload presente); o child de fork herda o contador
/// do GC — `GC.stat(:compact_count) >= 1` prova que o boot foi compactado.
/// CALISTO_COMPACT=0 desliga (daemon respawnado: flag de perf nao entra no
/// hash do socket). Semantica intacta: compactacao e performance, os outros
/// testes de boot congelado cobrem a paridade.
#[test]
fn app_daemon_compacts_heap_by_default() {
    let dir = runtime_dir("appcompact");
    let bc = dir.join("boot_count");
    let env = [("BOOT_COUNT_FILE", bc.to_str().unwrap())];

    // daemon da app com compactacao default on (preload presente)
    let first = app_run(&dir, "preloadapp", &["run", "main.rb"], &env);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let out = app_run(&dir, "preloadapp", &["run", "-e", "puts GC.stat(:compact_count)"], &env);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let n: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
    assert!(n >= 1, "daemon da app deve compactar o heap no boot: compact_count={}", String::from_utf8_lossy(&out.stdout));

    // desligado por override: stop + run com CALISTO_COMPACT=0
    stop_app(&dir, "preloadapp");
    let env_off = [("BOOT_COUNT_FILE", bc.to_str().unwrap()), ("CALISTO_COMPACT", "0")];
    let out = app_run(&dir, "preloadapp", &["run", "-e", "puts GC.stat(:compact_count)"], &env_off);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let n: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(99);
    assert_eq!(n, 0, "CALISTO_COMPACT=0 deve desligar a compactacao: {}", String::from_utf8_lossy(&out.stdout));
    stop_app(&dir, "preloadapp");
}

/// Fase M.1: `[run] compact` no calisto.toml — valor invalido falha com a
/// linha apontada; `compact = "false"` desliga (e o default-on cobre o true).
#[test]
fn compact_flag_in_toml_is_parsed_and_validated() {
    let dir = runtime_dir("appcompactflag");
    let project = dir.join("compactapp");
    std::fs::create_dir_all(project.join("config")).unwrap();
    std::fs::write(project.join("script.rb"), "puts :ok\n").unwrap();
    std::fs::write(project.join("config/boot.rb"), "# boot barato\n").unwrap();

    // booleano TOML puro invalido -> erro apontando a linha
    std::fs::write(
        project.join("calisto.toml"),
        "[run]\npreload = \"config/boot.rb\"\ncompact = banana\n",
    )
    .unwrap();
    let out = run_opt(
        &dir,
        RunOpts { args: &["run", "script.rb"], env: &[], stdin: None, cwd: Some(&project), timeout: 30 },
    );
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("calisto.toml:3") && err.contains("compact"),
        "erro deve apontar a linha e a chave: {err}"
    );

    // false explicito no toml desliga a compactacao do daemon da app
    std::fs::write(
        project.join("calisto.toml"),
        "[run]\npreload = \"config/boot.rb\"\ncompact = \"false\"\n",
    )
    .unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "-e", "puts GC.stat(:compact_count)"],
            env: &[],
            stdin: None,
            cwd: Some(&project),
            timeout: 30,
        },
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "0",
        "compact = false no calisto.toml"
    );
    let _ = run_opt(
        &dir,
        RunOpts { args: &["stop"], env: &[], stdin: None, cwd: Some(&project), timeout: 10 },
    );
}

/// Fase N: daemon da app boota com `--yjit` ([run] yjit) e roda o warmup
/// pos-boot ([run] warmup). O child de fork herda o codigo compilado
/// (`RubyVM::YJIT.runtime_stats[:compiled_iseq_count]` > 0) — inclusive com
/// a compactacao do boot ligada (Fase N.3: paginas de codigo do YJIT nao sao
/// heap de objetos, sobrevivem ao GC.compact — o teste prova que as duas
/// coisas convivem: compact_count >= 1 E codigo compilado no child).
#[test]
fn app_daemon_boots_with_yjit_and_warmup() {
    let dir = runtime_dir("appyjit");
    let bc = dir.join("boot_count");
    let wc = dir.join("warmup_count");
    let env = [
        ("BOOT_COUNT_FILE", bc.to_str().unwrap()),
        ("WARMUP_COUNT_FILE", wc.to_str().unwrap()),
    ];

    // boot + warmup rodam UMA vez no daemon
    let first = app_run(&dir, "yjitapp", &["run", "main.rb"], &env);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert_eq!(std::fs::read_to_string(&bc).unwrap().trim(), "1");
    assert_eq!(std::fs::read_to_string(&wc).unwrap().trim(), "1");

    // child: YJIT ligado, heap compactado no boot e codigo compilado herdado
    let probe = "st = RubyVM::YJIT.enabled? ? RubyVM::YJIT.runtime_stats : {}; \
                 puts [RubyVM::YJIT.enabled?, GC.stat(:compact_count), st[:compiled_iseq_count].to_i].join(' ')";
    let out = app_run(&dir, "yjitapp", &["run", "-e", probe], &env);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out_str = String::from_utf8_lossy(&out.stdout);
    let parts: Vec<&str> = out_str.trim().split(' ').collect();
    assert_eq!(parts.first().copied(), Some("true"), "yjit deve estar ligado: {}", out_str);
    let compact: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    assert!(compact >= 1, "compactacao do boot deve rodar com yjit: {}", out_str);
    let compiled: u64 = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    assert!(compiled > 0, "child deve herdar codigo compilado do warmup: {}", out_str);
    assert_eq!(std::fs::read_to_string(&wc).unwrap().trim(), "1", "warmup nao pode re-rodar por comando");
    stop_app(&dir, "yjitapp");
}

/// Fase N: overrides — CALISTO_YJIT=0 desliga o JIT; CALISTO_WARMUP=0
/// desliga o warmup (o resto do calisto.toml vale).
#[test]
fn yjit_and_warmup_can_be_disabled() {
    let dir = runtime_dir("appyjitoff");
    let bc = dir.join("boot_count");
    let wc = dir.join("warmup_count");

    // yjit off: daemon novo (CALISTO_YJIT=0 no spawn) — warmup continua rodando
    let env_off = [
        ("BOOT_COUNT_FILE", bc.to_str().unwrap()),
        ("WARMUP_COUNT_FILE", wc.to_str().unwrap()),
        ("CALISTO_YJIT", "0"),
    ];
    let out = app_run(&dir, "yjitapp", &["run", "-e", "puts RubyVM::YJIT.enabled?"], &env_off);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "false");
    assert_eq!(std::fs::read_to_string(&wc).unwrap().trim(), "1", "warmup roda independente do yjit");
    stop_app(&dir, "yjitapp");

    // warmup off: daemon novo (CALISTO_WARMUP=0) — yjit do toml continua on
    let env_nowarm = [
        ("BOOT_COUNT_FILE", bc.to_str().unwrap()),
        ("WARMUP_COUNT_FILE", wc.to_str().unwrap()),
        ("CALISTO_WARMUP", "0"),
    ];
    let out = app_run(&dir, "yjitapp", &["run", "-e", "puts RubyVM::YJIT.enabled?"], &env_nowarm);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "true", "yjit do toml continua valendo");
    assert!(
        std::fs::read_to_string(&wc).map(|s| s.trim().to_string()).unwrap_or_default() == "1",
        "CALISTO_WARMUP=0 deve desligar o warmup (contador ficaria 2)"
    );
    stop_app(&dir, "yjitapp");
}

/// Fase N: validacao do toml — yjit com valor invalido e warmup inexistente
/// falham com a linha/chave apontada.
#[test]
fn yjit_and_warmup_flags_validated_in_toml() {
    let dir = runtime_dir("appyjitbad");
    let project = dir.join("yjitbad");
    std::fs::create_dir_all(project.join("config")).unwrap();
    std::fs::write(project.join("script.rb"), "puts :ok\n").unwrap();
    std::fs::write(project.join("config/boot.rb"), "# boot barato\n").unwrap();

    std::fs::write(
        project.join("calisto.toml"),
        "[run]\npreload = \"config/boot.rb\"\nyjit = banana\n",
    )
    .unwrap();
    let out = run_opt(
        &dir,
        RunOpts { args: &["run", "script.rb"], env: &[], stdin: None, cwd: Some(&project), timeout: 30 },
    );
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("calisto.toml:3") && err.contains("yjit"), "erro deve apontar a linha: {err}");

    std::fs::write(
        project.join("calisto.toml"),
        "[run]\npreload = \"config/boot.rb\"\nwarmup = \"config/missing.rb\"\n",
    )
    .unwrap();
    let out = run_opt(
        &dir,
        RunOpts { args: &["run", "script.rb"], env: &[], stdin: None, cwd: Some(&project), timeout: 30 },
    );
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("warmup") && err.contains("missing.rb"),
        "erro deve apontar o warmup: {err}"
    );
}

#[test]
fn rails_runner_frozen_boot() {
    // Golden test do marco: `rails runner` em app Rails 8 + sqlite3 com boot
    // congelado. Gated: exige `bundle install` previo no fixture (rede).
    let dir = runtime_dir("railsapp");
    let app = fixture("railsapp");
    if !common::bundle_check(&app) {
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

#[test]
fn rails_dev_server_serves_http() {
    // Fase C: `bin/rails server` roda como child do fork do daemon da app;
    // GET /up -> 200 com o app ja bootado (<500ms do spawn a resposta).
    let dir = runtime_dir("railsserver");
    let app = fixture("railsapp");
    if !common::bundle_check(&app) {
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
    if !common::bundle_check(&app) {
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

/// GET /cpu HTTP 1.1 (conexao nova, sem keep-alive) — mede o round trip.
fn http_get_ms(port: u16) -> u64 {
    use std::io::{Read, Write};
    let t0 = Instant::now();
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect server");
    s.set_read_timeout(Some(std::time::Duration::from_secs(15))).unwrap();
    write!(s, "GET /cpu HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).expect("read resposta");
    assert!(
        buf.starts_with(b"HTTP/1.1 200"),
        "resposta inesperada: {}",
        String::from_utf8_lossy(&buf.get(..48).unwrap_or(&buf))
    );
    t0.elapsed().as_millis() as u64
}

/// Fase N (marco): com `[run] yjit` + warmup, o 1o request de cada child do
/// fork NAO paga compilacao JIT — p50 do 1o request ~= p50 steady-state.
/// Hoje (yjit on, sem warmup) o 1o request compila o hot path inteiro.
/// Gated em bundle install (railsapp); o warmup aquece /cpu em memoria
/// (ActionDispatch::Integration::Session) no daemon, antes do bind.
#[test]
fn rails_yjit_warmup_first_request_matches_steady_state() {
    let dir = runtime_dir("railsyjit");
    let app = fixture("railsapp");
    if !common::bundle_check(&app) {
        eprintln!("SKIP rails_yjit_warmup_first_request_matches_steady_state: rode `bundle install` em test/fixtures/railsapp");
        return;
    }
    let _ = std::fs::remove_file(app.join("tmp/pids/server.pid"));

    let warmup = dir.join("warmup.rb");
    std::fs::write(
        &warmup,
        "# frozen_string_literal: true\n\
         # Warmup de verdade: requests HTTP reais contra um Puma em memoria\n\
         # no daemon. Integration::Session NAO serve para este fim — o host\n\
         # default (www.example.com) e bloqueado pelo HostAuthorization com\n\
         # 403 antes do hot path rodar (medido: 1o request do child seguia\n\
         # lento); o Puma real aquece TODO o caminho de request (server +\n\
         # middleware stack + app) e o child do fork herda codigo e estado.\n\
         require \"puma\"\n\
         require \"puma/server\"\n\
         require \"net/http\"\n\
         server = Puma::Server.new(Rails.application)\n\
         server.add_tcp_listener(\"127.0.0.1\", 0)\n\
         server.run\n\
         port = server.connected_ports.first\n\
         begin\n\
           100.times do\n\
             res = Net::HTTP.start(\"127.0.0.1\", port) { |h| h.get(\"/cpu\") }\n\
             raise \"warmup: /cpu -> #{res.code}\" unless res.code == \"200\"\n\
           end\n\
         ensure\n\
           server.stop(true)\n\
         end\n",
    )
    .unwrap();

    // spawna o server (child do fork do daemon da app) e mede o 1o request
    // vs p50 steady; derruba o server + daemon no fim (proxima config comeca
    // limpa). O daemon spawna com o env da invocacao — o warmup/yjit do
    // benchmark valem para o boot inteiro.
    let measure = |env: &[(&str, &str)]| -> (u64, u64) {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let mut child = common::calisto(&dir)
            .arg("run")
            .arg("bin/rails")
            .args(["server", "-p"])
            .arg(port.to_string())
            .args(["-b", "127.0.0.1"])
            .current_dir(&app)
            .envs(env.iter().copied())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn rails server");

        // espera o puma aceitar conexao (fora da medicao)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("server nao subiu em 20s");
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let first = http_get_ms(port);
        let mut times = Vec::new();
        for _ in 0..24 {
            times.push(http_get_ms(port));
        }
        times.sort_unstable();
        let p50 = times[times.len() / 2];

        let _ = child.kill();
        let _ = child.wait();
        let _ = run_opt(
            &dir,
            RunOpts { args: &["stop"], env: &[], stdin: None, cwd: Some(&app), timeout: 10 },
        );
        (first, p50)
    };

    // config A: yjit on, SEM warmup — o 1o request paga a compilacao
    let (cold_t1, cold_p50) = measure(&[("CALISTO_YJIT", "1"), ("CALISTO_WARMUP", "0")]);
    // config B: yjit on + warmup — codigo compilado no daemon, herdado
    let (hot_t1, hot_p50) = measure(&[("CALISTO_YJIT", "1"), ("CALISTO_WARMUP", warmup.to_str().unwrap())]);

    eprintln!(
        "railsapp /cpu: sem warmup 1o={cold_t1}ms p50={cold_p50}ms ({:.0}x); com warmup 1o={hot_t1}ms p50={hot_p50}ms ({:.1}x)",
        cold_t1 as f64 / cold_p50.max(1) as f64,
        hot_t1 as f64 / hot_p50.max(1) as f64
    );
    assert!(
        cold_t1 >= cold_p50 * 2,
        "sem warmup o 1o request deve pagar o JIT (1o {cold_t1}ms vs p50 {cold_p50}ms)"
    );
    // com warmup resta um residual pequeno no 1o request do child (6-13ms
    // medidos: primeiro accept/threads do puma do fork — nao e JIT, que o
    // warmup removeu; o assert frio acima prova que a penalidade existe).
    // Bound absoluto: sem warmup o 1o request mede 120-190ms, entao 30ms
    // ainda prova o warmup com margem de ~5x; ratio contra p50 seria flaky
    // (p50 = 1ms).
    assert!(
        hot_t1 <= 30,
        "com warmup o 1o request deve ser ~steady (1o {hot_t1}ms vs p50 {hot_p50}ms)"
    );
    assert!(
        hot_t1 * 5 <= cold_t1,
        "warmup deve remover >= 80% da penalidade do 1o request ({hot_t1}ms vs {cold_t1}ms)"
    );
}
