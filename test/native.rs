//! Fase P: APIs nativas `calisto.*` — `Calisto::Hash` (sha256/blake3 em
//! Rust, SHA-NI) e `Calisto::SQLite` (binding sobre libsqlite3 do sistema).
//! Hermetico: stdlib-only, sem gems — roda no daemon generico.

mod common;
use common::*;
use std::path::Path;
use std::process::Output;

/// Vetores oficiais do BLAKE3 (BLAKE3-team/BLAKE3 test_vectors.json) — input
/// = padrao ciclico de 251 bytes (0..250). Cobre chunk unico (0/1/63/64),
/// chunk cheio com bloco final vazio (1024), chunk + resto (1025), arvore
/// multi-nivel (2048/4096/8192) e arvore alta (102400 = 100 chunks).
const BLAKE3_VECTORS: &[(usize, &str)] = &[
    (0, "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"),
    (1, "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213"),
    (63, "e9bc37a594daad83be9470df7f7b3798297c3d834ce80ba85d6e207627b7db7b"),
    (64, "4eed7141ea4a5cd4b788606bd23f46e212af9cacebacdc7d1f4c6dc7f2511b98"),
    (1024, "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7"),
    (1025, "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444"),
    (2048, "e776b6028c7cd22a4d0ba182a8bf62205d2ef576467e838ed6f2529b85fba24a"),
    (2049, "5f4d72f40d7a5f82b15ca2b2e44b1de3c2ef86c426c95c1af0b6879522563030"),
    (3072, "b98cb0ff3623be03326b373de6b9095218513e64f1ee2edd2525c7ad1e5cffd2"),
    (4096, "015094013f57a5277b59d8475c0501042c0b642e531b0a1c8f58d2163229e969"),
    (4097, "9b4052b38f1c5fc8b1f9ff7ac7b27cd242487b3d890d15c96a1c25b8aa0fb995"),
    (8192, "aae792484c8efe4f19e2ca7d371d8c467ffb10748d8a5a1ae579948f718a2a63"),
    (102400, "bc3e3d41a1146b069abffad3c0d44860cf664390afce4d9661f7902e7943e085"),
];

/// `calisto run -e` quente com o script — helper curto.
fn warm_eval(dir: &Path, code: &str) -> Output {
    run(dir, &["run", "-e", code])
}

#[test]
fn native_hash_blake3_matches_official_vectors() {
    let dir = runtime_dir("native-blake3");
    let code = r##"require "calisto/hash"
[0, 1, 63, 64, 1024, 1025, 2048, 2049, 3072, 4096, 4097, 8192, 102400].each do |n|
  data = (0...n).map { |i| i % 251 }.pack("C*")
  puts "#{n}:#{Calisto::Hash.blake3(data)}"
end"##;
    let out = warm_eval(&dir, code);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    for (n, expected) in BLAKE3_VECTORS {
        let line = format!("{n}:{expected}");
        assert!(
            stdout.lines().any(|l| l == line),
            "blake3({n}) = {expected:?} esperado, stdout:\n{stdout}"
        );
    }
}

#[test]
fn native_hash_sha256_parity_cold_warm_and_digest() {
    let dir = runtime_dir("native-sha256");
    // warm: SHA-NI; cold: fallback Digest (paridade = mesmo hexdigest)
    let code = r#"require "calisto/hash"
require "digest"
["", "abc", "x" * 63, "x" * 64, "x" * 65, "x" * 1000, "x" * 1048576].each do |s|
  raise "mismatch #{s.bytesize}" unless Calisto::Hash.sha256(s) == Digest::SHA256.hexdigest(s)
end
puts Calisto::Hash.sha256("abc")"#;
    let warm = warm_eval(&dir, code);
    assert!(warm.status.success(), "warm stderr: {}", String::from_utf8_lossy(&warm.stderr));
    let cold = run(&dir, &["run", "--cold", "-e", code]);
    assert!(cold.status.success(), "cold stderr: {}", String::from_utf8_lossy(&cold.stderr));
    assert_eq!(
        String::from_utf8_lossy(&warm.stdout),
        String::from_utf8_lossy(&cold.stdout),
        "cold/warm devem concordar (fallback Digest == SHA-NI)"
    );
    assert_eq!(
        String::from_utf8_lossy(&warm.stdout).trim(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn native_sqlite_hermetic_in_memory_db() {
    let dir = runtime_dir("native-sqlite");
    let code = r##"require "calisto/sqlite"
db = Calisto::SQLite.open(":memory:")
db.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, score REAL)")
db.execute("INSERT INTO t (name, score) VALUES (?, ?)", "calisto", 3.5)
db.execute("INSERT INTO t (name, score) VALUES (?, ?)", "ruby", 2)
raise "changes #{db.changes}" unless db.changes == 1
raise "rowid #{db.last_insert_rowid}" unless db.last_insert_rowid == 2
rows = db.execute("SELECT id, name, score FROM t ORDER BY id")
raise rows.inspect unless rows == [[1, "calisto", 3.5], [2, "ruby", 2.0]]
# prepared statement reutilizado: reset + re-bind a cada execute
stmt = db.prepare("SELECT name FROM t WHERE id = ?")
raise stmt.execute(1).inspect unless stmt.execute(1) == [["calisto"]]
raise stmt.execute(2).inspect unless stmt.execute(2) == [["ruby"]]
raise stmt.columns.inspect unless stmt.columns == ["name"]
stmt.close
raise "stmt not closed" unless stmt.closed?
# bind de nil e de tipos invalidos
db.execute("INSERT INTO t (id, name) VALUES (3, ?)", nil)
raise db.execute("SELECT name FROM t WHERE id = 3").inspect unless db.execute("SELECT name FROM t WHERE id = 3") == [[nil]]
begin
  db.execute("SELECT 1 WHERE 1 = ?", Object.new)
  raise "no type error"
rescue TypeError => e
  raise "msg #{e.message}" unless e.message.include?("cannot bind")
end
# erro de SQL vira Calisto::SQLite::Error (subclasse de StandardError)
begin
  db.execute("SELECT * FROM nonexistent")
  raise "no error"
rescue Calisto::SQLite::Error => e
  raise "msg #{e.message}" unless e.message.include?("no such table")
end
# apos o erro o db continua util — o stmt do erro nao pode ficar
# pendurado (senao o close_v2 do db ficaria adiado para sempre)
raise db.execute("SELECT 1").inspect unless db.execute("SELECT 1") == [[1]]
# statement reutilizavel que ERRA no step: o reset da proxima chamada
# limpa o estado de erro (nao pode ser finalizada no caminho de erro)
db.execute("CREATE TABLE u (x INTEGER UNIQUE)")
db.execute("INSERT INTO u VALUES (1)")
stmt2 = db.prepare("INSERT INTO u VALUES (?)")
begin
  stmt2.execute(1)
  raise "no unique error"
rescue Calisto::SQLite::Error => e
  raise "msg3 #{e.message}" unless e.message.include?("UNIQUE")
end
raise stmt2.execute(2).inspect unless stmt2.execute(2) == []
stmt2.close
# multi-statement: erro claro (escopo da API: uma statement por chamada)
begin
  db.execute("SELECT 1; SELECT 2")
  raise "no error 2"
rescue Calisto::SQLite::Error => e
  raise "msg2 #{e.message}" unless e.message.include?("multiple SQL")
end
db.close
raise "db not closed" unless db.closed?
puts Calisto::SQLite.libversion
# regressao do dfree: handles ABERTOS no exit — o GC shutdown finaliza
# via TypedData e precisa fechar o HANDLE certo (bug real: o dfree passava
# o ponteiro do box {stmt,db} como sqlite3_stmt*/sqlite3* — lixo no sqlite
# -> SIGSEGV/abort no exit; so nao crashava porque os smokes fechavam tudo
# explicitamente antes)
db3 = Calisto::SQLite.open(":memory:")
db3.execute("CREATE TABLE t3 (x)")
s3 = db3.prepare("INSERT INTO t3 VALUES (1)")
s3.execute
# sem close — dfree no shutdown
print "OPEN-AT-EXIT-OK""##;
    let out = warm_eval(&dir, code);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let v = String::from_utf8_lossy(&out.stdout);
    let v = v.trim();
    assert!(v.starts_with("3."), "libversion = {v:?}");
}

#[test]
fn native_sqlite_cold_raises_clear_load_error() {
    let dir = runtime_dir("native-sqlite-cold");
    // sqlite e nativo do daemon — --cold nao tem como servir (sem gem)
    let out = run(&dir, &["run", "--cold", "-e", r#"require "calisto/sqlite""#]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("calisto/sqlite"),
        "stderr deve explicar o problema: {stderr}"
    );
}

#[test]
fn native_hash_available_in_app_daemon_preload() {
    // O boot do daemon da app roda -rbundler/setup (limpa o $LOAD_PATH) —
    // o unshift do dir nativo precisa acontecer DEPOIS disso, ou o
    // entrypoint nao consegue `require "calisto/hash"` no boot (bug real:
    // o bloco nativo rodava antes do loop de -r).
    let dir = runtime_dir("native-app-preload");
    let app = dir.join("app");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("calisto.toml"), "[run]\npreload = \"boot.rb\"\n").unwrap();
    std::fs::write(
        app.join("boot.rb"),
        "require \"calisto/hash\"\nraise \"Calisto::Hash ausente no boot\" unless defined?(Calisto::Hash)\n",
    )
    .unwrap();
    // 1º comando sobe o daemon da app (o boot roda o entrypoint)
    let out = run_opt(
        &dir,
        RunOpts {
            args: &["run", "-e", "puts Calisto::Hash.sha256(\"boot\")"],
            env: &[],
            stdin: None,
            cwd: Some(&app),
            timeout: 30,
        },
    );
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    // sha256("boot") — o mesmo digest em qualquer execucao
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "4509beb0ab401d71fa4a5cd94a55c9a74f13332776ae4019c5bfc4c2005157ff"
    );
}

#[test]
fn native_hash_sha256_benchmark_100mb_beats_digest() {
    // Marco da Fase P: sha256 de 100MB >= 3x o Digest::SHA256 da stdlib
    // (medido 6.9x com SHA-NI; sem a extensao da CPU o escalar empata e o
    // teste pula — o dispatch e documentado no crate).
    let has_sha_ni = std::fs::read_to_string("/proc/cpuinfo")
        .map(|s| s.lines().any(|l| l.starts_with("flags") && l.contains("sha_ni")))
        .unwrap_or(false);
    if !has_sha_ni {
        eprintln!("native: skip benchmark (sem sha_ni no /proc/cpuinfo)");
        return;
    }
    let dir = runtime_dir("native-bench");
    let code = r#"require "calisto/hash"
require "digest"
data = "x" * (100 * 1024 * 1024)
t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC)
h = Calisto::Hash.sha256(data)
t1 = Process.clock_gettime(Process::CLOCK_MONOTONIC)
d = Digest::SHA256.hexdigest(data)
t2 = Process.clock_gettime(Process::CLOCK_MONOTONIC)
raise "mismatch" unless h == d
printf "ratio: %.2f\n", (t2 - t1) / (t1 - t0)"#;
    let out = warm_eval(&dir, code);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let ratio: f64 = stdout
        .lines()
        .find_map(|l| l.strip_prefix("ratio: ").and_then(|r| r.trim().parse().ok()))
        .expect("ratio no stdout");
    // O assert so vale no artefato release: no debug o Rust (e o hash nativo)
    // roda sem otimizacao e o Digest (C otimizado) ganha — o benchmark mede
    // o binario que se distribui (`cargo test --release --test native`).
    if cfg!(debug_assertions) {
        eprintln!("native: benchmark em build debug (ratio {ratio:.2}x) — rodar --release para o marco");
    } else {
        assert!(ratio >= 3.0, "calisto deve ser >=3x o Digest::SHA256 (medido: {ratio:.2}x)");
    }
}
