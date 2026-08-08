# calisto

A Bun-like runtime for Ruby: a single Rust binary that embeds a pinned CRuby
(3.4.10, multi-version capable) and gives it near-instant startup via a warm
fork-based daemon. Linux only (`fork`).

## Features

- **Fast startup** — a daemon boots the interpreter once; every `calisto run`
  is a `fork` of the warm VM. `calisto run -e 'puts 1+1'` in ~36ms (cold
  baseline with `--cold` for comparison).
- **Bundler-native** — a Gemfile in the project (walk-up) is activated like
  `bundle exec`; `.ruby-version` / `Gemfile ruby "x.y.z"` select the Ruby
  version. Install gems with a plain `bundle install`.
- **App preload** — `[run] preload` in `calisto.toml` boots the app once and
  forks it (Rails/Sinatra/Chatwoot-grade apps: 2nd command <500ms).
- **Memory (CoW)** — heap compaction post-boot (`[run] compact`, default on)
  plus optional `[run] yjit`/`[run] warmup`; `calisto doctor` reports
  RSS/Pss/Shared_Clean/Private_Dirty per process.
- **Product tooling** — `test` (minitest/rspec, parallel files, `--watch`),
  `task` (rake), `serve` (Rack), `exec` (gem binaries, `bunx`-style), `repl`,
  `.env`, `[scripts]` in `calisto.toml` (`calisto run dev`).
- **Single-file builds** — `calisto build --compile` bundles your app and its
  gems, including precompiled C extensions, into one self-contained file.
- **Native APIs** — `Calisto::Hash` (sha256/blake3/xxh64 — xxh64 is the
  non-crypto `Bun.hash`, ~37× `Digest::SHA256` on 100MB), `Calisto::SQLite`,
  `Calisto::Base64`/`URL`/`HTML`.
- **Ruby parity** — `calisto run` matches `ruby <script>` semantics (exit
  codes, signals, backtraces, `__FILE__`/`DATA`); validated against 17
  upstream ruby/ruby tests and a cold/warm parity invariant.

## Install

```sh
curl -fsSL https://github.com/felipebrgs1/ruby-rust/releases/latest/download/install.sh | sh
```

Installs to `~/.calisto` with a shim in `~/.local/bin` (add it to `PATH`).

## Quick start

```sh
calisto run examples/hello.rb        # warm daemon
calisto run -e 'puts 1+1'            # ruby -e semantics
calisto init my-app && cd my-app
calisto run                          # runs [scripts] start (npm/bun convention)
calisto test                         # minitest/rspec on the warm daemon
calisto task db:migrate              # rake
calisto serve -p 4567                # Rack app from config.ru
calisto exec sidekiq                 # gem binary, bundle context
calisto build app.rb --compile -o out.rb
```

## Commands

| Command | What it does |
|---|---|
| `run` | Run a script / `-e` code / `[scripts]` entry on the warm daemon (`--cold` for baseline, `--time`; child flags `-I/-r/-w/-W/-c/-E` mirror `ruby`) |
| `test` | Run minitest or rspec suites, files in parallel, on a dedicated test daemon (`RAILS_ENV=test`) |
| `task` | Run rake on the warm daemon |
| `serve` | Serve `config.ru` via rack/rackup as a daemon child |
| `exec` | Run a gem's binary in the bundle context, no binstub required |
| `repl` | IRB in the preloaded app context |
| `build` | Bundle requires into a single self-contained file (`--compile` embeds gems + C extensions) |
| `init` / `upgrade` / `completions` | Scaffold (`calisto.toml` + `hello.rb`), rebuild/download Rubies, shell completions |
| `add` / `remove` / `lock` | Thin `bundle` wrapper using the resolved Ruby |
| `status` / `stop` / `doctor` | Daemon lifecycle and memory diagnostics |

## App config (`calisto.toml`)

```toml
[run]
preload = "config/environment.rb"   # boot the app once, fork it after
compact = true                      # GC.compact post-boot (default: on)
yjit = true                         # boot the daemon with --yjit
warmup = "script.rb"                # warm the hot path before forking

[scripts]
dev = "bin/rails server"
db:migrate = "bin/rails db:migrate"
```

## Ruby versions

Resolution order: `CALISTO_RUBY` → `.ruby-version` (walk-up) → Gemfile
`ruby "x.y.z"` → pinned default (`vendor/current`). Build a version with
`RUBY_VERSION=<v> scripts/build-ruby.sh`; daemons are isolated per version.
`calisto upgrade` downloads prebuilt Rubies when no source tree is present.

## Build from source

```sh
scripts/build-ruby.sh   # required once: builds pinned CRuby + vendored libyaml
cargo build             # target/debug/calisto
cargo build --release   # lto+strip
cargo test              # integration suite (~45s)
```
