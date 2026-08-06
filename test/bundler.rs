//! Fase A: ativacao do Gemfile via Bundler (semantica de `bundle exec`).
//!
//! O fixture `gemapp` usa 5 gems default/bundled do vendor build (minitest,
//! test-unit, rake, rdoc, csv), entao o golden test roda hermetico: sem rede,
//! sem `bundle install` — o Bundler resolve as gems ja presentes no GEM_HOME
//! do pin 3.4.10 e o lock commitado fixa as versoes.

use std::path::Path;
use std::process::Output;

use common::{fixture, run_opt, runtime_dir, RunOpts};

mod common;

fn gemapp_run(dir: &Path, args: &[&str]) -> Output {
    run_opt(
        dir,
        RunOpts {
            args,
            env: &[],
            stdin: None,
            cwd: Some(&fixture("gemapp")),
            timeout: 30,
        },
    )
}

#[test]
fn gemfile_is_activated_warm_and_cold() {
    let dir = runtime_dir("bundler");
    let warm = gemapp_run(&dir, &["run", "app.rb"]);
    let cold = gemapp_run(&dir, &["run", "--cold", "app.rb"]);
    assert!(warm.status.success() && cold.status.success());
    let out = String::from_utf8_lossy(&warm.stdout);
    for line in [
        "gemfile=Gemfile",
        "minitest_in_loadpath=true",
        "testunit_in_loadpath=true",
        "rake_in_loadpath=true",
        "rdoc_in_loadpath=true",
        "csv_rows=2",
    ] {
        assert!(out.contains(line), "bundler nao ativou o Gemfile? falta {line}: {out}");
    }
    assert_eq!(
        warm.stdout, cold.stdout,
        "cold e warm devem produzir a mesma saida com Gemfile"
    );
    assert_eq!(warm.status.code(), cold.status.code());
    let _ = common::stop(&dir);
}

#[test]
fn gemfile_with_missing_gem_fails_like_bundle_exec() {
    // Gemfile que nao resolve: mesmo erro do `bundle exec` (GemNotFound), e o
    // script nem chega a rodar. Delegamos o erro ao Bundler, sem instalador.
    let dir = runtime_dir("missinggem");
    let app = fixture("gemapp").join("app.rb");
    let project = dir.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("Gemfile"),
        "source \"https://rubygems.org\"\ngem \"calisto-gem-inexistente-xyz\"\n",
    )
    .unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", app.to_str().unwrap()],
            env: &[],
            stdin: None,
            cwd: Some(&project),
            timeout: 30,
        },
    );
    assert_ne!(out.status.code(), Some(0), "Gemfile quebrado deve falhar");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("calisto-gem-inexistente-xyz"),
        "erro deve nomear a gem faltando: {err}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("gemfile="),
        "script nao deve rodar com bundle quebrado"
    );
    let _ = common::stop(&dir);
}

#[test]
fn bundle_gemfile_env_is_honored() {
    // BUNDLE_GEMFILE (como em `bundle exec --gemfile`): roda de um cwd sem
    // Gemfile apontando explicitamente para o Gemfile do app.
    let dir = runtime_dir("bgenv");
    let app = fixture("gemapp");
    let cwd = dir.join("elsewhere");
    std::fs::create_dir_all(&cwd).unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", app.join("app.rb").to_str().unwrap()],
            env: &[("BUNDLE_GEMFILE", app.join("Gemfile").to_str().unwrap())],
            stdin: None,
            cwd: Some(&cwd),
            timeout: 30,
        },
    );
    assert!(out.status.success());
    let out_s = String::from_utf8_lossy(&out.stdout);
    assert!(
        out_s.contains("minitest_in_loadpath=true"),
        "BUNDLE_GEMFILE deve ativar o bundle do app: {out_s}"
    );
    let _ = common::stop(&dir);
}

#[test]
fn ruby_version_mismatch_warns_but_runs() {
    let dir = runtime_dir("rvwarn");
    let script = dir.join("ver.rb");
    std::fs::write(&script, "puts :ok\n").unwrap();

    // .ruby-version divergente -> warning no stderr, exit 0 (nao aborta)
    let mismatch = dir.join("mismatch");
    std::fs::create_dir_all(&mismatch).unwrap();
    std::fs::write(mismatch.join(".ruby-version"), "3.2.1\n").unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "--cold", script.to_str().unwrap()],
            env: &[],
            stdin: None,
            cwd: Some(&mismatch),
            timeout: 30,
        },
    );
    assert_eq!(out.status.code(), Some(0));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("3.2.1") && err.contains(".ruby-version"),
        "warn esperado no stderr: {err}"
    );

    // .ruby-version 3.4.10 (com prefixo `ruby-`) -> silencioso
    let ok_dir = dir.join("match");
    std::fs::create_dir_all(&ok_dir).unwrap();
    std::fs::write(ok_dir.join(".ruby-version"), "ruby-3.4.10\n").unwrap();
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "--cold", script.to_str().unwrap()],
            env: &[],
            stdin: None,
            cwd: Some(&ok_dir),
            timeout: 30,
        },
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("warning"),
        "versao igual nao deve avisar"
    );
}

#[test]
fn sinatra_app_serves_http() {
    // Golden test do roadmap: app Sinatra rodando via `calisto run` + HTTP.
    // Gated: exige `bundle install` previo no fixture (rede) — sem as gems
    // instaladas o teste skipa com aviso em vez de falhar a suite.
    let dir = runtime_dir("sinatra");
    let app = fixture("sinatraapp");
    let vendor_bin = Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/current/bin");
    let check = std::process::Command::new(vendor_bin.join("bundle"))
        .env("PATH", format!("{}:{}", vendor_bin.display(), env!("PATH")))
        .arg("check")
        .current_dir(&app)
        .output();
    match check {
        Ok(out) if out.status.success() => {}
        _ => {
            eprintln!(
                "SKIP sinatra_app_serves_http: rode `bundle install` em test/fixtures/sinatraapp"
            );
            return;
        }
    }

    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let mut child = common::calisto(&dir)
        .args(["run", "app.rb"])
        .env("PORT", port.to_string())
        .current_dir(&app)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn calisto");

    // espera o servidor aceitar conexao
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut stream = loop {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("servidor sinatra nao subiu: {e}");
            }
        }
    };
    use std::io::{Read, Write};
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: calisto.test\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut body = String::new();
    stream.read_to_string(&mut body).unwrap();
    assert!(
        body.contains("200") && body.contains("hello from sinatra"),
        "resposta esperada do sinatra: {body}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = common::stop(&dir);
}
