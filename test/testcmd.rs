//! Fase E: `calisto test` — detecção minitest/rspec, daemon de teste dedicado
//! (RAILS_ENV=test, socket próprio), fork por arquivo com paralelismo e meta
//! <500ms/arquivo quente.

mod common;

use common::*;
use std::path::Path;
use std::time::Instant;

fn app_test(dir: &Path, app: &str, extra: &[&str]) -> std::process::Output {
    let mut args = vec!["test"];
    args.extend_from_slice(extra);
    run_opt(
        dir,
        RunOpts {
            args: &args,
            env: &[],
            stdin: None,
            cwd: Some(&fixture(app)),
            timeout: 60,
        },
    )
}

fn app_run(dir: &Path, app: &str, args: &[&str]) -> std::process::Output {
    run_opt(
        dir,
        RunOpts {
            args,
            env: &[],
            stdin: None,
            cwd: Some(&fixture(app)),
            timeout: 60,
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
fn minitest_runs_warm_with_frozen_boot() {
    // Hermético (sem gems, sem rede): preloadapp tem boot simulado de 2s.
    // 1o run paga o boot no daemon de teste; o 2o e fork rapido — e o teste
    // boot_state_test.rb prova que o boot NAO re-rodou (contador == 1).
    let dir = runtime_dir("testherm");
    let app = fixture("preloadapp");
    let bc = dir.join("boot_count");
    let env = [("BOOT_COUNT_FILE", bc.to_str().unwrap())];
    let args = ["test"];

    let t0 = Instant::now();
    let first = run_opt(
        &dir,
        RunOpts { args: &args, env: &env, stdin: None, cwd: Some(&app), timeout: 60 },
    );
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let first_ms = t0.elapsed().as_millis();
    assert!(first_ms > 1500, "1o run deve pagar o boot (2s): {first_ms}ms");
    assert_eq!(std::fs::read_to_string(&bc).unwrap().trim(), "1");

    // 2o run: a suite inteira (2 arquivos, um com sleep 0.6s) em <1s —
    // paralelismo de arquivos + fork quente (serializado seria ~1.3s)
    let t0 = Instant::now();
    let second = run_opt(
        &dir,
        RunOpts { args: &args, env: &env, stdin: None, cwd: Some(&app), timeout: 60 },
    );
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    let ms = t0.elapsed().as_millis();
    assert!(ms < 1000, "suite quente com teste de 0.6s deve ser <1s (paralelo): {ms}ms");
    assert_eq!(std::fs::read_to_string(&bc).unwrap().trim(), "1", "boot nao re-roda");
    let out = String::from_utf8_lossy(&second.stdout);
    assert!(out.contains("2 arquivo(s)") && out.contains("0 falhou(ram)"), "{out}");
    stop_app(&dir, "preloadapp");
}

#[test]
fn filter_does_not_duplicate_files() {
    // Filtro com caminho relativo (ex.: `calisto test test/foo_test.rb`) deve
    // casar com o arquivo descoberto (absoluto) SEM duplica-lo — o summary
    // mostra o arquivo UMA vez (regressao achada no comparativo do chatwoot).
    let dir = runtime_dir("testfilter");
    let app = fixture("preloadapp");
    let bc = dir.join("boot_count");
    let env = [("BOOT_COUNT_FILE", bc.to_str().unwrap())];
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["test", "test/boot_state_test.rb"],
            env: &env,
            stdin: None,
            cwd: Some(&app),
            timeout: 60,
        },
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("1 arquivo(s) de teste"), "{out}");
    stop_app(&dir, "preloadapp");
}

#[test]
fn failing_test_exits_nonzero() {
    let dir = runtime_dir("testfail");
    let app = fixture("preloadapp");
    let bc = dir.join("boot_count");
    let env = [("BOOT_COUNT_FILE", bc.to_str().unwrap())];
    let failing = dir.join("failing_test.rb");
    std::fs::write(
        &failing,
        "# frozen_string_literal: true\nrequire \"minitest/autorun\"\nclass BoomTest < Minitest::Test\n  def test_boom\n    assert false, \"boom\"\n  end\nend\n",
    )
    .unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["test", failing.to_str().unwrap()],
            env: &env,
            stdin: None,
            cwd: Some(&app),
            timeout: 60,
        },
    );
    assert_eq!(out.status.code(), Some(1), "suite com falha deve sair 1");
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("FAIL") && out.contains("boom"), "{out}");
    stop_app(&dir, "preloadapp");
}

#[test]
fn no_tests_found_fails_clearly() {
    let dir = runtime_dir("testnone");
    let project = dir.join("emptyproj");
    std::fs::create_dir_all(&project).unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["test"],
            env: &[],
            stdin: None,
            cwd: Some(&project),
            timeout: 30,
        },
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("nenhum teste"));
}

#[test]
fn rspec_detected_via_dot_rspec() {
    // `.rspec` presente -> framework rspec (spec/*_spec.rb). A spec referencia
    // spec_helper (sem rspec instalado aqui) e falha — o que prova que o
    // arquivo certo foi coletado e executado.
    let dir = runtime_dir("testrspec");
    let project = dir.join("rspecproj");
    std::fs::create_dir_all(project.join("spec")).unwrap();
    std::fs::write(project.join(".rspec"), "--require spec_helper\n").unwrap();
    std::fs::write(project.join("spec/foo_spec.rb"), "require \"spec_helper\"\n").unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["test"],
            env: &[],
            stdin: None,
            cwd: Some(&project),
            timeout: 30,
        },
    );
    let out_s = String::from_utf8_lossy(&out.stdout);
    assert!(out_s.contains("foo_spec.rb"), "detecção rspec deve rodar spec/*_spec.rb: {out_s}");
    assert_eq!(out.status.code(), Some(1), "spec sem rspec falha (esperado)");
}

#[test]
fn railsapp_minitest_golden() {
    // Golden do marco: `calisto test` na suite do railsapp (minitest) — cada
    // arquivo <500ms quente, e o teste rails_env_test.rb prova que o daemon de
    // teste bootou com RAILS_ENV=test (o daemon dev deixaria "development").
    let dir = runtime_dir("testrails");
    let app = fixture("railsapp");
    if !common::bundle_check(&app) {
        eprintln!("SKIP railsapp_minitest_golden: rode `bundle install` em test/fixtures/railsapp");
        return;
    }

    let first = app_test(&dir, "railsapp", &[]);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    let out = String::from_utf8_lossy(&first.stdout);
    assert!(out.contains("2 arquivo(s)") && out.contains("0 falhou(ram)"), "{out}");

    let t0 = Instant::now();
    let second = app_test(&dir, "railsapp", &[]);
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    let ms = t0.elapsed().as_millis();
    // 2 arquivos, paralelos: a suite inteira bem abaixo de 1s
    assert!(ms < 1000, "suite railsapp quente deve ser <1s: {ms}ms");
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("0 falhou(ram)"),
        "{}",
        String::from_utf8_lossy(&second.stdout)
    );
    stop_app(&dir, "railsapp");
}

#[test]
fn dotenv_loaded_warm_and_cold_with_parity() {
    // .env do projeto entra no env: warm e --cold concordam (paridade e
    // invariante), aspas/`export` sao tratados, e var existente NAO e
    // sobrescrita.
    let dir = runtime_dir("testenv");
    let app = fixture("preloadapp");
    let bc = dir.join("boot_count");
    let env = [("BOOT_COUNT_FILE", bc.to_str().unwrap())];
    let env_check = ["run", "env_check.rb"];
    let env_check_cold = ["run", "--cold", "env_check.rb"];

    let warm = run_opt(
        &dir,
        RunOpts { args: &env_check, env: &env, stdin: None, cwd: Some(&app), timeout: 60 },
    );
    assert!(warm.status.success(), "{}", String::from_utf8_lossy(&warm.stderr));
    let warm_out = String::from_utf8_lossy(&warm.stdout);
    assert!(warm_out.contains("CALISTO_DOTENV=loaded"), "{warm_out}");
    assert!(warm_out.contains("CALISTO_DOTENV_QUOTED=quoted value"), "{warm_out}");
    assert!(warm_out.contains("CALISTO_DOTENV_EXPORT=exported"), "{warm_out}");

    let cold = run_opt(
        &dir,
        RunOpts { args: &env_check_cold, env: &env, stdin: None, cwd: Some(&app), timeout: 30 },
    );
    assert!(cold.status.success(), "{}", String::from_utf8_lossy(&cold.stderr));
    assert_eq!(
        warm_out,
        String::from_utf8_lossy(&cold.stdout),
        "cold e warm devem concordar no .env (paridade)"
    );

    // var existente no ambiente tem precedencia (semantica dotenv)
    let over = run_opt(
        &dir,
        RunOpts {
            args: &env_check_cold,
            env: &[("BOOT_COUNT_FILE", bc.to_str().unwrap()), ("CALISTO_DOTENV", "override")],
            stdin: None,
            cwd: Some(&app),
            timeout: 30,
        },
    );
    assert!(String::from_utf8_lossy(&over.stdout).contains("CALISTO_DOTENV=override"));
    stop_app(&dir, "preloadapp");
}

#[test]
fn task_runs_rake_warm_and_idempotent() {
    // `calisto task db:migrate` no daemon quente da app (dev): 1o paga o
    // boot, 2o e fork rapido, e rodar de novo nao falha (idempotente).
    let dir = runtime_dir("taskrak");
    let app = fixture("railsapp");
    if !common::bundle_check(&app) {
        eprintln!("SKIP task_runs_rake_warm_and_idempotent: rode `bundle install` em test/fixtures/railsapp");
        return;
    }
    let schema = app.join("db/schema.rb");
    let dev_db = app.join("db/development.sqlite3");
    let _ = std::fs::remove_file(&schema);
    let _ = std::fs::remove_file(&dev_db);

    let first = app_run(&dir, "railsapp", &["task", "db:migrate"]);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert!(schema.exists(), "db:migrate deveria gerar db/schema.rb");

    let t0 = Instant::now();
    let second = app_run(&dir, "railsapp", &["task", "db:migrate"]);
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    let ms = t0.elapsed().as_millis();
    assert!(ms < 1000, "task warm deve ser <1s: {ms}ms");

    // limpa artefatos do fixture (o teste nao pode sujar a arvore)
    let _ = std::fs::remove_file(&schema);
    let _ = std::fs::remove_file(&dev_db);
    stop_app(&dir, "railsapp");
}

#[test]
fn serve_serves_rack_app_as_daemon_child() {
    // `calisto serve` sobe o config.ru (rackup + puma do bundle) como child do
    // fork — e o daemon CONTINUA servindo outros comandos enquanto o server
    // roda (accept loop multi-conexao).
    let dir = runtime_dir("servesmoke");
    let app = fixture("railsapp");
    if !common::bundle_check(&app) {
        eprintln!("SKIP serve_serves_rack_app_as_daemon_child: rode `bundle install` em test/fixtures/railsapp");
        return;
    }

    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut child = common::calisto(&dir)
        .args(["serve", "-p", &port.to_string()])
        .current_dir(&app)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn calisto serve");

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
                panic!("calisto serve nao subiu: {e}");
            }
        }
    };
    assert!(
        body.contains("200") && body.contains("green"),
        "GET /up deve responder 200: {body}"
    );

    // multi-conexao: um comando roda no daemon enquanto o server serve
    let runner = run_opt(
        &dir,
        RunOpts {
            args: &["run", "bin/rails", "runner", "puts 42"],
            env: &[],
            stdin: None,
            cwd: Some(&app),
            timeout: 30,
        },
    );
    assert!(runner.status.success(), "{}", String::from_utf8_lossy(&runner.stderr));
    assert_eq!(String::from_utf8_lossy(&runner.stdout).trim(), "42");

    let _ = child.kill();
    let _ = child.wait();
    stop_app(&dir, "railsapp");
}
