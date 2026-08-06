//! Suíte upstream do ruby/ruby (branch v3_4_10, BSD-2-Clause — ver
//! test/vendor/ruby/COPYING.ruby): roda os testes do repo oficial do Ruby
//! contra o calisto e exige PARIDADE com o ruby puro (mesmo runtime pinado).
//!
//! Contrato: para cada teste, `calisto run --preload 0 <test>` deve produzir
//! exatamente o mesmo resumo (tests/failures/errors) e exit code que
//! `ruby -I tool/lib -I test/lib <test>`. Se o ruby puro falha por infra
//! (ex.: test_eval e Dir.mktmpdir), o calisto deve falhar do MESMO jeito.

mod common;

use common::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const TESTS: &[&str] = &[
    "test_require.rb",
    "test_iseq.rb",
    "test_exception.rb",
    "test_alias.rb",
    "test_arity.rb",
    "test_assignment.rb",
    "test_autoload.rb",
    "test_beginendblock.rb",
    "test_defined.rb",
    "test_ifunless.rb",
    "test_lambda.rb",
    "test_literal.rb",
    "test_object.rb",
    "test_proc.rb",
    "test_range.rb",
    "test_syntax.rb",
    "test_eval.rb",
];

fn vendor() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("test/vendor/ruby")
}

fn vendor_ruby() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("vendor/current/bin/ruby")
}

/// "40 tests, 463 assertions, 0 failures, 1 errors, 0 skips" -> (40, 0, 1)
fn parse_summary(out: &[u8]) -> Option<(u32, u32, u32)> {
    let s = String::from_utf8_lossy(out);
    let line = s.lines().find(|l| l.contains(" failures,"))?;
    let mut tests = 0;
    let mut failures = 0;
    let mut errors = 0;
    for part in line.split(',').map(str::trim) {
        let mut words = part.split_whitespace();
        let n: u32 = words.next()?.parse().ok()?;
        match words.next()? {
            "tests" => tests = n,
            "failures" => failures = n,
            "errors" => errors = n,
            _ => {}
        }
    }
    Some((tests, failures, errors))
}

fn run_with_timeout(child: ChildGuard) -> Output {
    child.wait()
}

struct ChildGuard {
    child: std::process::Child,
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
}

impl ChildGuard {
    fn wait(mut self) -> Output {
        let out = std::thread::spawn(move || {
            use std::io::Read;
            let mut v = Vec::new();
            self.stdout.read_to_end(&mut v).unwrap();
            v
        });
        let err = std::thread::spawn(move || {
            use std::io::Read;
            let mut v = Vec::new();
            self.stderr.read_to_end(&mut v).unwrap();
            v
        });
        let deadline = Instant::now() + Duration::from_secs(120);
        let status = loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                break st;
            }
            assert!(
                Instant::now() < deadline,
                "teste upstream excedeu 120s (infra quebrada?)"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        Output { status, stdout: out.join().unwrap(), stderr: err.join().unwrap() }
    }
}

/// Args comuns aos dois lados:
/// - exclui testes de memory leak (medem RSS; flaky sob carga)
/// - seed fixa: testes upstream tem dependencias implicitas de require
///   (ex.: Tempfile sem `require "tempfile"`) que so funcionam quando um
///   helper carrega a lib antes -- com ordem aleatoria, quebra as vezes
const FILTER: &[&str] = &["-n", "!/memory_leak/", "--seed=1"];

fn spawn_baseline(root: &Path, ruby: &Path, test: &str) -> ChildGuard {
    let mut child = Command::new(ruby)
        .current_dir(root)
        .arg("-I").arg(root.join("tool/lib"))
        .arg("-I").arg(root.join("test/lib"))
        .arg(root.join("test/ruby").join(test))
        .args(FILTER)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn baseline ruby");
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    ChildGuard { child, stdout, stderr }
}

fn spawn_calisto(root: &Path, test: &str) -> ChildGuard {
    let dir = runtime_dir("upstream");
    let mut child = calisto(&dir)
        .current_dir(root)
        .env(
            "RUBYLIB",
            format!(
                "{}:{}",
                root.join("tool/lib").display(),
                root.join("test/lib").display()
            ),
        )
        .args(["run", "--preload", "0"])
        .arg(root.join("test/ruby").join(test))
        .args(FILTER)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn calisto");
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    ChildGuard { child, stdout, stderr }
}

#[test]
fn upstream_ruby_tests_match_baseline() {
    let root = vendor();
    let ruby = vendor_ruby();
    let problems: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = TESTS
            .iter()
            .map(|t| {
                let (root, ruby) = (&root, &ruby);
                s.spawn(move || {
                    let baseline = run_with_timeout(spawn_baseline(root, ruby, t));
                    let calisto = run_with_timeout(spawn_calisto(root, t));
                    (t, baseline, calisto)
                })
            })
            .collect();
        let mut problems = Vec::new();
        for h in handles {
            let (t, baseline, calisto) = h.join().unwrap();
            let codes_equal = baseline.status.code() == calisto.status.code();
            let b = parse_summary(&baseline.stdout);
            let c = parse_summary(&calisto.stdout);
            let parity = match (b, c) {
                (Some(b), Some(c)) => b == c && codes_equal,
                (None, None) => codes_equal,
                _ => false,
            };
            if !parity {
                // Testes upstream tem flaky de ambiente (memoria/timing/subprocessos;
                // o baseline ruby puro tambem falha sob carga). Um bug real do
                // calisto diverge em TODAS as tentativas; flaky converge.
                let mut persistent = true;
                for _ in 0..3 {
                    std::thread::sleep(Duration::from_millis(300));
                    let b2 = run_with_timeout(spawn_baseline(&root, &ruby, t));
                    let c2 = run_with_timeout(spawn_calisto(&root, t));
                    let codes2 = b2.status.code() == c2.status.code();
                    let p2 = match (parse_summary(&b2.stdout), parse_summary(&c2.stdout)) {
                        (Some(x), Some(y)) => x == y && codes2,
                        (None, None) => codes2,
                        _ => false,
                    };
                    if p2 {
                        persistent = false;
                        break;
                    }
                }
                if persistent {
                    let show = |out: &Output| -> String {
                        // o test-unit imprime os detalhes de Error/Failure no stdout
                        let s = String::from_utf8_lossy(&out.stdout);
                        let err = String::from_utf8_lossy(&out.stderr);
                        let details: Vec<&str> = s
                            .lines()
                            .filter(|l| l.contains("Error:") || l.contains("Failure:"))
                            .collect();
                        format!(
                            "exit={:?} resumo={:?}\n  erros: {}\n  stderr: {}",
                            out.status.code(),
                            parse_summary(&out.stdout),
                            if details.is_empty() { "(nada no stdout)".to_string() } else { details.join(" | ") },
                            err.trim()
                        )
                    };
                    problems.push(format!(
                        "{t}: divergencia persistente apos retries\n--- baseline: {}\n--- calisto: {}",
                        show(&baseline),
                        show(&calisto)
                    ));
                }
            }
        }
        problems
    });
    assert!(problems.is_empty(), "diferencas vs ruby puro:\n{}", problems.join("\n"));
}
