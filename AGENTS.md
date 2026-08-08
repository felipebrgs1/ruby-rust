# Repository Guidelines

## Project Overview

Calisto is a Bun-like runtime for **Ruby**: a single Rust binary that embeds and manages a pinned CRuby 3.4.10, gives it near-instant startup via a warm fork-based daemon, and can bundle stdlib-only apps into a single self-contained file. No third-party gems — stdlib only, by design. Linux-only (relies on `fork`).

Status: Fases 1-2 (runtime + fast startup), Fase A (gems via
C completa
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
validado no Chatwoot (119 examples: frio 5.0s → quente 0.70s, 7.2×); marco
G: `calisto exec sidekiq -r <app>` no Maybe processa o `CalistoProbeJob` (golden
realapps) e `calisto run -e 'puts 1+1'` quente **36ms** (marco <50ms). Ciclo
runtime (Fases L-Q): **Fase L completa** (CRuby embutido via libruby dlopen —
daemon in-process com accept loop em Rust; o `server.rb` legado morreu na
Fase S) e **Fase M completa** (memória/CoW: `[run]`
compact` — GC.start+GC.compact pós-boot, default on no daemon de app,
`CALISTO_COMPACT` sobrepõe; `calisto doctor` mede RSS/Pss/Shared_Clean/
Private_Dirty do daemon e de um child de probe via smaps_rollup; marcos:
Private_Dirty do child do Chatwoot **−46%**; `MALLOC_ARENA_MAX=2` no spawn) e
**Fase N completa** (YJIT quente no fork — `[run] yjit` boota o daemon com
`--yjit` e `[run] warmup` roda um script de aquecimento pós-boot ANTES da
compactação/bind; o child herda o código compilado — páginas CoW; marco:
1º request `/cpu` do railsapp 119–188ms sem warmup → 6–13ms com warmup via
Puma real em memória no daemon — Integration::Session não serve, o
HostAuthorization devolve 403 antes do hot path. Fixes do caminho: `$0`/
`$PROGRAM_NAME` no child usam slot próprio sem o setter do CRuby
(set_arg0→setproctitle corrompia a heap: `argv_env_len` do boot mistura
argv da heap com env da stack → strlcpy+zeroing gigantes; o teste
`concurrent_runs_serialize_through_daemon` pegava 8 NULs no script path),
timer thread da VM parada antes do fork (`rb_thread_stop/start_timer_thread`
via .symtab da libruby — deadlock do sched.lock) e fork-safety do stdio do
glibc via pthread_atfork) e **Fase P completa** (APIs nativas `calisto.*` —
`rb_define_method` via FFI no boot do daemon: `Calisto::Hash.sha256/blake3`
em Rust puro — blake3 validado contra os vetores oficiais, sha256 com
SHA-NI — e `Calisto::SQLite` sobre a libsqlite3 do sistema com TypedData;
shims `calisto/sqlite.rb`+`calisto/hash.rb` no dir de runtime injetado no
`$LOAD_PATH` do child pós-Bundler.setup; cold: hash cai no fallback Digest,
sqlite levanta LoadError claro; benchmark sha256 100MB: **6.9×** o
Digest::SHA256). **Fase O fechada com decisão documentada** (spike criu:
dump unprivileged exige userns+caps dropadas; restore exige pid ns próprio
+ proc privado = mini-container rootless; restore duplica o RSS — criu
exige privilégio demais para o alvo dev laptop; o gap do 1º comando
pós-boot fica aceito e o hash do socket da app já é o gancho de
invalidação reutilizável), **Fase Q completa** (distribuição:
`scripts/release.sh` monta `calisto-linux-x86_64.tar.gz` + tarballs por
ruby com sha256; `install.sh` curl|sh → `~/.calisto` + shim; `calisto
upgrade` BAIXA rubies pré-compilados quando não há scripts/ (instalação
portátil; `--source` força o build; `CALISTO_UPGRADE_URL` nos testes);
`CALISTO_HOME` vira a base do vendor_root mantendo o walk-up do checkout;
workflow `.github/workflows/release.yml` publica o release a cada tag) e
**Fase R completa** (paridade de CLI: `calisto run -I/-r/-w/-W0..2/-c/-E`
+ formas anexadas `-Ilib`/`--`; flags do CHILD via `CALISTO_RUN_FLAGS` no
env_blob — sem mudança de protocolo, o daemon aplica
pós-Bundler.setup; `-c` compila sem executar e pula requires como
o ruby; `-v`/`--version` imprimem a descrição da VM do ruby resolvido —
`test/runflags.rs` 16 testes com paridade cold/warm; `-n/-p/-a/...` viram
não-fazer documentado) e **Fase S completa** (runtime 100% Rust — `server.rb`
deletado, modo único de daemon embutido; 3.4.4 rebuildado com
`--enable-shared` roda embutido (pidfile → exe == calisto, gated em
versions.rs); ruby sem libruby.so → **erro claro** com o comando de rebuild;
goldens maybe/chatwoot **ativos** sob o daemon Rust — marco de compactação do
Chatwoot revalidado: Private_Dirty do child **−58%** (era −46% no legado)) e
**Fase T completa** (APIs nativas novas: `Calisto::Hash.xxh64` — o
`Bun.hash`, hash não-cripto 37× o Digest::SHA256 em 100MB — e os codecs
`Calisto::Base64`/`Calisto::URL`/`Calisto::HTML` com paridade exata da
stdlib, cold/warm concordando via shims com fallback puro).
Ciclo L–T fechado; próximos candidatos: degraus reais com o instalado,
snapshot gated se o cenário de privilégio mudar.

## Architecture & Data Flow

```
src/main.rs (Rust CLI, zero deps — dispatch; lógica em módulos por domínio)
  ├─ commands/run.rs, test.rs, task.rs, serve.rs, exec.rs, repl.rs,
  │    build.rs, tooling.rs, deps.rs, doctor.rs (um módulo por comando)
  ├─ daemon.rs + child.rs (daemon embutido Fase L + child do fork)
  ├─ protocol.rs (RESP-style + SCM_RIGHTS), runtime.rs, appconfig.rs,
  │    shims.rs, base64.rs
  ├─ spawns the daemon: EMBEDDED — o próprio binário calisto com a
  │    VM CRuby in-process (dlopen da libruby.so.<v> via crates/calisto-ruby,
  │    RTLD_GLOBAL, símbolos por dlsym; `calisto daemon --internal [-r<gem>]`
  │    — boot via ruby_options(["-e",""]) = process_options completo do CLI;
  │    accept loop 100% RUST: poll 10ms, recvmsg SCM_RIGHTS, fork por RUN/EVAL,
  │    waitpid WNOHANG, client-death kill, STOP). Modo único desde a Fase S:
  │    ruby sem libruby.so → erro claro com o comando de rebuild.
  │    daemon: preload stdlib → bind unix socket → accept loop → fork 1 child per RUN
```

**`calisto run` flow**: client connects to daemon socket (spawning the daemon on first use) → sends `RUN` with base64 fields (cwd, env, script, args) + its own stdio fds via `SCM_RIGHTS` → daemon `fork()`s a child → child (RUST): `rb_thread_atfork` (obrigatório — a timer thread da VM não sobrevive ao fork e o cleanup pendura), dup2 dos fds, chdir, ENV.replace (clearenv+setenv) → bootstrap Ruby sob rb_protect (`Bundler.setup`, CALISTO_LOAD_PATH, $0/ARGV via C API, setup_data) → RUN: `Kernel#load` (rb_f_load — CWD p/ paths relativos; rb_load C-level resolve só $LOAD_PATH) / EVAL: cadeia iseq do CLI (`rb_parser_new` → `rb_parser_set_context(NULL, TRUE)` → `rb_parser_compile_string_path("-e")` → `rb_iseq_new_main(parent=0)` → `rb_iseq_eval_main`) — backtrace idêntico ao `ruby -e`. Exit: `ruby_cleanup(TAG)` (0 normal / 6 raise; o status do SystemExit sai do errinfo — cleanup(42) viola TAG_FATAL e aborta) → `std::process::exit`.

`**calisto run` flow**: client connects to daemon socket (spawning the daemon on first use) → sends `RUN` with base64 fields (cwd, env, script, args) + its own stdio fds via `SCM_RIGHTS` → daemon `fork()`s a child → child dup2's the fds, chdirs, sets `$0`/`ARGV`, requires `bundler/setup` **só com Gemfile no walk-up (ou `BUNDLE_GEMFILE`)** — sem bundle o `ruby` puro não carrega bundler e o calisto também não (economiza ~25ms por comando; com Gemfile ativa como `bundle exec`), `load`s the script → daemon `waitpid`s and replies `STATUS <code>` → client exits with that code. Child output streams live (real fds, not pipes through the daemon).

**Accept loop multi-conexão (Fase E)**: o daemon roda `select` sobre o listener + conexões ativas e `waitpid WNOHANG` a cada tick — um child de longa duração (server, sidekiq, suíte lenta) NÃO bloqueia novos RUNs. Cliente morto com child rodando → TERM→KILL (por conexão, como o `wait_for` antigo). `STOP` derruba children e devolve `STATUS` aos clientes antes de sair. Child fecha o socket de controle e o listener no fork (hygiene de fds).

**`calisto test` flow** (Fase E): detecta minitest (`test/**/*_test.rb`) ou rspec (`.rspec`/`spec/**/*_spec.rb`), usa um **daemon de teste dedicado** — igual ao da app, mas com `RAILS_ENV=test`/`RACK_ENV=test` no boot e socket próprio (hash inclui sal `"test"`; o Rails fixa o env no boot, um fork do boot dev nunca enxergaria `:test`). Cada arquivo é um `RUN` (fork) no daemon quente, em paralelo (worker por CPU, teto = nº de arquivos; o accept loop multi-conexão é o que permite). `CALISTO_LOAD_PATH=test|spec` no env do RUN injeta `-I` no child (depois do `Bundler.setup`, que limpa o `$LOAD_PATH`) para `require "test_helper"`/`rails_helper` funcionar. `--watch` roda no cliente Rust (polling de mtime a cada 300ms) — fork-safe por construção, sem listen/inotify (lição do Chatwoot).

**`calisto task`** (Fase E): rake no daemon quente via shim `load Gem.bin_path("rake", "rake")` (equivale ao `bin/rake` do Rails, sem exigir binstub), gerado no dir de runtime como `task.rb` (não `rake.rb` — o dir de runtime entrou no `$LOAD_PATH` na Fase P e um `rake.rb` lá sombrearia o `require "rake"` do próprio rake); roda no daemon da app (dev).

**`calisto serve`** (Fase E): launcher `serve.rb` no dir de runtime → `Rack::Builder.parse_file(config.ru)` + `Rackup::Server.start` (rack 3/rackup) com fallback `Rack::Server` (rack 2); o server roda como child do fork; kill no cliente derruba via client-death kill.

**`.env`** (Fase E): parser no CLIENTE Rust (walk-up do cwd, sem sobrescrever vars existentes, suporta `#`, `export`, aspas). O env resultante propaga para o spawn do daemon (o boot da app vê `DATABASE_URL` etc.), para o `env_blob` do RUN (o script vê) e para o `--cold` (paridade cold/warm preservada — parser só no daemon divergiria). `calisto test` força `RAILS_ENV=test`/`RACK_ENV=test` por cima de qualquer `.env`.

**`calisto run -e 'code'`** (Fase G): op **`EVAL`** no daemon (mesmo formato do RUN: cwd, env_blob, code, args) — o child roda com semântica exata de `ruby -e`: no daemon EMBUTIDO via cadeia iseq do CLI (`rb_parser_new` → compile `"-e"` → `rb_iseq_new_main` → `rb_iseq_eval_main` — backtrace só `-e:N`, sem frames de eval): paridade exata com `ruby -e` em $0/ARGV/backtrace (`-e:1`)/exit codes. Múltiplos `-e` são concatenados com `"\n"` no cliente (como o ruby; `__LINE__` segue a concatenação). Cold: `ruby [-rbundler/setup] -e code args` (o flag só com Gemfile — paridade com o child warm) — paridade cold/warm é invariante coberta no `exec.rs`. O child ganhou `child_enter` (bootstrap comum: traps default, dup stdio, hygiene de fds, chdir, ENV.replace, `bundler/setup`, CALISTO_LOAD_PATH + `$calisto_native_dir` — Fase P) compartilhado por RUN/EVAL.

**`calisto exec <bin>`** (Fase G): shim `exec.rb` no dir de runtime, resolvido como `bundle exec` mas **sem depender de binstub**: (1) argumento que é caminho de arquivo executável (`./bin/rails`), (2) spec ativada do bundle (`Gem.loaded_specs`, dedup por nome — default gems aparecem 2×; ambiguidade entre gems DIFERENTES → erro com candidatos), (3) PATH. Binário ruby (shebang ruby) é **`load` in-process** (kernel_load do bundler: `$0` = caminho, ARGV = args) — sem shebang no PATH nem re-exec; binário nativo é `exec` direto (126/127 como bundler). Ex.: `calisto exec sidekiq -r <app>` no Maybe roda o worker como child do fork.

**`calisto run <script>`** (Fase H): `[scripts]` no calisto.toml (`dev = "bin/rails server"`, `test = "rake test"`, `db:migrate = "bin/rails db:migrate"`…). Um nome que **não é arquivo** resolve para `[scripts.NAME]` (arquivo existente sempre vence) e roda como `calisto exec` no daemon — o `exec_argv` do Fase G é compartilhado (shim `exec.rb` no dir de runtime, `load` in-process), com os args do CLI no final do comando e cwd do child na **raiz da app** (dir do calisto.toml, como bun run). `--cold` roda o shim no interpretador direto (paridade cold/warm; `run_cold` ganhou cwd explícito). Comando tokenizado shell-like no cliente (`split_command`: whitespace separa, aspas simples/duplas agrupam — sem escapes/expansão); vazio/aspas quebradas → erro claro. **`preload` é opcional** no parser (seções `[run]` e `[scripts]`): calisto.toml só com scripts NÃO vira daemon da app — `app_daemon()` (`preload.is_some()`) filtra todos os seletores de daemon (run/task/serve/exec/repl/test/status/stop/doctor); scripts não entram no hash do socket (mudar script não reinicia o daemon). Nota: `calisto run test` roda `bin/rails test` no daemon **dev** — o teste de env do railsapp falha por design (Rails.env "development" no boot congelado); é exatamente a razão do daemon de teste RAILS_ENV=test do `calisto test`.

**`calisto repl`** (Fase G): shim `repl.rb` (`require "irb"; IRB.start`), args repassados ao IRB (parse_opts do ARGV); roda como child do fork — no daemon da app (calisto.toml) é console no boot congelado; no genérico, stdlib preloaded. Foreground; kill no cliente derruba via client-death kill (como `serve`).

**`calisto init` / `upgrade` / `completions`** (Fase J): `init` escreve `calisto.toml` (`[scripts] start = "./hello.rb"`), `hello.rb` (shebang `#!/usr/bin/env ruby` + 755 — o shim do exec resolve executáveis e detecta binário ruby pelo shebang para `load` in-process, sem ruby no PATH) e `.gitignore`; nunca sobrescreve arquivo existente sem `--force`. `calisto run` **bare** com `[scripts] start` roda o start (convenção npm/bun — `calisto run` hoje aceita zero args só nesse caso; sem `start`, erro de uso de sempre). `upgrade` (Fase J + Q.2) spawna `sh <vendor>/../scripts/build-ruby.sh` com stdio herdado (`CALISTO_BUILD_SCRIPT` override para testes) — sem versão rebuilda o pin, com versão seta `RUBY_VERSION`; versão sem sha conhecido (3.4.10/3.4.4) falha antes de spawnar; o exit code do script propaga. **Sem scripts/ (instalação portátil via CALISTO_HOME/curl|sh — o tarball não traz o checkout) o upgrade BAIXA o ruby pré-compilado do release** (`curl` + `sha256sum -c` + `tar` — zero deps; `CALISTO_UPGRADE_URL` sobrepõe a base, ex.: `file://` nos testes); `--source` força o build (erro claro sem script). `completions bash|zsh` imprime o script (bash: `complete -F _calisto`, zsh: `#compdef`), flags por subcomando e `*.rb`.

Fase A: `calisto run` ativa Gemfile via Bundler com semântica de `bundle exec` — o child (fork) faz `require "bundler/setup"` **quando há Gemfile no walk-up (ou `BUNDLE_GEMFILE`)** (RUBYOPT não funciona: só é lido no boot do interpretador) e o cold mode passa `-rbundler/setup` na mesma condição. Sem bundle, ninguém carrega o bundler (paridade com o `ruby` puro + ~25ms por comando economizados). Sem instalador próprio: gems instalam com `bundle install` normal. `.ruby-version` (walk up) **seleciona a versão** (Fase I: `vendor/ruby-<v>`; não instalada → erro claro com o comando de build). **Gemfile presente (walk-up do cwd, ou `BUNDLE_GEMFILE`) desativa o preload stdlib** — preload + bundle colidiriam se o Gemfile pinar default gems em versões diferentes (ex.: base64 0.2 do pin vs 0.3 que o Sinatra 4 exige → `Gem::LoadError "already activated"`); sem preload, o `Bundler.setup` ativa o bundle num interpretador "fresco", como o `bundle exec`.

- Fase B (preload de app): `calisto.toml` na raiz da app (walk-up do cwd) com `[run] preload = "entrypoint"` faz o `run` usar um **daemon dedicado da app** (socket em `<runtime>/apps/<fnv1a(app_root+preload+salt+versão)>` — o sal `"test"` do daemon de teste e a versão do ruby (Fase I) entram no hash; pin default mantém o hash clássico, como Spring/Zeus). O daemon da app boota com `-rbundler/setup` + cwd na raiz da app e `load`a o entrypoint no boot (preload stdlib vazio); cada RUN é fork do boot congelado. Fork-safe: conexões ActiveRecord são desconectadas após o boot (o child reconecta lazy) e o entrypoint é registrado em `$LOADED_FEATURES` — `load` não registra, e sem isso o Rails re-roda `config/environment.rb` no child via `require_environment!` (initialize! duplo → "Application has been already initialized"). Daemon stale após editar o entrypoint: `calisto stop` na app (hot reload é Fase E). `status`/`stop`/`doctor` operam no daemon da app quando o cwd está numa app.

**Wire protocol** (RESP-style over unix socket): `"<OP> <n>\r\n"` then n fields `"$<len>\r\n<data>"`. Commands: `PING` → `OK`, `STOP` → `BYE`, `RUN` → `STATUS <code>`, `EVAL` → `STATUS <code>` (idem RUN, mas evala o campo code como `ruby -e`). Fields are base64 (hand-rolled encoder, no crates).

**`calisto build` flow**: `build.rb` parses static `require`/`require_relative`/`autoload` with Ripper (the real lexer), BFS-collects project files under the root, emits a bundle where each file is evaluated via `eval(code, TOPLEVEL_BINDING, original_path, 1)` (preserves `__FILE__`/`__dir__`/`require_relative`) and a loader monkey-patches `Kernel#require`/`require_relative` against an index. Files outside the root (stdlib like `json`) are NOT bundled — delegated to real `require`. `--compile` (Fase F) também embute gems do Gemfile.lock: specs resolvidas via `Gem::Specification.find_by_name` com o GEM_PATH do app (`vendor/bundle`); `.rb` avaliados e **C extensions** embutidas como base64 no `$calisto_native` (loader extrai p/ `/tmp/calisto-native/<abs sanitizado>/` e `require` absoluto → dlopen). Nativos vêm dos require_paths (gems precompiladas, ex.: sqlite3-x86_64-linux) e dos dirs `extensions/` compilados pelo bundle install (puma/nio4r/json); requires **dinâmicos** de nativos (sqlite3: `require "sqlite3/#{RUBY_VERSION}/sqlite3_native"`) são cobertos por **pré-índice** do nome canônico (relativo ao require_path, sem extensão); o BFS resolve `.so`/`.bundle` como candidatos de require. Compilar do zero continua com o `bundle install` (sem toolchain própria). Armadilha do Ruby 3.4: `Hash#each` com bloco de aridade 1 entrega `[k, v]` — o coletor usa `each_key`. O loader pré-carrega os alvos dos `autoload` antes do arquivo registrador (bug do CRuby 3.4: autoload via `eval` dispara na definição do const) em rodadas com retry; `require_relative` de arquivo não embutido delega com caminho absoluto.

## Key Directories

| Path | Purpose |
|---|---|
|`src/`|Rust CLI dividido em módulos por domínio (estrutura inspirada no cli/ do Deno/Bun): `main.rs` (dispatch + help, ~210 linhas), `commands/` (um módulo por comando: run/test/task/serve/exec/repl/build/tooling/deps/doctor), `daemon.rs` (accept loop embutido), `child.rs` (bootstrap do fork), `protocol.rs` (wire protocol RESP-style + SCM_RIGHTS), `runtime.rs` (dirs/spawn/resolução do ruby), `appconfig.rs` (calisto.toml/dotenv), `shims.rs` (shims ruby gerados), `base64.rs`|
|`crates/calisto-build/`|Workspace crate: `src/lib.rs` (spawns bundler) + `src/build.rb` (Ripper bundler, embedded)|
|`crates/calisto-ruby/`|Workspace crate (Fase L): CRuby embedding via dlopen — FFI hand-rolled zero deps (libruby.so.<v>, símbolos por dlsym, RTLD_GLOBAL), `libruby_path()` localiza a libruby do ruby resolvido (modo único desde a Fase S); `NativeFns` (Fase P) — símbolos das APIs nativas (rb_define_method/TypedData/conversões)|
|`crates/calisto-hash/`|Workspace crate (Fase P+T): `Calisto::Hash` — sha256 escalar + **SHA-NI** (`std::arch`, dispatch por `is_x86_feature_detected!`), blake3 hand-rolled (port do reference_impl, vetores oficiais em `test/native.rs`) e **xxh64** (o `Bun.hash` — hash não-cripto, vetores do sanity check oficial do xxHash)|
|`crates/calisto-sqlite/`|Workspace crate (Fase P): binding `Calisto::SQLite` sobre libsqlite3 do sistema (dlopen `libsqlite3.so.0`, FFI hand-rolled) — Database/Statement via TypedData com dfree no GC|
|`crates/calisto-native/`|Workspace crate (Fase T): codecs de string em Rust — `Calisto::Base64` (espelho do stdlib base64 0.3: encode64 com wrap/strict/urlsafe + decodes lenient/strict), `Calisto::URL` (CGI.escape/unescape) e `Calisto::HTML` (ERB::Util.html_escape — o `Bun.escapeHTML`); shims com fallback puro em cold|
|`crates/calisto-{test,task,serve,tooling,cli,runtime}/`|Planned modules, only `.gitkeep` — do not implement until they get a `Cargo.toml`|
|Integration suite (`common/mod.rs` harness, `cli.rs`, `stdio.rs`, `daemon.rs`, `preload.rs`, `build.rs`, `ruby_upstream.rs`, `bundler.rs`, `app.rs`, `realapps.rs`, `testcmd.rs`, `exec.rs`, `scripts.rs`, `versions.rs`, `tooling.rs`, `deps.rs`, `native.rs`, `runflags.rs`), `fixtures/`|Integration suite (`common/mod.rs` harness, `cli.rs`, `stdio.rs`, `daemon.rs`, `preload.rs`, `build.rs`, `ruby_upstream.rs`, `bundler.rs`, `app.rs`, `realapps.rs`, `testcmd.rs`, `exec.rs`, `scripts.rs`, `versions.rs`, `tooling.rs`, `deps.rs`, `native.rs`), `fixtures/` (inclui `gemapp/`, `sinatraapp/` da Fase A, `preloadapp/`, `railsapp/` da Fase B/C e `maybe/`, `chatwoot/` — checkouts gitignored — dos degraus 4/5 da Fase D), `test/vendor/ruby/` (upstream ruby/ruby tests)|
| `scripts/` | `build-ruby.sh` — builds the pinned CRuby |
| `examples/` | `hello.rb` (preload smoke), `bench.rb` (stdlib workload for `--time`) |
| `vendor/` | Pinned CRuby install + sources. **Gitignored** — never commit; reproduce with `scripts/build-ruby.sh` |

## Development Commands

```bash
scripts/build-ruby.sh              # REQUIRED once: builds pinned CRuby 3.4.10 + vendored libyaml (idempotent)
cargo build                        # debug binary: target/debug/calisto
cargo build --release              # lto+strip release
cargo test                         # full suite (~45s; upstream parity runs 17 ruby/ruby files)
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

**⚠️ After editing `crates/calisto-build/src/build.rb`, rebuild** — it is embedded via `include_str!`. A stale release binary silently ships the old bundler.

## Code Conventions & Common Patterns

**Rust** (`src/` módulos + crates):
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
|`src/main.rs`|Ponto de entrada (~210 linhas): `reset_sigpipe` + dispatch dos comandos + `print_help`. A lógica vive nos módulos: `commands/` (run/test/task/serve/exec/repl/build/tooling/deps/doctor), `daemon.rs` (daemon embutido — Fase L), `child.rs`, `protocol.rs`, `runtime.rs`, `appconfig.rs`, `shims.rs`, `base64.rs`|
| `crates/calisto-ruby/src/lib.rs` | (Fase L) CRuby embedding: dlopen `libruby.so.<v>` (RTLD_NOW\|GLOBAL, símbolos por dlsym), `libruby_path(ruby)` decide embutido vs legado, `Ruby::open` + `boot` (ruby_sysinit → init_stack → init → `ruby_options(["-e",""])` = boot completo do CLI — prelude/rubygems incluído; `rb_path2class`/`rb_cObject` só depois do init — BSS), `require`/`load` via Kernel (hook do rubygems / rb_f_load — CWD p/ relativo), `eval_main_iseq` (parser → compile "-e" → iseq MAIN → eval_main — sem frames de eval), chamadas protegidas via `rb_protect` + trampolines (Mutex), `thread_atfork`, `cleanup(TAG)` |
| `crates/calisto-build/src/build.rb` | Bundler: `walk_requires`, BFS collection, `split_end_marker`, gems do Gemfile.lock (pure + nativos `.so` p/ `$calisto_native` + pré-índice), bundle generation with loader |
| `crates/calisto-build/src/lib.rs` | `bundle(ruby, entry, out, root) -> Result<BundleStats, String>`; parses `BUNDLED <n>` |
| `crates/calisto-hash/src/lib.rs` | (Fase P+T) registro de `Calisto::Hash` (rb_define_singleton_method); `sha256.rs` (escalar + dispatch SHA-NI), `blake3.rs` (one-shot fiel ao reference_impl) e `xxh64.rs` (one-shot fiel ao xxhash.c, vetores do sanity check oficial) |
| `crates/calisto-sqlite/src/lib.rs` | (Fase P) dlopen da libsqlite3 + FFI hand-rolled, TypedData (Database/Statement com dfree), `register(vm)` best-effort (sem a lib → daemon degrada, shim levanta LoadError) |
| `scripts/build-ruby.sh` | Pin `RUBY_VERSION` (default 3.4.10; sha conhecido p/ 3.4.4 também), vendored libyaml, `--enable-shared` (Fase L: `lib/libruby.so.<v>` p/ o daemon embutido), `vendor/ruby-<v>/` + symlink `vendor/current` (não troca ao construir versão extra), `CALISTO_REBUILD=1` força rebuild **destrutivo** (rm -rf do prefixo — ver armadilha no fim) |
| `test/common/mod.rs` | Integration harness shared by all test targets |

## Runtime/Tooling Preferences

- **Ruby**: pinned 3.4.10 (sha256 `ecee2d07...9ec`), built by `scripts/build-ruby.sh` into `vendor/current/bin/ruby`. Stdlib-only; **no gems, no Bundler**. Default gem tooling (`test-unit`, `minitest`, `rake`) ships with the vendor build.
- **Rust**: edition 2021, zero deps, `[profile.release] lto+strip`.
- **Env vars** (all `CALISTO_*`): `CALISTO_RUBY` (alternate ruby), `CALISTO_PRELOAD` (stdlib preload list; `0`/`none` disables), `CALISTO_RUNTIME_DIR` (daemon socket/pid location — tests use this for isolation), `CALISTO_HOME` (Q.3: base do vendor na instalação portátil — `$CALISTO_HOME/vendor`; sem a var, walk-up do executável), `CALISTO_BUILD_SCRIPT` (upgrade: path do script de build — testes usam um fake; default `<vendor>/../scripts/build-ruby.sh`), `CALISTO_UPGRADE_URL` (Q.2: base dos downloads de rubies pré-compilados — ex.: `file://` nos testes; default release do GitHub), `CALISTO_BUNDLE` (deps: binario do bundle — testes usam um fake; default `ruby -S bundle` do ruby resolvido), `CALISTO_BUNDLE_RUBY` (deps: exportado pelo client pro child — o ruby resolvido, p/ observação), `CALISTO_EMBED_RUBY` (Fase L: setado pelo client no spawn do daemon embutido — o ruby resolvido, p/ o daemon achar a .so), `CALISTO_COMPACT` (Fase M: override do `[run] compact` — `0/1`; default on no daemon de app), `CALISTO_YJIT`/`CALISTO_WARMUP` (Fase N: overrides do `[run] yjit`/`[run] warmup` do daemon de app), `CALISTO_SOCKET`/`CALISTO_PIDFILE` (set by the client when spawning the daemon), `CALISTO_LOAD_PATH` (test: injeta `-I` no child pós-Bundler.setup), `CALISTO_APP_PRELOAD`/`CALISTO_APP_WARMUP` (setados pelo client no spawn do daemon da app — entrypoint e warmup), `CALISTO_SERVE_HOST`/`CALISTO_SERVE_PORT` (serve), `CALISTO_PROBE_PID`/`CALISTO_PROBE_DONE` (doctor: protocolo do child de probe), `CALISTO_DAEMON_NO_DETACH` (debug: mantém stderr do spawner no boot do daemon). O spawn do daemon também seta `MALLOC_ARENA_MAX=2` (Fase M.3 — arenas do glibc).
- Default daemon preload: `json,yaml,erb,pathname,fileutils,time,date,digest,base64,uri,net/http,ostruct,set,csv,stringio,logger,socket`.
- No Makefile, no CI, no README — do not create docs unless asked. `git` commits with `-c user.name="felipeb" -c user.email="felipeb@local"` (repo has no local git identity).

## Testing & QA

Framework: **cargo integration tests** (16 targets in Integration suite (`common/mod.rs` harness, `cli.rs`, `stdio.rs`, `daemon.rs`, `preload.rs`, `build.rs`, `ruby_upstream.rs`, `bundler.rs`, `app.rs`, `realapps.rs`, `testcmd.rs`, `exec.rs`, `scripts.rs`, `versions.rs`, `tooling.rs`, `deps.rs`, `native.rs`, `runflags.rs`), `fixtures/`, declared as `[[test]]` in `Cargo.toml`). No unit tests in `main.rs` (0 tests there is expected).

Harness (`test/common/mod.rs`): each test spawns the real binary via `env!("CARGO_BIN_EXE_calisto")` with a **unique `CALISTO_RUNTIME_DIR`** (isolated daemon per test → parallel-safe). `run_opt` pipes stdio, writes stdin, and enforces a **timeout** (kills + panics if the daemon ever holds a pipe — a known regression class). `spawn_stdout` for live-process tests (read child pid, send signals).

Coverage contract:
- `cli.rs` — commands, flags, error paths, `status`/`stop` lifecycle; **Fase M**: `doctor` reporta RSS/Pss/Shared_Clean/Private_Dirty do daemon e do child de probe (smaps_rollup) com o daemon vivo — e silencioso sem daemon.
- `stdio.rs` — argv/env/cwd/stdin/exit codes/backtraces; **`cold_and_warm_agree`** (parity between `--cold` and warm daemon is a hard invariant); `__FILE__`/`__dir__`/`DATA`.
- `daemon.rs` — socket reuse, stale-socket recovery, orphan kill on client death (checks `/proc/<pid>`), signal exit codes (`SIGKILL` → 137), concurrent runs, pipeline non-hang; **Fase S**: daemon embutido (pidfile → `/proc/<pid>/exe` == binário calisto) para todas as versões com libruby.so — e ruby sem `.so` (CALISTO_RUBY fake) → erro claro com o comando de rebuild.
- `preload.rs` — default/`0`/custom preload behavior.
- `build.rs` — bundle parity with original sources (renames the source tree away to prove self-contained), `DATA` emulation, `__FILE__`/`__dir__` preservation, stdlib delegation, bundle under `calisto run`. **Fase F**: `compile_bundle_embeds_gems_and_runs_without_bundle` (Sinatra + 10 gems, GEM_PATH vazio); `compile_embeds_c_extensions_and_runs` (puma — .so compilado pelo bundle install — roda standalone); `compile_embeds_sqlite3_c_extension` (gem precompilada com require **dinâmico** — pré-índice do nome canônico; gated em bundle install do railsapp).
- `ruby_upstream.rs` — **parity contract**: each of the 17 upstream ruby/ruby (tag v3_4_10) runtime tests must produce the same test-unit summary (tests/failures/errors) and exit code as plain `ruby -I tool/lib -I test/lib`. Uses `--seed=1` + filter `-n '!/memory_leak/'` (upstream tests have implicit `require` deps and RSS-based tests that are flaky even on pure ruby), and retries against environment flakiness. Always run upstream tests with `--preload 0`.
- `bundler.rs` — **Fase A (Gemfile activation)**: fixture `gemapp` (5 default/bundled gems, `Gemfile.lock` commitado → hermético, sem rede nem `bundle install`) prova ativação via `$LOAD_PATH`; `cold_and_warm_agree` com bundle; Gemfile com gem faltando falha como `bundle exec` (GemNotFound, script não roda); `BUNDLE_GEMFILE` env é honrado; `.ruby-version` com versão não instalada → erro claro (exit != 0) com o comando de build vs 3.4.10 (instalada) → silêncio (Fase I); golden Sinatra HTTP **gated** em `bundle install` prévio no fixture (`test/fixtures/sinatraapp`) — skipa com aviso se as gems não estiverem instaladas.
- `app.rs` — **Fase B (preload de app)**: fixture `preloadapp` (boot simulado 2s + contador, hermético) prova que o boot roda UMA vez no daemon e o 2º `run` <500ms; daemon da app isolado do genérico; `calisto.toml` inválido (entrypoint inexistente/sintaxe) → erro claro apontando o problema; golden Rails **gated** (`test/fixtures/railsapp`, Rails 8 + sqlite3): `bin/rails runner` 2º run <500ms e query `SELECT 1` reconecta no child (fork-safe). **Fase C (Rails mínimo)**: `bin/rails server` responde GET `/up` → 200 em <500ms do spawn (servidor roda como child do fork; o cliente killado derruba o Puma via client-death kill) e `bin/rails console` roda IRB no contexto da app via stdin pipe. **Fase M**: `[run] compact` — daemon da app compacta o heap no boot por default (`GC.stat(:compact_count)` do child ≥1), `CALISTO_COMPACT=0`/`compact = "false"` desligam (0) e valor inválido → erro apontando a linha.
- `realapps.rs` — **Fase D, degraus 4 e 5** (apps reais, gated em checkout gitignored + bundle install + docker compose que o teste sobe sozinho): **degrau 4 (Maybe Finance, Rails 7.2 + Sidekiq 8)**: db:prepare, 2º comando <500ms, `CalistoProbeJob` enfileirado no Redis processado pelo worker `calisto exec sidekiq -r <app>` (Fase G — o binário da gem é resolvido no bundle ativo e carregado in-process no daemon quente; o `-r` é o require path que o CLI 8 exige), smoke HTTP `GET /sessions/new` → 200. Ajustes do fixture: pin nativo **3.4.4** (a Fase D tinha pinado 3.4.10 como workaround do pin único; a Fase I reverteu — `.ruby-version`/Gemfile 3.4.4, lock regenerado sob 3.4.4), `active_job.queue_adapter = :sidekiq` (dev default :async), `bin/sidekiq` próprio (CLI 8 exige `-r <dir>`). **degrau 5 (Chatwoot, Rails 7.1 + ~155 gems + pgvector)**: db:prepare + seed condicional, 2º comando <500ms, dev server com login (devise_token_auth POST /auth/sign_in → access-token+client), `GET /api/v1/accounts/1/conversations` autenticado → 200 e ActionCable WebSocket handshake `/cable` → 101 (lê 1 chunk — o WS não fecha). Ajustes do fixture: pin 3.4.10, `scout_apm` removido (C ext não compila no 3.4.10, `require: false`), **`config.file_watcher = ActiveSupport::FileUpdateChecker`** (sem listen/inotify — o fork do daemon herda watchers e o 2º fork morre rc=1 silencioso; watch é Fase E), migration `Taggable::Cache`→`Caching` (API mudou na acts-as-taggable-on 12), imagem postgres `pgvector/pgvector:pg16` (migration do Captain usa `CREATE EXTENSION vector`), limpa `tmp/pids/server.pid` antes do server. **Fase M (marco)**: `chatwoot_compaction_cuts_probe_child_pss` — baseline `CALISTO_COMPACT=0` vs default on, medido via `doctor` (o Chatwoot pina 3.4.4 → desde a Fase S o daemon roda **embutido** e o teste cobre o hook de compactação Rust): Private_Dirty do child **−58%** (≥30% no assert), Pss menor.
- `testcmd.rs` — **Fase E (test/task/serve/.env)**: `calisto test` hermético no preloadapp (boot 2s pago UMA vez no daemon de teste; suite de 2 arquivos — um com sleep 0.6s — <1s quente, provando paralelismo de arquivos via accept loop multi-conexão; contador de boot == 1); teste falhando → exit 1; sem testes → erro claro; detecção rspec via `.rspec`/`spec/`; **golden railsapp** (minitest, gated em bundle install): `calisto test` 2 arquivos <1s quente e o teste `rails_env_test.rb` prova `Rails.env == "test"` (daemon de teste com RAILS_ENV=test no boot — o daemon dev deixaria "development"); `.env` warm == cold (paridade), aspas/`export`/não-sobrescreve; `calisto task db:migrate` idempotente e quente (<1s; limpa db/schema.rb do fixture no fim); `calisto serve` GET /up → 200 com o daemon CONTINUANDO a servir `runner` enquanto o server roda (multi-conexão).

- `exec.rs` — **Fase G (run -e/exec/repl)**: `-e` com paridade cold/warm ($0 = "-e", ARGV, backtrace `-e:1`, múltiplos `-e` concatenados com `__LINE__` 1 e 2 — paridade com o ruby); **marco <50ms** no 2º `run -e` quente (36ms medido); `-e` no daemon da app (preloadapp) roda no boot congelado sem re-boot; `exec` resolve rake no gemapp (hermético, sem binstub), cai no PATH (sh), carrega script ruby com shebang in-process ($0/ARGV), inexistente → 127 e warm <500ms no daemon da app; `repl` roda IRB como child do fork no contexto da app (boot UMA vez, `puts 1+1` → 2 via stdin pipe).
- `scripts.rs` — **Fase H (scripts no calisto.toml)**: fixture hermética `scriptsapp` (só `[scripts]`, sem preload) prova resolução + args do CLI ao final do comando + aspas agrupando + cwd na raiz da app (subdir); paridade cold/warm no script; arquivo existente vence o script de mesmo nome; script inexistente/vazio/aspas quebradas → erro claro; calisto.toml só de scripts NÃO cria daemon da app (`apps/` ausente, daemon genérico serve arquivo); `[scripts]` na `preloadapp` → script roda no daemon da app com boot UMA vez (<500ms no 2º); **golden railsapp** (gated em bundle install): `run dev -p PORT` sobe o server (GET /up → 200), `run db:migrate` idempotente (135ms quente) e `run test <filtro>` roda a suite (74ms) — `run test` completo roda `bin/rails test` no daemon dev e o teste de env falha POR DESIGN (exit 1), documentando a razão do daemon de teste.
- `versions.rs` — **Fase I (multi-versões)**: gated em `vendor/ruby-3.4.4` (RUBY_VERSION=3.4.4 scripts/build-ruby.sh): `.ruby-version` seleciona (prefixo `ruby-` tolerado) com cold/warm concordando; diretiva `ruby "3.4.4"` do Gemfile seleciona (lock vazio hermético); versão não instalada → erro claro com o build; daemons genéricos isolados por versão (`runtime_dir/ruby-3.4.4/` — stop de um não derruba o outro); daemon da app por versão (hash do socket inclui a versão — o mesmo app em 3.4.4 e default paga o boot UMA vez cada, `apps/` com 2 dirs). O gate `bundle_check` do harness resolve a versão da app (`.ruby-version`/Gemfile → bundler do vendor certo).
- `tooling.rs` — **Fase J (init/upgrade/completions)**: `calisto init` (cwd explícito no teste — nunca herdar o cwd do processo!) gera calisto.toml + hello.rb executável + .gitignore e o app roda com `calisto run` BARE (= `[scripts] start`, convenção npm/bun — `run` sem script sem `start` continua erro de uso), com `run hello.rb` (arquivo vence) e `--cold` (paridade); nunca sobrescreve sem `--force`; nome-é-arquivo/flag desconhecida → erro. `upgrade` roda o build script (fake via `CALISTO_BUILD_SCRIPT`; sem versão = pin default, com versão passa `RUBY_VERSION`), propaga o exit code do script, versão sem sha conhecido (3.4.10/3.4.4) falha ANTES de spawnar e script ausente/args demais → erro claro. `completions bash|zsh` imprimem scripts instaláveis (`complete -F _calisto calisto`/`#compdef calisto`); shell ausente/desconhecido → erro de uso.
- `deps.rs` — **Fase K (add/remove/lock)**: wrapper fino do bundle com fake via `CALISTO_BUNDLE` (hermético): args passam direto, cwd na raiz do projeto (dir do Gemfile — walk-up), `BUNDLE_GEMFILE` setado, PATH prefixado com o bin dir do ruby (trap do restart do bundler), `CALISTO_BUNDLE_RUBY` exportado e exit code propaga (FAKE_BUNDLE_RC); sem Gemfile → erro claro sugerindo `bundle init`; `.ruby-version` 3.4.4 → bundle do `vendor/ruby-3.4.4` (gated, como versions.rs).
- `native.rs` — **Fase P+T (APIs nativas calisto.\*)**: blake3 contra os **vetores oficiais** (13 comprimentos: 0..102400 — chunk único/cheio/árvore; input = padrão cíclico de 251 bytes como o test_vectors.json); sha256 com paridade cold/warm E com `Digest::SHA256` em 7 tamanhos (cobre SHA-NI vs stdlib); sqlite hermético em `:memory:` (binds de todos os tipos, prepared statement reutilizado com re-bind, changes/rowid/columns, close/closed?, `Calisto::SQLite::Error` com errmsg, multi-statement → erro, TypeError em bind inválido); cold sqlite → LoadError claro; benchmark sha256 de 100MB: ratio ≥3× o Digest no **--release** (debug não mede — Rust sem otimização; skip sem `sha_ni` na CPU). **Fase T**: base64 com paridade cold/warm E stdlib (6 métodos × 10 tamanhos cobrindo o wrap de 60 chars, urlsafe com/sem padding) + semântica de decode (lenient com lixo/`=`/grupo parcial; strict com ArgumentError "invalid base64" nos casos inválidos — mesmas regras do pack "m"/"m0", warm E cold); URL escape/unescape == CGI (unicode, `%zz` inválido, hex minúsculo, roundtrip); HTML == ERB::Util.html_escape; xxh64 contra os **vetores do sanity check oficial** do xxHash (buffer determinístico, seeds 0/PRIME32, caminhos tail e blocos; cold → NotImplementedError) + benchmark de 100MB: ratio ≥3× o Digest::SHA256 (medido **37×** no release).
- `runflags.rs` — **Fase R (flags ruby do run)**: `-I`/`-r` (isolados, combinados, anexados `-Ilib`, LoadError de lib ausente), `-w`/`-W0`/`-W2` (warnings), `-c` (ok/bad/`-e`/`__END__`/não executa), `-E` (ext, ext:int, inválido), `--` termina flags, `-v`/`--version` (topo e `run -v`), flags+script do calisto.toml → erro claro, flags no daemon da app — 16 testes com **paridade cold/warm** em cada flag.
- `tooling.rs` — **Fase J+Q (init/upgrade/completions/distribuição)**: (Fase J) init/upgrade/completions como antes, 6 testes + (Fase Q) 4: `CALISTO_HOME` vence o walk-up do checkout (--version mostra o ruby fake do home); upgrade sem scripts/ BAIXA o tarball (`CALISTO_UPGRADE_URL=file://` hermético) e extrai em `<vendor>/`; sha256 ruim → erro sem extração; `--source` força o build.

QA rule of thumb: for any change to `run` semantics, the acceptance test is `cold_and_warm_agree` plus the upstream parity harness — if pure `ruby` and calisto diverge, it's a bug in calisto.

## Armadilhas conhecidas do ambiente

- **Bundler restart por shebang**: o ruby 3.4.10 embute bundler 2.6.9; app cujo `Gemfile.lock` pina outra versão (ex.: Chatwoot locka 2.5.16) dispara `Bundler::SelfManager#restart_with_locked_bundler_if_needed` no `require 'bundler/setup'` frio — o bundler re-executa o processo via shebang `#!/usr/bin/env ruby` e **precisa de `ruby` no PATH** (senão: `env: 'ruby': No such file`). O daemon warm não é afetado (o restart acontece no boot do daemon, que não tem shebang; o child herda o bundler já ativo). `calisto run --cold bin/rails ...` num app com bundler divergente exige `PATH` com o ruby pinado; o `calisto add/remove/lock` (Fase K) já prefixa o PATH com o bin dir do ruby resolvido.
- **Rebuild in-place do ruby (Fase L)**: `CALISTO_REBUILD=1` agora é **destrutivo** (rm -rf do prefixo) — sem isso, o `make` roda o passo de instalação das default gems com C ext usando o `bin/ruby` **stale** ainda no prefixo e grava as exts no dir de api da build antiga (`extensions/.../<ver>-static/`), criando specs "regulares" órfãos das default gems (cgi/date/erb/stringio). O rubygems passa a "Ignoring <gem> because its extensions are not built" no boot do daemon — sintoma: teste de paridade de backtrace do `-e` falha (warm com a linha extra no stderr). Se aparecer, limpar `specifications/<gem>.gemspec` + `gems/<gem>/` órfãos resolve (default gems voltam a ser default).
- **Shell persistente**: `export RAILS_ENV=test`/`POSTGRES_*` no shell do dev vaza para o `cargo test` (o teste `rails_console_runs_in_app_context` espera `development` e quebra). Rodar a suíte com o ambiente limpo.
- **`rb_str_bytesize` não é exportado** (Fase P): o CRuby 3.4 não exporta o bytesize em bytes (só `rb_str_strlen`, que devolve CARACTERES para multibyte) — `Ruby::string_bytes` usa `String#bytesize` via `rb_funcallv`. Qualquer binding nativo novo deve conferir o símbolo com `nm -D` antes de assumir.
- **gcc 15+ (default c23) vs headers do ruby 3.4.4 sob `-std=c99`** (Fase S): o `ruby/internal/stdbool.h` do 3.4.4 tem o bug corrigido no 3.4.10 — com `HAVE_STDBOOL_H` ausente e `HAVE__BOOL` setado, nenhum branch define `bool`; extensões que forçam `-std=c99` nos próprios extconfs (bootsnap, commonmarker, nokogiri, unf_ext, skylight...) quebram com "unknown type name 'bool'" (gcc 15+/16 também mudou o `<stdbool.h>` do c23, que não define mais `bool`). O `scripts/build-ruby.sh` porta o header fixado do 3.4.10 para qualquer prefixo após o install (idempotente). Rebuildar o 3.4.4 manualmente fora do script reintroduz o bug.
- **Debug gem não sobrevive ao fork** (Fase S): `require "debug"` (Bundler.require de apps Rails em dev) inicia `DEBUGGER__::SESSION` com TracePoint `:script_compiled` + threads de UI no daemon; o fork do child mata as threads e o 1º script compilado no child trava esperando comando. O daemon desativa a sessão pós-boot (daemon.rs, antes do compact/bind) — `binding.break` continua funcionando no child (a sessão nasce fresca lá). Sintoma antigo do bug: `run bin/rails db:prepare` no Maybe pendurava >60s com "DEBUGGER Exception: No live threads left".
- **Fixtures dos goldens (maybe/chatwoot)**: checkout gitignored; o `bundle install` exige libpq (vendored em `vendor/src/postgresql-16.6/install` — o libpq do sistema não existe neste dev) e libyaml (vendored do build-ruby.sh); `PATH` com o `pg_config` vendored + `PKG_CONFIG_PATH` no yaml-0.1.pc. Exts antigas em `extensions/.../3.4.0-static/` ficam invisíveis após rebuild com `--enable-shared` (rbconfig muda o sufixo do dir) — re-gerar com `bundle install` (as gems C-ext precisam ser removidas antes; o bundler não reconstrói exts de gems já instaladas).
