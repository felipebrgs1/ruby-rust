//! calisto — CLI (binario unico, zero deps). Ponto de entrada: dispatch dos
//! comandos. A organizacao segue o padrao dos runtimes modernos (Deno/Bun):
//! cada comando/dominio vive num modulo proprio.
//!
//! - `commands/` — um modulo por comando (run/test/task/serve/exec/repl/
//!   build/tooling/deps/doctor)
//! - `daemon.rs` + `child.rs` — daemon embutido (accept loop em Rust) e o
//!   child do fork (bootstrap da VM)
//! - `protocol.rs` — wire protocol (RESP-style + SCM_RIGHTS) e requests
//! - `runtime.rs` — dirs de runtime, resolucao do ruby, spawn dos daemons
//! - `appconfig.rs` — calisto.toml / dotenv
//! - `shims.rs` — shims ruby gerados no dir de runtime
//! - `base64.rs` — base64 hand-rolled

use crate::appconfig::DEFAULT_PRELOAD;
use crate::runtime::PINNED_RUBY;
use crate::commands::*;
use crate::daemon::cmd_daemon;
use std::env;

unsafe extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}



mod appconfig;
mod base64;
mod child;
mod commands;
mod daemon;
mod protocol;
mod runtime;
mod shims;



const SIGPIPE: i32 = 13;
const SIG_DFL: usize = 0;


/// Comportamento Unix padrao: morrer silenciosamente com SIGPIPE quando o
/// consumidor fecha o pipe (ex.: `calisto doctor | head`), em vez de panicar
/// com EPIPE (Rust ignora SIGPIPE por padrao).
fn reset_sigpipe() {
    unsafe {
        signal(SIGPIPE, SIG_DFL);
    }
}


fn main() {
    reset_sigpipe();
    let argv: Vec<String> = env::args().skip(1).collect();
    let code = match argv.first().map(String::as_str) {
        Some("run") => cmd_run(&argv[1..]),
        Some("test") => cmd_test(&argv[1..]),
        Some("task") => cmd_task(&argv[1..]),
        Some("serve") => cmd_serve(&argv[1..]),
        Some("exec") => cmd_exec(&argv[1..]),
        Some("repl") => cmd_repl(&argv[1..]),
        Some("build") => cmd_build(&argv[1..]),
        Some("init") => cmd_init(&argv[1..]),
        Some("upgrade") => cmd_upgrade(&argv[1..]),
        Some("completions") => cmd_completions(&argv[1..]),
        Some("add") => cmd_bundle_wrapper("add", &argv[1..]),
        Some("remove") => cmd_bundle_wrapper("remove", &argv[1..]),
        Some("lock") => cmd_bundle_wrapper("lock", &argv[1..]),
        // interno (Fase L): spawnado pelo proprio cliente quando o ruby
        // resolvido tem libruby.so — roda o daemon com a VM embutida
        Some("daemon") => cmd_daemon(&argv[1..]),
        Some("status") => cmd_status(),
        Some("stop") => cmd_stop(),
        Some("doctor") => cmd_doctor(),
        Some("help" | "-h" | "--help") => {
            print_help();
            0
        }
        Some(other) => {
            eprintln!("calisto: unknown command '{other}'");
            print_help();
            1
        }
        None => {
            print_help();
            0
        }
    };
    std::process::exit(code);
}


fn print_help() {
    println!(
        "calisto - a Bun-like runtime for Ruby (pinned CRuby + fork-based fast startup)

USAGE:
  calisto run [--cold] [--time] [--preload LIST] [-e 'code' | <script.rb> | <script>] [args...]
  calisto test [--watch] [file|dir...]
  calisto task <args...>
  calisto serve [-p PORT] [-o HOST]
  calisto exec <bin> [args...]
  calisto repl [args...]
  calisto build <app.rb> [-o out.rb] [--root DIR]
  calisto init [name] [--force]
  calisto upgrade [version]
  calisto completions <bash|zsh>
  calisto add <gem...> | remove <gem...> | lock
  calisto status | stop | doctor | help

  run     executes <script.rb> on the pinned CRuby. Default: warm daemon that
          forks a child per run (fast startup). --cold spawns the interpreter
          directly for baseline comparison.
          -e 'code' (ou --eval) roda codigo inline com semantica de `ruby -e`
          ($0 = \"-e\", backtrace \"-e:1\", ARGV = args restantes; multiplos -e
          sao concatenados com newline, como o ruby). Tambem aceita --cold.
          --preload LIST overrides the stdlib the daemon preloads (\"0\" disables;
          default: {DEFAULT_PRELOAD}).
          A Gemfile do diretorio atual (buscando para cima) e ativada como em
          `bundle exec ruby`; instale as gems com `bundle install` normal.
          Com um calisto.toml no diretorio atual (walk up) o daemon vira o
          daemon da app (socket dedicado) e pre-carrega o entrypoint de
          [run].preload no boot — boot congelado, cada comando roda como fork.
          [run] compact (Fase M) compacta o heap pos-boot (CoW; default on);
          [run] yjit = true (Fase N) boota com --yjit e [run] warmup = \"path\"
          roda um script de aquecimento no daemon (ex.: requests em memoria)
          — o hot path compilado e herdado por cada fork.
          Um nome que nao e arquivo resolve para [scripts.NAME] do calisto.toml
          (Fase H): `calisto run dev` roda o comando de `dev = \"bin/rails
          server\"` no daemon (como `calisto exec`, com o Gemfile ativo), com
          os args do CLI no final. Arquivo existente sempre vence; calisto.toml
          so com [scripts] (sem [run].preload) usa o daemon generico.
          `calisto run` sem script roda o comando `start` do calisto.toml
          quando existe (convencao npm/bun — o `calisto init` gera).
          O ruby usado (Fase I, multi-versoes) vem de: CALISTO_RUBY (override),
          .ruby-version (walk up, como rbenv) ou a diretiva `ruby \"x.y.z\"` do
          Gemfile; a versao pedida precisa estar em vendor/ruby-<v>/ (rode
          RUBY_VERSION=<v> scripts/build-ruby.sh) — senao e erro claro. Sem
          pedido, o pin default vendor/current ({PINNED_RUBY}). Daemons sao
          isolados por versao (socket proprio).
  test    roda a suite de testes no daemon quente: detecta minitest
          (test/**/*_test.rb) ou rspec (spec/**/*_spec.rb, via .rspec) e roda
          cada arquivo como um fork — o boot da app (calisto.toml) e pago UMA
          vez num daemon de teste dedicado (RAILS_ENV=test, socket proprio);
          arquivos rodam em paralelo. --watch re-roda ao salvar. Exit != 0 se
          algum arquivo falhar. Args sao filtros (arquivos ou diretorios).
  task    roda rake no daemon quente: `calisto task db:migrate` == `calisto run
          bin/rake db:migrate` (equivalente ao bin/rake do Rails, sem exigir
          binstub). Usa o daemon da app (dev) quando ha calisto.toml.
  serve   sobe a Rack app do config.ru como child do fork do daemon quente
          (rackup/rack do bundle; ex.: `calisto serve -p 4567`). Fica em
          foreground; kill no cliente derruba o server.
  exec    roda o binario de uma gem no daemon quente, no contexto da app
          (ex.: `calisto exec sidekiq`, `calisto exec rubocop`). Resolucao
          como `bundle exec`, sem depender de binstub: caminho de arquivo
          (./bin/rails), executavel de uma spec do bundle ativo, ou PATH.
          Binario ruby e carregado in-process (kernel_load do bundler);
          nativo e exec direto. Ambiguo (2+ gems com o mesmo executavel) e
          erro com os candidatos; nao encontrado -> 127.
  repl    IRB no daemon quente, no contexto da app pre-carregada (calisto.toml)
          ou da stdlib preloaded (sem app). Args sao repassados ao IRB. Fica
          em foreground; kill no cliente derruba o child.
  build   empacota <app.rb> e seus requires (arquivos do projeto, stdlib-only)
          num arquivo unico self-contained. Arquivos fora da raiz (stdlib)
          nao sao embutidos. --root define a raiz do projeto (default: o
          diretorio do entrypoint).
          --compile embute as gems do Gemfile.lock (Fase F): pure-Ruby
          avaliados e C extensions (.so/.bundle ja compilados) embutidos
          como bytes e extraidos p/ tmpdir no runtime — o bundle roda sem
          bundle install (GEM_HOME/GEM_PATH vazios); requires dinamicos de
          nativos cobertos por pre-indice. Compilar do zero continua
          delegado ao bundle install.
          Autoloads das gems sao cobertos (pre-carga em rodadas).
  init    scaffold de app (como `bun init`): calisto.toml com
          [scripts] start = \"./hello.rb\", hello.rb e .gitignore. Sem nome usa
          o diretorio atual; `calisto init <name>` cria <name>/. Nunca
          sobrescreve arquivo existente sem --force. O app gerado roda com
          `calisto run` (bare = o script start).
  upgrade rebuild do pin do CRuby (scripts/build-ruby.sh; idempotente — rubies
          ja construidos sao pulados). `calisto upgrade <version>` constroi
          vendor/ruby-<v> para apps com .ruby-version/Gemfile (versoes com sha
          conhecido: 3.4.10/3.4.4; outras: RUBY_SHA256=<sha> manual).
  completions imprime o script de completions do shell em stdout (bash/zsh) —
          ex.: `calisto completions bash > /etc/bash_completion.d/calisto`.
  add/remove/lock
          wrappers finos do `bundle add/remove/lock` (decisao da Fase A: nada
          de instalador proprio) com o ruby da versao certa (Fase I) e cwd na
          raiz do projeto (walk-up do Gemfile, como o resto do calisto). Args
          passam direto ao bundle (ex.: `calisto add sinatra --group web`);
          sem Gemfile, erro claro sugerindo `bundle init`.
  status  shows whether the warm daemon is running
  stop    stops the warm daemon
  doctor  prints environment, pinned ruby version and daemon state

CONFIG:
  CALISTO_RUBY        path to a ruby binary (default: vendor/current/bin/ruby)
  CALISTO_PRELOAD     comma-separated stdlib preload list
  CALISTO_RUNTIME_DIR daemon socket/pid location (default: $XDG_RUNTIME_DIR/calisto)
  CALISTO_COMPACT     [run].compact override (0/1): compacta o heap do daemon
                      pos-boot (GC.start + GC.compact — Fase M) para os forks
                      compartilharem paginas via CoW; default on no daemon de
                      app (calisto.toml com [run].preload)
  CALISTO_YJIT        [run].yjit override (0/1): daemon da app boota com
                      --yjit (Fase N) — o warmup compila o hot path no daemon
                      e os children herdam o codigo JIT pronto
  CALISTO_WARMUP      [run].warmup override (path; 0/none desliga): script
                      rodado no daemon pos-boot (ex.: requests contra a Rack
                      app em memoria) antes da compactacao/bind

NOTE: calisto run is equivalent to `bundle exec ruby <script>` with -e/-E VM
flags (alem de -e) ainda nao suportados; fora de Gemfile, identico a
`ruby <script>`.
Linux only (fork)."
    );
}
