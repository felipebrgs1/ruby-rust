//! Fase H: scripts no calisto.toml (`calisto run <script>`, o package.json do
//! Ruby).
//!
//! Invariantes cobertos:
//! - nome que nao e arquivo resolve para [scripts.NAME]; arquivo existente
//!   SEMPRE vence (regressao do run de arquivos)
//! - args do CLI sao repassados ao comando do script; aspas agrupam palavras
//! - scripts rodam com cwd na raiz da app (dir do calisto.toml), como bun run
//! - warm e --cold concordam no script (paridade e regra de ouro do run)
//! - calisto.toml so com [scripts] (sem [run] preload) NAO vira daemon da
//!   app — usa o daemon generico (apps/ nao e criado)
//! - scripts no app com preload rodam no daemon da app (boot UMA vez)
//! - script inexistente/vazio e erro claro apontando o problema
//! - golden: `run dev` sobe o railsapp, `run db:migrate` idempotente e
//!   `run test` roda a suite — `bin/rails test` no daemon dev (o teste de
//!   env falha por design: daemon dev vs daemon de teste e a razao de
//!   `calisto test` existir)

mod common;

use common::*;
use std::path::Path;
use std::time::Instant;

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
fn run_script_executes_command_with_args_at_app_root() {
    // `calisto run greet extra` roda o comando de [scripts.greet] no daemon,
    // com os args do CLI no final — e o child roda com cwd na raiz da app
    // (dir do calisto.toml), mesmo chamado de uma subdir (como bun run).
    let dir = runtime_dir("scriptrun");
    let app = fixture("scriptsapp");
    let sub = app.join("sub");
    std::fs::create_dir_all(&sub).unwrap();

    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "greet", "extra", "-x"],
            env: &[],
            stdin: None,
            cwd: Some(&sub),
            timeout: 30,
        },
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("hello"), "{out}");
    assert!(
        out.contains("ARGV=world,extra,-x"),
        "args do CLI devem ir ao final do comando do script: {out}"
    );
    assert!(
        out.contains(&format!("cwd={}", app.display())),
        "script deve rodar na raiz da app: {out}"
    );
}

#[test]
fn run_script_quotes_group_words() {
    // Aspas no comando do script agrupam palavras em um unico ARGV (sem
    // shell): `quoted = "bin/hello 'a b'"` -> ARGV == ["a b"].
    let dir = runtime_dir("scriptquote");
    let out = app_run(&dir, "scriptsapp", &["run", "quoted"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = String::from_utf8_lossy(&out.stdout);
    assert!(out.contains("ARGV=a b"), "aspas devem agrupar: {out}");
}

#[test]
fn run_script_cold_agrees_with_warm() {
    // Paridade cold/warm e invariante do run: `--cold` roda o shim de exec no
    // interpretador direto com cwd na raiz — mesmo argv, mesmo stdout.
    let dir = runtime_dir("scriptcold");

    let warm = app_run(&dir, "scriptsapp", &["run", "greet", "x"]);
    assert!(warm.status.success(), "{}", String::from_utf8_lossy(&warm.stderr));
    let cold = app_run(&dir, "scriptsapp", &["run", "--cold", "greet", "x"]);
    assert!(cold.status.success(), "{}", String::from_utf8_lossy(&cold.stderr));

    assert_eq!(warm.stdout, cold.stdout, "warm e cold devem concordar no script");
    assert_eq!(warm.status.code(), cold.status.code());
    assert!(
        String::from_utf8_lossy(&cold.stdout).contains("ARGV=world,x"),
        "{}",
        String::from_utf8_lossy(&cold.stdout)
    );
}

#[test]
fn run_script_file_wins_over_script_name() {
    // Precedencia: arquivo existente vence o script de mesmo nome (regressao
    // do `run <script.rb>` tradicional — um nome que vira arquivo nao pode
    // mudar de significado).
    let dir = runtime_dir("scriptfile");
    let project = dir.join("filewins");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("calisto.toml"),
        "[scripts]\nrunme = \"bin/hello\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(project.join("bin")).unwrap();
    std::fs::write(
        project.join("bin/hello"),
        "#!/usr/bin/env ruby\n# frozen_string_literal: true\nputs :script\n",
    )
    .unwrap();
    std::fs::set_permissions(
        project.join("bin/hello"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(project.join("runme"), "puts :file\n").unwrap();

    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "runme"],
            env: &[],
            stdin: None,
            cwd: Some(&project),
            timeout: 30,
        },
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "file",
        "arquivo existente deve vencer o script de mesmo nome"
    );
}

#[test]
fn run_script_not_found_fails_clearly() {
    // Sem arquivo e sem [scripts.nome] -> erro que nomeia as duas possiveis
    // intencoes (regressao do missing_script_fails do cli.rs, agora com dica).
    let dir = runtime_dir("scriptmissing");
    let out = app_run(&dir, "scriptsapp", &["run", "nao-existe"]);
    assert_ne!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("nao-existe"), "{err}");
    assert!(err.contains("scripts.nao-existe"), "dica do script: {err}");
}

#[test]
fn run_script_empty_or_broken_command_fails_clearly() {
    let dir = runtime_dir("scriptbad");
    let project = dir.join("badscripts");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("calisto.toml"),
        "[scripts]\nempty = \"\"\nbroken = \"bin/hello 'x\"\n",
    )
    .unwrap();

    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "empty"],
            env: &[],
            stdin: None,
            cwd: Some(&project),
            timeout: 30,
        },
    );
    assert_ne!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("empty"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "broken"],
            env: &[],
            stdin: None,
            cwd: Some(&project),
            timeout: 30,
        },
    );
    assert_ne!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nao fechadas"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn scripts_only_config_uses_generic_daemon() {
    // calisto.toml so com [scripts] nao vira daemon da app (Fase B): o run de
    // arquivo continua no daemon generico e apps/ nao e criado no runtime dir.
    let dir = runtime_dir("scriptgeneric");
    let app = fixture("scriptsapp");

    let out = app_run(&dir, "scriptsapp", &["run", "hello"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // o daemon generico foi usado (socket no runtime dir raiz)...
    assert!(dir.join("calisto.sock").exists(), "daemon generico deveria rodar");
    // ...e nenhum daemon da app foi criado
    assert!(
        !dir.join("apps").exists(),
        "config so de scripts nao pode virar daemon da app"
    );

    // e um arquivo (nao script) ainda roda no daemon generico
    let hello = fixture("hello.rb");
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", hello.to_str().unwrap()],
            env: &[],
            stdin: None,
            cwd: Some(&app),
            timeout: 30,
        },
    );
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    stop_app(&dir, "scriptsapp");
}

#[test]
fn run_script_in_app_with_preload_boots_once() {
    // Script no app COM preload (preloadapp): roda como fork do daemon da app
    // — o boot (2s simulados) roda UMA vez e o 2o script e fork rapido.
    let dir = runtime_dir("scriptapp");
    let app = fixture("preloadapp");
    let bc = dir.join("boot_count");
    let env = [("BOOT_COUNT_FILE", bc.to_str().unwrap())];

    let first = run_opt(
        &dir,
        RunOpts {
            args: &["run", "hello"],
            env: &env,
            stdin: None,
            cwd: Some(&app),
            timeout: 60,
        },
    );
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert_eq!(
        std::fs::read_to_string(&bc).unwrap().trim(),
        "1",
        "1o script paga o boot no daemon da app"
    );

    let t0 = Instant::now();
    let second = run_opt(
        &dir,
        RunOpts {
            args: &["run", "hello", "again"],
            env: &env,
            stdin: None,
            cwd: Some(&app),
            timeout: 60,
        },
    );
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));
    let ms = t0.elapsed().as_millis();
    assert!(ms < 500, "script warm no daemon da app deve ser <500ms: {ms}ms");
    assert_eq!(
        std::fs::read_to_string(&bc).unwrap().trim(),
        "1",
        "script nao pode re-bootar a app"
    );
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("ARGV=again"),
        "{}",
        String::from_utf8_lossy(&second.stdout)
    );
    stop_app(&dir, "preloadapp");
}

#[test]
fn run_scripts_on_railsapp() {
    // Golden do marco Fase H: `calisto run dev` sobe o railsapp, `calisto run
    // db:migrate` e idempotente e `calisto run test` roda a suite via
    // `bin/rails test` no daemon dev (com args do CLI repassados). Gated:
    // exige `bundle install` previo no fixture (rede).
    let dir = runtime_dir("scriptrails");
    let app = fixture("railsapp");
    if !common::bundle_check(&app) {
        eprintln!("SKIP run_scripts_on_railsapp: rode `bundle install` em test/fixtures/railsapp");
        return;
    }
    let schema = app.join("db/schema.rb");
    let dev_db = app.join("db/development.sqlite3");
    let _ = std::fs::remove_file(&schema);
    let _ = std::fs::remove_file(&dev_db);

    // aquece o daemon da app (boot congelado) antes dos marcos de tempo
    let warm = app_run(&dir, "railsapp", &["run", "bin/rails", "runner", "puts 1"]);
    assert!(warm.status.success(), "{}", String::from_utf8_lossy(&warm.stderr));

    // marco 1: `run db:migrate` — idempotente (2a execucao nao falha)
    let first = app_run(&dir, "railsapp", &["run", "db:migrate"]);
    assert!(first.status.success(), "{}", String::from_utf8_lossy(&first.stderr));
    assert!(schema.exists(), "db:migrate deveria gerar db/schema.rb");
    let second = app_run(&dir, "railsapp", &["run", "db:migrate"]);
    assert!(second.status.success(), "{}", String::from_utf8_lossy(&second.stderr));

    // marco 2: `run test <filtro>` — args do CLI repassados ao script
    let suite = app_run(&dir, "railsapp", &["run", "test", "test/arithmetic_test.rb"]);
    assert!(suite.status.success(), "{}", String::from_utf8_lossy(&suite.stderr));
    assert!(
        String::from_utf8_lossy(&suite.stdout).contains("runs"),
        "suite deve rodar via script: {}",
        String::from_utf8_lossy(&suite.stdout)
    );

    // `run test` (suite completa) roda `bin/rails test` no daemon DEV: o
    // teste rails_env_test falha por design (Rails.env ficou "development" no
    // boot congelado — o daemon de teste RAILS_ENV=test e a razao de
    // `calisto test` existir). O ponto e provar que o script executa a suite
    // de verdade e reporta a falha.
    let full = app_run(&dir, "railsapp", &["run", "test"]);
    assert_ne!(full.status.code(), Some(0), "rails_env_test deve falhar no daemon dev");
    let full_out = format!(
        "{}{}",
        String::from_utf8_lossy(&full.stdout),
        String::from_utf8_lossy(&full.stderr)
    );
    assert!(
        full_out.contains("failures") || full_out.contains("rails_env"),
        "a suite deve ter rodado e reportado a falha de env: {full_out}"
    );

    // marco 3: `run dev -p PORT` sobe o server (args no final do comando)
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut child = common::calisto(&dir)
        .arg("run")
        .arg("dev")
        .args(["-p", &port.to_string(), "-b", "127.0.0.1"])
        .current_dir(&app)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn run dev");

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
                panic!("run dev nao subiu o server: {e}");
            }
        }
    };
    assert!(
        body.contains("200") && body.contains("green"),
        "health /up deve responder 200 up: {body}"
    );

    let _ = child.kill();
    let _ = child.wait();

    // limpa artefatos do fixture (o teste nao pode sujar a arvore)
    let _ = std::fs::remove_file(&schema);
    let _ = std::fs::remove_file(&dev_db);
    stop_app(&dir, "railsapp");
}
