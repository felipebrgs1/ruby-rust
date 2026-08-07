# Repository Guidelines

## Project Overview

Calisto is a Bun-like runtime for **Ruby**: a single Rust binary that embeds and manages a pinned CRuby 3.4.10, gives it near-instant startup via a warm fork-based daemon, and can bundle stdlib-only apps into a single self-contained file. No third-party gems — stdlib only, by design. Linux-only (relies on `fork`).

Status: Fases 1-2 (runtime + fast startup), Fase A (gems via
Bundler), Fase B (preload de app), Fase C (Rails mínimo), Fase D completa
(escada real: Sinatra → Rails → Maybe Finance/Sidekiq → Chatwoot/API+ActionCable),
**Fase E completa** (test/task/serve/.env/watch; daemon multi-conexão — o
risco conhecido do accept loop single-connection foi resolvido), **Fase F
completa** (build --compile com gems — pure-Ruby + **C extensions**: .so já
compilados embutidos como bytes e extraídos p/ tmpdir no runtime; marco
Sinatra + sqlite3/puma com GEM_PATH vazio), **Fase G
completa** (`exec`/`run -e`/`repl`), **Fase H completa** (`[scripts]` no
calisto.toml — `calisto run dev`/`test`/`db:migrate` no railsapp, o
package.json do Ruby), **Fase I completa** (multi-versões: seleção por
`.ruby-version`/Gemfile, daemons por versão, maybe/chatwoot nativos em 3.4.4),
**Fase J completa** (`init`/`upgrade`/`completions` — scaffold como `bun
init`, rebuild do pin via scripts/build-ruby.sh, completions bash/zsh; `calisto
run` bare roda o `[scripts] start` do calisto.toml, convenção npm/bun) e
**Fase K completa** (`add`/`remove`/`lock` — wrapper fino do bundle com o ruby
da versão certa; `calisto add sinatra` → `calisto run` ativa sem passos
manuais)
do ROADMAP.md done. Golden rspec real
validado no Chatwoot (119 examples: frio 5.0s → quente 0.70s, 7.2×); marco G:
`calisto exec sidekiq -r <app>` no Maybe processa o `CalistoProbeJob` (golden
realapps) e `calisto run -e 'puts 1+1'` quente **36ms** (marco <50ms). O
roadmap está **completo** (Fases 1-2 + A-K) — itens remanescentes são
melhorias: compilar C exts do zero no build (mkmf) e `calisto doctor`/UX.

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

**Accept loop multi-conexão (Fase E)**: o daemon roda `select` sobre o listener + conexões ativas e `waitpid WNOHANG` a cada tick — um child de longa duração (server, sidekiq, suíte lenta) NÃO bloqueia novos RUNs. Cliente morto com child rodando → TERM→KILL (por conexão, como o `wait_for` antigo). `STOP` derruba children e devolve `STATUS` aos clientes antes de sair. Child fecha o socket de controle e o listener no fork (hygiene de fds).

**`calisto test` flow** (Fase E): detecta minitest (`test/**/*_test.rb`) ou rspec (`.rspec`/`spec/**/*_spec.rb`), usa um **daemon de teste dedicado** — igual ao da app, mas com `RAILS_ENV=test`/`RACK_ENV=test` no boot e socket próprio (hash inclui sal `"test"`; o Rails fixa o env no boot, um fork do boot dev nunca enxergaria `:test`). Cada arquivo é um `RUN` (fork) no daemon quente, em paralelo (worker por CPU, teto = nº de arquivos; o accept loop multi-conexão é o que permite). `CALISTO_LOAD_PATH=test|spec` no env do RUN injeta `-I` no child (depois do `Bundler.setup`, que limpa o `$LOAD_PATH`) para `require "test_helper"`/`rails_helper` funcionar. `--watch` roda no cliente Rust (polling de mtime a cada 300ms) — fork-safe por construção, sem listen/inotify (lição do Chatwoot).

**`calisto task`** (Fase E): rake no daemon quente via shim `load Gem.bin_path("rake", "rake")` (equivale ao `bin/rake` do Rails, sem exigir binstub), gerado no dir de runtime; roda no daemon da app (dev).

**`calisto serve`** (Fase E): launcher `serve.rb` no dir de runtime → `Rack::Builder.parse_file(config.ru)` + `Rackup::Server.start` (rack 3/rackup) com fallback `Rack::Server` (rack 2); o server roda como child do fork; kill no cliente derruba via client-death kill.

**`.env`** (Fase E): parser no CLIENTE Rust (walk-up do cwd, sem sobrescrever vars existentes, suporta `#`, `export`, aspas). O env resultante propaga para o spawn do daemon (o boot da app vê `DATABASE_URL` etc.), para o `env_blob` do RUN (o script vê) e para o `--cold` (paridade cold/warm preservada — parser só no daemon divergiria). `calisto test` força `RAILS_ENV=test`/`RACK_ENV=test` por cima de qualquer `.env`.

**`calisto run -e 'code'`** (Fase G): op **`EVAL`** no daemon (mesmo formato do RUN: cwd, env_blob, code, args) — o child faz `eval(code, TOPLEVEL_BINDING, "-e", 1)` com `$0 = "-e"` (sem DATA): paridade exata com `ruby -e` em $0/ARGV/backtrace (`-e:1`)/exit codes. Múltiplos `-e` são concatenados com `"\n"` no cliente (como o ruby; `__LINE__` segue a concatenação). Cold: `ruby -rbundler/setup -e code args` — paridade cold/warm é invariante coberta no `exec.rs`. O daemon ganhou `child_enter` (bootstrap comum do child: traps default, dup stdio, hygiene de fds — `$server` global, chdir, ENV.replace, `bundler/setup`, CALISTO_LOAD_PATH) compartilhado por RUN/EVAL.

**`calisto exec <bin>`** (Fase G): shim `exec.rb` no dir de runtime, resolvido como `bundle exec` mas **sem depender de binstub**: (1) argumento que é caminho de arquivo executável (`./bin/rails`), (2) spec ativada do bundle (`Gem.loaded_specs`, dedup por nome — default gems aparecem 2×; ambiguidade entre gems DIFERENTES → erro com candidatos), (3) PATH. Binário ruby (shebang ruby) é **`load` in-process** (kernel_load do bundler: `$0` = caminho, ARGV = args) — sem shebang no PATH nem re-exec; binário nativo é `exec` direto (126/127 como bundler). Ex.: `calisto exec sidekiq -r <app>` no Maybe roda o worker como child do fork.

**`calisto run <script>`** (Fase H): `[scripts]` no calisto.toml (`dev = "bin/rails server"`, `test = "rake test"`, `db:migrate = "bin/rails db:migrate"`…). Um nome que **não é arquivo** resolve para `[scripts.NAME]` (arquivo existente sempre vence) e roda como `calisto exec` no daemon — o `exec_argv` do Fase G é compartilhado (shim `exec.rb` no dir de runtime, `load` in-process), com os args do CLI no final do comando e cwd do child na **raiz da app** (dir do calisto.toml, como bun run). `--cold` roda o shim no interpretador direto (paridade cold/warm; `run_cold` ganhou cwd explícito). Comando tokenizado shell-like no cliente (`split_command`: whitespace separa, aspas simples/duplas agrupam — sem escapes/expansão); vazio/aspas quebradas → erro claro. **`preload` é opcional** no parser (seções `[run]` e `[scripts]`): calisto.toml só com scripts NÃO vira daemon da app — `app_daemon()` (`preload.is_some()`) filtra todos os seletores de daemon (run/task/serve/exec/repl/test/status/stop/doctor); scripts não entram no hash do socket (mudar script não reinicia o daemon). Nota: `calisto run test` roda `bin/rails test` no daemon **dev** — o teste de env do railsapp falha por design (Rails.env "development" no boot congelado); é exatamente a razão do daemon de teste RAILS_ENV=test do `calisto test`.

**`calisto repl`** (Fase G): shim `repl.rb` (`require "irb"; IRB.start`), args repassados ao IRB (parse_opts do ARGV); roda como child do fork — no daemon da app (calisto.toml) é console no boot congelado; no genérico, stdlib preloaded. Foreground; kill no cliente derruba via client-death kill (como `serve`).

**`calisto init` / `upgrade` / `completions`** (Fase J): `init` escreve `calisto.toml` (`[scripts] start = "./hello.rb"`), `hello.rb` (shebang `#!/usr/bin/env ruby` + 755 — o shim do exec resolve executáveis e detecta binário ruby pelo shebang para `load` in-process, sem ruby no PATH) e `.gitignore`; nunca sobrescreve arquivo existente sem `--force`. `calisto run` **bare** com `[scripts] start` roda o start (convenção npm/bun — `calisto run` hoje aceita zero args só nesse caso; sem `start`, erro de uso de sempre). `upgrade` spawna `sh <vendor>/../scripts/build-ruby.sh` com stdio herdado (`CALISTO_BUILD_SCRIPT` override para testes) — sem versão rebuilda o pin, com versão seta `RUBY_VERSION`; versão sem sha conhecido (3.4.10/3.4.4) falha antes de spawnar; o exit code do script propaga. `completions bash|zsh` imprime o script (bash: `complete -F _calisto`, zsh: `#compdef`), flags por subcomando e `*.rb`.

Fase A: `calisto run` ativa Gemfile via Bundler com semântica de `bundle exec` — o child (fork) faz `require "bundler/setup"` (RUBYOPT não funciona: só é lido no boot do interpretador) e o cold mode passa `-rbundler/setup`. Sem instalador próprio: gems instalam com `bundle install` normal. `.ruby-version` (walk up) **seleciona a versão** (Fase I: `vendor/ruby-<v>`; não instalada → erro claro com o comando de build). **Gemfile presente (walk-up do cwd, ou `BUNDLE_GEMFILE`) desativa o preload stdlib** — preload + bundle colidiriam se o Gemfile pinar default gems em versões diferentes (ex.: base64 0.2 do pin vs 0.3 que o Sinatra 4 exige → `Gem::LoadError "already activated"`); sem preload, o `Bundler.setup` ativa o bundle num interpretador "fresco", como o `bundle exec`.

Fase B (preload de app): `calisto.toml` na raiz da app (walk-up do cwd) com `[run] preload = "entrypoint"` faz o `run` usar um **daemon dedicado da app** (socket em `<runtime>/apps/<fnv1a(app_root+preload)>`, como Spring/Zeus). O daemon da app boota com `-rbundler/setup` + cwd na raiz da app e `load`a o entrypoint no boot (preload stdlib vazio); cada RUN é fork do boot congelado. Fork-safe: conexões ActiveRecord são desconectadas após o boot (o child reconecta lazy) e o entrypoint é registrado em `$LOADED_FEATURES` — `load` não registra, e sem isso o Rails re-roda `config/environment.rb` no child via `require_environment!` (initialize! duplo → "Application has been already initialized"). Daemon stale após editar o entrypoint: `calisto stop` na app (hot reload é Fase E). `status`/`stop`/`doctor` operam no daemon da app quando o cwd está numa app.

**Wire protocol** (RESP-style over unix socket): `"<OP> <n>\r\n"` then n fields `"$<len>\r\n<data>"`. Commands: `PING` → `OK`, `STOP` → `BYE`, `RUN` → `STATUS <code>`, `EVAL` → `STATUS <code>` (idem RUN, mas evala o campo code como `ruby -e`). Fields are base64 (hand-rolled encoder, no crates).

**`calisto build` flow**: `build.rb` parses static `require`/`require_relative`/`autoload` with Ripper (the real lexer), BFS-collects project files under the root, emits a bundle where each file is evaluated via `eval(code, TOPLEVEL_BINDING, original_path, 1)` (preserves `__FILE__`/`__dir__`/`require_relative`) and a loader monkey-patches `Kernel#require`/`require_relative` against an index. Files outside the root (stdlib like `json`) are NOT bundled — delegated to real `require`. `--compile` (Fase F) também embute gems do Gemfile.lock: specs resolvidas via `Gem::Specification.find_by_name` com o GEM_PATH do app (`vendor/bundle`); `.rb` avaliados e **C extensions** embutidas como base64 no `$calisto_native` (loader extrai p/ `/tmp/calisto-native/<abs sanitizado>/` e `require` absoluto → dlopen). Nativos vêm dos require_paths (gems precompiladas, ex.: sqlite3-x86_64-linux) e dos dirs `extensions/` compilados pelo bundle install (puma/nio4r/json); requires **dinâmicos** de nativos (sqlite3: `require "sqlite3/#{RUBY_VERSION}/sqlite3_native"`) são cobertos por **pré-índice** do nome canônico (relativo ao require_path, sem extensão); o BFS resolve `.so`/`.bundle` como candidatos de require. Compilar do zero continua com o `bundle install` (sem toolchain própria). Armadilha do Ruby 3.4: `Hash#each` com bloco de aridade 1 entrega `[k, v]` — o coletor usa `each_key`. O loader pré-carrega os alvos dos `autoload` antes do arquivo registrador (bug do CRuby 3.4: autoload via `eval` dispara na definição do const) em rodadas com retry; `require_relative` de arquivo não embutido delega com caminho absoluto.

## Key Directories

| Path | Purpose |
|---|---|
| `src/` | Rust CLI (`main.rs`) + embedded Ruby daemon (`daemon/server.rb`) |
| `crates/calisto-build/` | First real workspace crate: `src/lib.rs` (spawns bundler) + `src/build.rb` (Ripper bundler, embedded) |
| `crates/calisto-{test,task,serve,sqlite,tooling,cli,runtime}/` | Planned modules, only `.gitkeep` — do not implement until they get a `Cargo.toml` |
|`test/`|Integration suite (`common/mod.rs` harness, `cli.rs`, `stdio.rs`, `daemon.rs`, `preload.rs`, `build.rs`, `ruby_upstream.rs`, `bundler.rs`, `app.rs`, `realapps.rs`, `testcmd.rs`, `exec.rs`, `scripts.rs`, `versions.rs`, `tooling.rs`, `deps.rs`), `fixtures/` (inclui `gemapp/`, `sinatraapp/` da Fase A, `preloadapp/`, `railsapp/` da Fase B/C e `maybe/`, `chatwoot/` — checkouts gitignored — dos degraus 4/5 da Fase D), `vendor/ruby/` (upstream ruby/ruby tests)|
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
./target/debug/calisto test                                     # minitest/rspec no daemon quente
./target/debug/calisto test --watch                             # re-roda ao salvar (polling, fork-safe)
./target/debug/calisto task db:migrate                          # rake no daemon quente
./target/debug/calisto serve -p 4567                            # config.ru via rackup/rack
./target/debug/calisto run -e 'puts 1+1'                        # codigo inline (ruby -e), quente
./target/debug/calisto run dev                                  # [scripts] do calisto.toml (Fase H)
./target/debug/calisto exec rake --version                      # binario de gem no daemon quente
./target/debug/calisto repl                                    # IRB no contexto da app
./target/debug/calisto build test/fixtures/buildapp/app_main.rb -o /tmp/out.rb
./target/debug/calisto init meu-app && cd meu-app && ./target/debug/calisto run   # scaffold (Fase J)
./target/debug/calisto upgrade [3.4.4]                         # rebuild do pin / build de versao
./target/debug/calisto completions bash                        # script de completions p/ stdout
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
|`src/main.rs`|CLI: `run` (`--cold`/`--time`/`--preload LIST`/`-e`), `test` (`--watch`), `task`, `serve` (`-p`/`-o`), `exec`, `repl`, `build` (`-o`/`--root`/`--compile`), `init` (`--force`), `upgrade` (`[version]`), `completions` (bash/zsh), `add`/`remove`/`lock`, `status`, `stop`, `doctor`, `help`|
|`src/daemon/server.rb`|Daemon: preload → bind (handles stale socket `EADDRINUSE`) → detach stdio to `/dev/null` → `RequestReader` (recvmsg não-bloqueante + SCM_RIGHTS) → **accept loop multi-conexão** (select + waitpid WNOHANG; client-death kill por conexão; STOP derruba children) → `child_enter` (bootstrap comum) + `start_child`/`start_child_eval` (fork, `dup_into_stdio`, `setup_data`, `CALISTO_LOAD_PATH`, eval `-e`). **Sem `require "base64"` no boot** (Fase I): decoder hand-rolled (`b64_decode`) — ativar a default gem antes do `Bundler.setup` do child/preload dispararia "already activated" quando o bundle pinar versão diferente (ex.: base64 0.2.0 do 3.4.4 vs 0.3.0 do 3.4.10) |
| `crates/calisto-build/src/build.rb` | Bundler: `walk_requires`, BFS collection, `split_end_marker`, gems do Gemfile.lock (pure + nativos `.so` p/ `$calisto_native` + pré-índice), bundle generation with loader |
| `crates/calisto-build/src/lib.rs` | `bundle(ruby, entry, out, root) -> Result<BundleStats, String>`; parses `BUNDLED <n>` |
| `scripts/build-ruby.sh` | Pin `RUBY_VERSION` (default 3.4.10; sha conhecido p/ 3.4.4 também), vendored libyaml, `vendor/ruby-<v>/` + symlink `vendor/current` (não troca ao construir versão extra) |
| `test/common/mod.rs` | Integration harness shared by all test targets |

## Runtime/Tooling Preferences

- **Ruby**: pinned 3.4.10 (sha256 `ecee2d07...9ec`), built by `scripts/build-ruby.sh` into `vendor/current/bin/ruby`. Stdlib-only; **no gems, no Bundler**. Default gem tooling (`test-unit`, `minitest`, `rake`) ships with the vendor build.
- **Rust**: edition 2021, zero deps, `[profile.release] lto+strip`.
- **Env vars** (all `CALISTO_*`): `CALISTO_RUBY` (alternate ruby), `CALISTO_PRELOAD` (stdlib preload list; `0`/`none` disables), `CALISTO_RUNTIME_DIR` (daemon socket/pid location — tests use this for isolation), `CALISTO_BUILD_SCRIPT` (upgrade: path do script de build — testes usam um fake; default `<vendor>/../scripts/build-ruby.sh`), `CALISTO_BUNDLE` (deps: binario do bundle — testes usam um fake; default `ruby -S bundle` do ruby resolvido), `CALISTO_BUNDLE_RUBY` (deps: exportado pelo client pro child — o ruby resolvido, p/ observação), `CALISTO_SOCKET`/`CALISTO_PIDFILE` (set by the client when spawning the daemon).
- Default daemon preload: `json,yaml,erb,pathname,fileutils,time,date,digest,base64,uri,net/http,ostruct,set,csv,stringio,logger,socket`.
- No Makefile, no CI, no README — do not create docs unless asked. `git` commits with `-c user.name="felipeb" -c user.email="felipeb@local"` (repo has no local git identity).

## Testing & QA

Framework: **cargo integration tests** (16 targets in `test/`, declared as `[[test]]` in `Cargo.toml`). No unit tests in `main.rs` (0 tests there is expected).

Harness (`test/common/mod.rs`): each test spawns the real binary via `env!("CARGO_BIN_EXE_calisto")` with a **unique `CALISTO_RUNTIME_DIR`** (isolated daemon per test → parallel-safe). `run_opt` pipes stdio, writes stdin, and enforces a **timeout** (kills + panics if the daemon ever holds a pipe — a known regression class). `spawn_stdout` for live-process tests (read child pid, send signals).

Coverage contract:
- `cli.rs` — commands, flags, error paths, `status`/`stop` lifecycle.
- `stdio.rs` — argv/env/cwd/stdin/exit codes/backtraces; **`cold_and_warm_agree`** (parity between `--cold` and warm daemon is a hard invariant); `__FILE__`/`__dir__`/`DATA`.
- `daemon.rs` — socket reuse, stale-socket recovery, orphan kill on client death (checks `/proc/<pid>`), signal exit codes (`SIGKILL` → 137), concurrent runs, pipeline non-hang.
- `preload.rs` — default/`0`/custom preload behavior.
- `build.rs` — bundle parity with original sources (renames the source tree away to prove self-contained), `DATA` emulation, `__FILE__`/`__dir__` preservation, stdlib delegation, bundle under `calisto run`. **Fase F**: `compile_bundle_embeds_gems_and_runs_without_bundle` (Sinatra + 10 gems, GEM_PATH vazio); `compile_embeds_c_extensions_and_runs` (puma — .so compilado pelo bundle install — roda standalone); `compile_embeds_sqlite3_c_extension` (gem precompilada com require **dinâmico** — pré-índice do nome canônico; gated em bundle install do railsapp).
- `ruby_upstream.rs` — **parity contract**: each of the 17 upstream ruby/ruby (tag v3_4_10) runtime tests must produce the same test-unit summary (tests/failures/errors) and exit code as plain `ruby -I tool/lib -I test/lib`. Uses `--seed=1` + filter `-n '!/memory_leak/'` (upstream tests have implicit `require` deps and RSS-based tests that are flaky even on pure ruby), and retries against environment flakiness. Always run upstream tests with `--preload 0`.
- `bundler.rs` — **Fase A (Gemfile activation)**: fixture `gemapp` (5 default/bundled gems, `Gemfile.lock` commitado → hermético, sem rede nem `bundle install`) prova ativação via `$LOAD_PATH`; `cold_and_warm_agree` com bundle; Gemfile com gem faltando falha como `bundle exec` (GemNotFound, script não roda); `BUNDLE_GEMFILE` env é honrado; `.ruby-version` com versão não instalada → erro claro (exit != 0) com o comando de build vs 3.4.10 (instalada) → silêncio (Fase I); golden Sinatra HTTP **gated** em `bundle install` prévio no fixture (`test/fixtures/sinatraapp`) — skipa com aviso se as gems não estiverem instaladas.
- `app.rs` — **Fase B (preload de app)**: fixture `preloadapp` (boot simulado 2s + contador, hermético) prova que o boot roda UMA vez no daemon e o 2º `run` <500ms; daemon da app isolado do genérico; `calisto.toml` inválido (entrypoint inexistente/sintaxe) → erro claro apontando o problema; golden Rails **gated** (`test/fixtures/railsapp`, Rails 8 + sqlite3): `bin/rails runner` 2º run <500ms e query `SELECT 1` reconecta no child (fork-safe). **Fase C (Rails mínimo)**: `bin/rails server` responde GET `/up` → 200 em <500ms do spawn (servidor roda como child do fork; o cliente killado derruba o Puma via client-death kill) e `bin/rails console` roda IRB no contexto da app via stdin pipe.
- `realapps.rs` — **Fase D, degraus 4 e 5** (apps reais, gated em checkout gitignored + bundle install + docker compose que o teste sobe sozinho): **degrau 4 (Maybe Finance, Rails 7.2 + Sidekiq 8)**: db:prepare, 2º comando <500ms, `CalistoProbeJob` enfileirado no Redis processado pelo worker `calisto exec sidekiq -r <app>` (Fase G — o binário da gem é resolvido no bundle ativo e carregado in-process no daemon quente; o `-r` é o require path que o CLI 8 exige), smoke HTTP `GET /sessions/new` → 200. Ajustes do fixture: pin nativo **3.4.4** (a Fase D tinha pinado 3.4.10 como workaround do pin único; a Fase I reverteu — `.ruby-version`/Gemfile 3.4.4, lock regenerado sob 3.4.4), `active_job.queue_adapter = :sidekiq` (dev default :async), `bin/sidekiq` próprio (CLI 8 exige `-r <dir>`). **degrau 5 (Chatwoot, Rails 7.1 + ~155 gems + pgvector)**: db:prepare + seed condicional, 2º comando <500ms, dev server com login (devise_token_auth POST /auth/sign_in → access-token+client), `GET /api/v1/accounts/1/conversations` autenticado → 200 e ActionCable WebSocket handshake `/cable` → 101 (lê 1 chunk — o WS não fecha). Ajustes do fixture: pin 3.4.10, `scout_apm` removido (C ext não compila no 3.4.10, `require: false`), **`config.file_watcher = ActiveSupport::FileUpdateChecker`** (sem listen/inotify — o fork do daemon herda watchers e o 2º fork morre rc=1 silencioso; watch é Fase E), migration `Taggable::Cache`→`Caching` (API mudou na acts-as-taggable-on 12), imagem postgres `pgvector/pgvector:pg16` (migration do Captain usa `CREATE EXTENSION vector`), limpa `tmp/pids/server.pid` antes do server.
- `testcmd.rs` — **Fase E (test/task/serve/.env)**: `calisto test` hermético no preloadapp (boot 2s pago UMA vez no daemon de teste; suite de 2 arquivos — um com sleep 0.6s — <1s quente, provando paralelismo de arquivos via accept loop multi-conexão; contador de boot == 1); teste falhando → exit 1; sem testes → erro claro; detecção rspec via `.rspec`/`spec/`; **golden railsapp** (minitest, gated em bundle install): `calisto test` 2 arquivos <1s quente e o teste `rails_env_test.rb` prova `Rails.env == "test"` (daemon de teste com RAILS_ENV=test no boot — o daemon dev deixaria "development"); `.env` warm == cold (paridade), aspas/`export`/não-sobrescreve; `calisto task db:migrate` idempotente e quente (<1s; limpa db/schema.rb do fixture no fim); `calisto serve` GET /up → 200 com o daemon CONTINUANDO a servir `runner` enquanto o server roda (multi-conexão).

- `exec.rs` — **Fase G (run -e/exec/repl)**: `-e` com paridade cold/warm ($0 = "-e", ARGV, backtrace `-e:1`, múltiplos `-e` concatenados com `__LINE__` 1 e 2 — paridade com o ruby); **marco <50ms** no 2º `run -e` quente (36ms medido); `-e` no daemon da app (preloadapp) roda no boot congelado sem re-boot; `exec` resolve rake no gemapp (hermético, sem binstub), cai no PATH (sh), carrega script ruby com shebang in-process ($0/ARGV), inexistente → 127 e warm <500ms no daemon da app; `repl` roda IRB como child do fork no contexto da app (boot UMA vez, `puts 1+1` → 2 via stdin pipe).
- `scripts.rs` — **Fase H (scripts no calisto.toml)**: fixture hermética `scriptsapp` (só `[scripts]`, sem preload) prova resolução + args do CLI ao final do comando + aspas agrupando + cwd na raiz da app (subdir); paridade cold/warm no script; arquivo existente vence o script de mesmo nome; script inexistente/vazio/aspas quebradas → erro claro; calisto.toml só de scripts NÃO cria daemon da app (`apps/` ausente, daemon genérico serve arquivo); `[scripts]` na `preloadapp` → script roda no daemon da app com boot UMA vez (<500ms no 2º); **golden railsapp** (gated em bundle install): `run dev -p PORT` sobe o server (GET /up → 200), `run db:migrate` idempotente (135ms quente) e `run test <filtro>` roda a suite (74ms) — `run test` completo roda `bin/rails test` no daemon dev e o teste de env falha POR DESIGN (exit 1), documentando a razão do daemon de teste.
- `versions.rs` — **Fase I (multi-versões)**: gated em `vendor/ruby-3.4.4` (RUBY_VERSION=3.4.4 scripts/build-ruby.sh): `.ruby-version` seleciona (prefixo `ruby-` tolerado) com cold/warm concordando; diretiva `ruby "3.4.4"` do Gemfile seleciona (lock vazio hermético); versão não instalada → erro claro com o build; daemons genéricos isolados por versão (`runtime_dir/ruby-3.4.4/` — stop de um não derruba o outro); daemon da app por versão (hash do socket inclui a versão — o mesmo app em 3.4.4 e default paga o boot UMA vez cada, `apps/` com 2 dirs). O gate `bundle_check` do harness resolve a versão da app (`.ruby-version`/Gemfile → bundler do vendor certo).
- `tooling.rs` — **Fase J (init/upgrade/completions)**: `calisto init` (cwd explícito no teste — nunca herdar o cwd do processo!) gera calisto.toml + hello.rb executável + .gitignore e o app roda com `calisto run` BARE (= `[scripts] start`, convenção npm/bun — `run` sem script sem `start` continua erro de uso), com `run hello.rb` (arquivo vence) e `--cold` (paridade); nunca sobrescreve sem `--force`; nome-é-arquivo/flag desconhecida → erro. `upgrade` roda o build script (fake via `CALISTO_BUILD_SCRIPT`; sem versão = pin default, com versão passa `RUBY_VERSION`), propaga o exit code do script, versão sem sha conhecido (3.4.10/3.4.4) falha ANTES de spawnar e script ausente/args demais → erro claro. `completions bash|zsh` imprimem scripts instaláveis (`complete -F _calisto calisto`/`#compdef calisto`); shell ausente/desconhecido → erro de uso.
- `deps.rs` — **Fase K (add/remove/lock)**: wrapper fino do bundle com fake via `CALISTO_BUNDLE` (hermético): args passam direto, cwd na raiz do projeto (dir do Gemfile — walk-up), `BUNDLE_GEMFILE` setado, PATH prefixado com o bin dir do ruby (trap do restart do bundler), `CALISTO_BUNDLE_RUBY` exportado e exit code propaga (FAKE_BUNDLE_RC); sem Gemfile → erro claro sugerindo `bundle init`; `.ruby-version` 3.4.4 → bundle do `vendor/ruby-3.4.4` (gated, como versions.rs).

QA rule of thumb: for any change to `run` semantics, the acceptance test is `cold_and_warm_agree` plus the upstream parity harness — if pure `ruby` and calisto diverge, it's a bug in calisto.

## Armadilhas conhecidas do ambiente

- **Bundler restart por shebang**: o ruby 3.4.10 embute bundler 2.6.9; app cujo `Gemfile.lock` pina outra versão (ex.: Chatwoot locka 2.5.16) dispara `Bundler::SelfManager#restart_with_locked_bundler_if_needed` no `require 'bundler/setup'` frio — o bundler re-executa o processo via shebang `#!/usr/bin/env ruby` e **precisa de `ruby` no PATH** (senão: `env: 'ruby': No such file`). O daemon warm não é afetado (o restart acontece no boot do daemon, que não tem shebang; o child herda o bundler já ativo). `calisto run --cold bin/rails ...` num app com bundler divergente exige `PATH` com o ruby pinado; o `calisto add/remove/lock` (Fase K) já prefixa o PATH com o bin dir do ruby resolvido.
- **Shell persistente**: `export RAILS_ENV=test`/`POSTGRES_*` no shell do dev vaza para o `cargo test` (o teste `rails_console_runs_in_app_context` espera `development` e quebra). Rodar a suíte com o ambiente limpo.
