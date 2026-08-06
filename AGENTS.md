# Repository Guidelines

## Project Overview

Calisto is a Bun-like runtime for **Ruby**: a single Rust binary that embeds and manages a pinned CRuby 3.4.10, gives it near-instant startup via a warm fork-based daemon, and can bundle stdlib-only apps into a single self-contained file. No third-party gems — stdlib only, by design. Linux-only (relies on `fork`).

Status: Fases 1-2 (runtime + fast startup, bundler stdlib-only), Fase A (gems via
Bundler), Fase B (preload de app), Fase C (Rails mínimo) e Fase D completa
(escada real: Sinatra → Rails → Maybe Finance/Sidekiq → Chatwoot/API+ActionCable)
do ROADMAP.md done. O roadmap agora mira um **Bun por inteiro** (Fases E-K):
E (test/task/serve/.env/watch), F (build --compile com gems), G (exec/-e/repl),
H (scripts no calisto.toml), I (multi-versões de ruby), J (init/upgrade),
K (deps add/remove). Next: **Fase E** — `calisto test` (crates
`calisto-{test,task,serve,sqlite,tooling}` existem como scaffolds vazios).
Risco conhecido pendente: daemon single-connection (server de longa duração
bloqueia novos RUNs — precisa accept loop multi-conexão antes da Fase E).

## Architecture & Data Flow

```
src/main.rs (Rust CLI, zero deps)
  ├─ include_str! embeds src/daemon/server.rb (Ruby)
  ├─ spawns pinned CRuby (vendor/current/bin/ruby) running the daemon
  │    daemon: preload stdlib → bind unix socket → accept loop → fork 1 child per RUN
  │    stdio (stdin/stdout/stderr) passed per-request via SCM_RIGHTS over the socket
  └─ calisto build → crates/calisto-build: spawns build.rb (Ruby, Ripper) in pinned ruby
       → emits a single-file bundle with a loader that intercepts require/require_relative
```

**`calisto run` flow**: client connects to daemon socket (spawning the daemon on first use) → sends `RUN` with base64 fields (cwd, env, script, args) + its own stdio fds via `SCM_RIGHTS` → daemon `fork()`s a child → child dup2's the fds, chdirs, sets `$0`/`ARGV`, requires `bundler/setup` (no-op fora de Gemfile; ativa o Gemfile do cwd como `bundle exec`), `load`s the script → daemon `waitpid`s and replies `STATUS <code>` → client exits with that code. Child output streams live (real fds, not pipes through the daemon).

Fase A: `calisto run` ativa Gemfile via Bundler com semântica de `bundle exec` — o child (fork) faz `require "bundler/setup"` (RUBYOPT não funciona: só é lido no boot do interpretador) e o cold mode passa `-rbundler/setup`. Sem instalador próprio: gems instalam com `bundle install` normal. `.ruby-version` divergente do pin → warning sem abortar. **Gemfile presente (walk-up do cwd, ou `BUNDLE_GEMFILE`) desativa o preload stdlib** — preload + bundle colidiriam se o Gemfile pinar default gems em versões diferentes (ex.: base64 0.2 do pin vs 0.3 que o Sinatra 4 exige → `Gem::LoadError "already activated"`); sem preload, o `Bundler.setup` ativa o bundle num interpretador "fresco", como o `bundle exec`.

Fase B (preload de app): `calisto.toml` na raiz da app (walk-up do cwd) com `[run] preload = "entrypoint"` faz o `run` usar um **daemon dedicado da app** (socket em `<runtime>/apps/<fnv1a(app_root+preload)>`, como Spring/Zeus). O daemon da app boota com `-rbundler/setup` + cwd na raiz da app e `load`a o entrypoint no boot (preload stdlib vazio); cada RUN é fork do boot congelado. Fork-safe: conexões ActiveRecord são desconectadas após o boot (o child reconecta lazy) e o entrypoint é registrado em `$LOADED_FEATURES` — `load` não registra, e sem isso o Rails re-roda `config/environment.rb` no child via `require_environment!` (initialize! duplo → "Application has been already initialized"). Daemon stale após editar o entrypoint: `calisto stop` na app (hot reload é Fase E). `status`/`stop`/`doctor` operam no daemon da app quando o cwd está numa app.

**Wire protocol** (RESP-style over unix socket): `"<OP> <n>\r\n"` then n fields `"$<len>\r\n<data>"`. Commands: `PING` → `OK`, `STOP` → `BYE`, `RUN` → `STATUS <code>`. Fields are base64 (hand-rolled encoder, no crates).

**`calisto build` flow**: `build.rb` parses static `require`/`require_relative` with Ripper (the real lexer), BFS-collects project files under the root, emits a bundle where each file is evaluated via `eval(code, TOPLEVEL_BINDING, original_path, 1)` (preserves `__FILE__`/`__dir__`/`require_relative`) and a loader monkey-patches `Kernel#require`/`require_relative` against an index. Files outside the root (stdlib like `json`) are NOT bundled — delegated to real `require`.

## Key Directories

| Path | Purpose |
|---|---|
| `src/` | Rust CLI (`main.rs`) + embedded Ruby daemon (`daemon/server.rb`) |
| `crates/calisto-build/` | First real workspace crate: `src/lib.rs` (spawns bundler) + `src/build.rb` (Ripper bundler, embedded) |
| `crates/calisto-{test,task,serve,sqlite,tooling,cli,runtime}/` | Planned modules, only `.gitkeep` — do not implement until they get a `Cargo.toml` |
| `test/` | Integration suite (`common/mod.rs` harness, `cli.rs`, `stdio.rs`, `daemon.rs`, `preload.rs`, `build.rs`, `ruby_upstream.rs`, `bundler.rs`, `app.rs`, `realapps.rs`), `fixtures/` (inclui `gemapp/`, `sinatraapp/` da Fase A, `preloadapp/`, `railsapp/` da Fase B/C e `maybe/`, `chatwoot/` — checkouts gitignored — dos degraus 4/5 da Fase D), `vendor/ruby/` (upstream ruby/ruby tests) |
| `scripts/` | `build-ruby.sh` — builds the pinned CRuby |
| `examples/` | `hello.rb` (preload smoke), `bench.rb` (stdlib workload for `--time`) |
| `vendor/` | Pinned CRuby install + sources. **Gitignored** — never commit; reproduce with `scripts/build-ruby.sh` |

## Development Commands

```bash
scripts/build-ruby.sh              # REQUIRED once: builds pinned CRuby 3.4.10 + vendored libyaml (idempotent)
cargo build                        # debug binary: target/debug/calisto
cargo build --release              # lto+strip release
cargo test                         # full suite (~10s; upstream parity runs 17 ruby/ruby files)
cargo test --test ruby_upstream    # just the ruby/ruby parity harness

# smoke
./target/debug/calisto run examples/hello.rb
./target/debug/calisto run --cold --time examples/hello.rb     # baseline: cold interpreter
./target/debug/calisto build test/fixtures/buildapp/app_main.rb -o /tmp/out.rb
./target/debug/calisto status | stop | doctor
```

**⚠️ After editing `src/daemon/server.rb` or `crates/calisto-build/src/build.rb`, rebuild** — they are embedded via `include_str!`. A stale release binary silently ships the old daemon (this bit us once: tests appeared 11× faster because the autorunner never ran).

## Code Conventions & Common Patterns

**Rust** (`src/main.rs`, `crates/calisto-build/src/lib.rs`):
- Zero external dependencies. Raw FFI for `sendmsg`/`signal` via `unsafe extern "C"` + `#[repr(C)]` structs (`SOL_SOCKET`/`SCM_RIGHTS` = 1), hand-rolled base64, RESP framing.
- Errors: `Result<T, String>` with `format!` (no anyhow/thiserror); CLI commands return `i32` and `main` does `std::process::exit(code)`. User-facing errors: `eprintln!("calisto: ...")` + `return 1`; warnings prefixed `calisto: warning:`.
- `main()` calls `reset_sigpipe()` first (restores `SIG_DFL` — Rust ignores SIGPIPE by default, which panics on `calisto doctor | head`).
- Integration tests must use the `test/common/mod.rs` harness (see Testing), not raw `Command`.

**Ruby** (daemon, bundler, fixtures):
- `# frozen_string_literal: true` in every file.
- `rescue SystemExit; raise` — **never** `rescue SystemExit => e; exit(e.status)`: re-exiting runs `at_exit` hooks twice and breaks test-unit/autorunner (real bug found via upstream suite).
- `rescue Exception` only with `# rubocop:disable Lint/RescueException -- mimic ruby script`.
- `warn` for notices, `abort` for invalid usage, `rescue nil` for best-effort cleanup.
- Parsing Ruby source = Ripper only (`Ripper.lex` for `__END__`, `Ripper.sexp` for requires) — never regex on code.
- Comments are mostly **pt-BR** (code/identifiers in English) — keep that convention.

**Cross-cutting**: any behavior claim about `run` must match `ruby <script>` semantics (exit codes, signals → 128+n, backtraces without daemon frames, `DATA`/`__END__`, `at_exit`).

## Important Files

| File | Role |
|---|---|
| `src/main.rs` | CLI: `run` (`--cold`/`--time`/`--preload LIST`), `build` (`-o`/`--root`), `status`, `stop`, `doctor`, `help` |
| `src/daemon/server.rb` | Daemon: preload → bind (handles stale socket `EADDRINUSE`) → detach stdio to `/dev/null` → `RequestReader` (recvmsg + SCM_RIGHTS) → `handle_run` (fork, `dup_into_stdio`, `setup_data`, `wait_for` with client-death kill) |
| `crates/calisto-build/src/build.rb` | Bundler: `walk_requires`, BFS collection, `split_end_marker`, bundle generation with loader |
| `crates/calisto-build/src/lib.rs` | `bundle(ruby, entry, out, root) -> Result<BundleStats, String>`; parses `BUNDLED <n>` |
| `scripts/build-ruby.sh` | Pin `RUBY_VERSION=3.4.10` + sha256, vendored libyaml, `vendor/current` symlink |
| `test/common/mod.rs` | Integration harness shared by all test targets |

## Runtime/Tooling Preferences

- **Ruby**: pinned 3.4.10 (sha256 `ecee2d07...9ec`), built by `scripts/build-ruby.sh` into `vendor/current/bin/ruby`. Stdlib-only; **no gems, no Bundler**. Default gem tooling (`test-unit`, `minitest`, `rake`) ships with the vendor build.
- **Rust**: edition 2021, zero deps, `[profile.release] lto+strip`.
- **Env vars** (all `CALISTO_*`): `CALISTO_RUBY` (alternate ruby), `CALISTO_PRELOAD` (stdlib preload list; `0`/`none` disables), `CALISTO_RUNTIME_DIR` (daemon socket/pid location — tests use this for isolation), `CALISTO_SOCKET`/`CALISTO_PIDFILE` (set by the client when spawning the daemon).
- Default daemon preload: `json,yaml,erb,pathname,fileutils,time,date,digest,base64,uri,net/http,ostruct,set,csv,stringio,logger,socket`.
- No Makefile, no CI, no README — do not create docs unless asked. `git` commits with `-c user.name="felipeb" -c user.email="felipeb@local"` (repo has no local git identity).

## Testing & QA

Framework: **cargo integration tests** (6 targets in `test/`, declared as `[[test]]` in `Cargo.toml`). No unit tests in `main.rs` (0 tests there is expected).

Harness (`test/common/mod.rs`): each test spawns the real binary via `env!("CARGO_BIN_EXE_calisto")` with a **unique `CALISTO_RUNTIME_DIR`** (isolated daemon per test → parallel-safe). `run_opt` pipes stdio, writes stdin, and enforces a **timeout** (kills + panics if the daemon ever holds a pipe — a known regression class). `spawn_stdout` for live-process tests (read child pid, send signals).

Coverage contract:
- `cli.rs` — commands, flags, error paths, `status`/`stop` lifecycle.
- `stdio.rs` — argv/env/cwd/stdin/exit codes/backtraces; **`cold_and_warm_agree`** (parity between `--cold` and warm daemon is a hard invariant); `__FILE__`/`__dir__`/`DATA`.
- `daemon.rs` — socket reuse, stale-socket recovery, orphan kill on client death (checks `/proc/<pid>`), signal exit codes (`SIGKILL` → 137), concurrent runs, pipeline non-hang.
- `preload.rs` — default/`0`/custom preload behavior.
- `build.rs` — bundle parity with original sources (renames the source tree away to prove self-contained), `DATA` emulation, `__FILE__`/`__dir__` preservation, stdlib delegation, bundle under `calisto run`.
- `ruby_upstream.rs` — **parity contract**: each of the 17 upstream ruby/ruby (tag v3_4_10) runtime tests must produce the same test-unit summary (tests/failures/errors) and exit code as plain `ruby -I tool/lib -I test/lib`. Uses `--seed=1` + filter `-n '!/memory_leak/'` (upstream tests have implicit `require` deps and RSS-based tests that are flaky even on pure ruby), and retries against environment flakiness. Always run upstream tests with `--preload 0`.
- `bundler.rs` — **Fase A (Gemfile activation)**: fixture `gemapp` (5 default/bundled gems, `Gemfile.lock` commitado → hermético, sem rede nem `bundle install`) prova ativação via `$LOAD_PATH`; `cold_and_warm_agree` com bundle; Gemfile com gem faltando falha como `bundle exec` (GemNotFound, script não roda); `BUNDLE_GEMFILE` env é honrado; `.ruby-version` divergente → warning (exit 0) vs 3.4.10 → silêncio; golden Sinatra HTTP **gated** em `bundle install` prévio no fixture (`test/fixtures/sinatraapp`) — skipa com aviso se as gems não estiverem instaladas.
- `app.rs` — **Fase B (preload de app)**: fixture `preloadapp` (boot simulado 2s + contador, hermético) prova que o boot roda UMA vez no daemon e o 2º `run` <500ms; daemon da app isolado do genérico; `calisto.toml` inválido (entrypoint inexistente/sintaxe) → erro claro apontando o problema; golden Rails **gated** (`test/fixtures/railsapp`, Rails 8 + sqlite3): `bin/rails runner` 2º run <500ms e query `SELECT 1` reconecta no child (fork-safe). **Fase C (Rails mínimo)**: `bin/rails server` responde GET `/up` → 200 em <500ms do spawn (servidor roda como child do fork; o cliente killado derruba o Puma via client-death kill) e `bin/rails console` roda IRB no contexto da app via stdin pipe.
- `realapps.rs` — **Fase D, degraus 4 e 5** (apps reais, gated em checkout gitignored + bundle install + docker compose que o teste sobe sozinho): **degrau 4 (Maybe Finance, Rails 7.2 + Sidekiq 8)**: db:prepare, 2º comando <500ms, `CalistoProbeJob` enfileirado no Redis processado pelo worker `bin/sidekiq` (child do fork), smoke HTTP `GET /sessions/new` → 200. Ajustes do fixture: pin 3.4.10, `active_job.queue_adapter = :sidekiq` (dev default :async), `bin/sidekiq` próprio (CLI 8 exige `-r <dir>`). **degrau 5 (Chatwoot, Rails 7.1 + ~155 gems + pgvector)**: db:prepare + seed condicional, 2º comando <500ms, dev server com login (devise_token_auth POST /auth/sign_in → access-token+client), `GET /api/v1/accounts/1/conversations` autenticado → 200 e ActionCable WebSocket handshake `/cable` → 101 (lê 1 chunk — o WS não fecha). Ajustes do fixture: pin 3.4.10, `scout_apm` removido (C ext não compila no 3.4.10, `require: false`), **`config.file_watcher = ActiveSupport::FileUpdateChecker`** (sem listen/inotify — o fork do daemon herda watchers e o 2º fork morre rc=1 silencioso; watch é Fase E), migration `Taggable::Cache`→`Caching` (API mudou na acts-as-taggable-on 12), imagem postgres `pgvector/pgvector:pg16` (migration do Captain usa `CREATE EXTENSION vector`), limpa `tmp/pids/server.pid` antes do server.

QA rule of thumb: for any change to `run` semantics, the acceptance test is `cold_and_warm_agree` plus the upstream parity harness — if pure `ruby` and calisto diverge, it's a bug in calisto.
