//! calisto — shims
//!
//! shims ruby gerados no dir de runtime (exec/repl/serve/task/calisto.*).
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — shims (extraido de src/main.rs na reorg do CLI).

use std::fs;
use std::path::{Path, PathBuf};
use crate::runtime::*;







pub const RAKE_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto: equivale ao bin/rake do Rails (`load Gem.bin_path`),
# sem depender de binstub existir. O child roda com o Gemfile ativo.
begin
  load Gem.bin_path(\"rake\", \"rake\")
rescue Gem::GemNotFoundException => e
  warn \"calisto task: rake nao encontrado no bundle: #{e.message}\"
  exit 1
end
";



// ---- Fase P: APIs nativas calisto.* -----------------------------------------

/// Shim de `require "calisto/sqlite"` (Fase P): os metodos nativos sao
/// registrados no boot do daemon (Rust -> rb_define_method); este arquivo so
/// torna o require resolvivel no $LOAD_PATH. Sem o modulo (--cold, daemon
/// legado, libsqlite3 ausente) levanta LoadError claro.
pub const SQLITE_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto (Fase P): marcador das APIs nativas `calisto/sqlite`
# (binding Rust sobre libsqlite3 do sistema, registrado no boot do daemon).
unless defined?(Calisto::SQLite)
  raise LoadError, \"calisto/sqlite e nativo do daemon calisto (indisponivel em --cold ou com daemon legado)\"
end
Calisto::SQLite
";



/// Shim de `require "calisto/hash"`: no daemon o modulo e nativo (Rust);
/// em --cold cai no fallback puro com Digest::SHA256 (mesmo hexdigest —
/// paridade cold/warm). blake3 nao existe na stdlib — erro claro no cold.
pub const HASH_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto (Fase P): marcador das APIs nativas `calisto/hash`
# (sha256/blake3 em Rust no daemon). Fallback puro em --cold: sha256 via
# Digest (mesmo hexdigest — paridade cold/warm); blake3 e so nativo.
if defined?(Calisto::Hash)
  Calisto::Hash
else
  require \"digest\"
  module Calisto
    module Hash
      def self.sha256(data)
        Digest::SHA256.hexdigest(data)
      end

      def self.blake3(_data)
        raise NotImplementedError, \"Calisto::Hash.blake3 requer o daemon calisto (indisponivel em --cold)\"
      end

      def self.xxh64(_data, _seed = 0)
        raise NotImplementedError, \"Calisto::Hash.xxh64 requer o daemon calisto (indisponivel em --cold)\"
      end
    end
  end
end
";



/// Shim de `require "calisto/base64"` (Fase T): nativo no daemon (Rust);
/// em --cold cai no stdlib base64 (pure Ruby) — mesma saida, paridade
/// cold/warm (so perde a velocidade nativa).
pub const BASE64_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto (Fase T): marcador das APIs nativas `calisto/base64`
# (Rust no daemon). Fallback puro em --cold: stdlib base64 (mesma saida).
if defined?(Calisto::Base64)
  Calisto::Base64
else
  require \"base64\"
  module Calisto
    module Base64
      def self.encode64(bin) = ::Base64.encode64(bin)
      def self.decode64(str) = ::Base64.decode64(str)
      def self.strict_encode64(bin) = ::Base64.strict_encode64(bin)
      def self.strict_decode64(str) = ::Base64.strict_decode64(str)
      def self.urlsafe_encode64(bin, padding: true) = ::Base64.urlsafe_encode64(bin, padding:)
      def self.urlsafe_decode64(str) = ::Base64.urlsafe_decode64(str)
    end
  end
end
";



/// Shim de `require "calisto/url"` (Fase T): nativo no daemon; em --cold cai
/// no CGI da stdlib (mesma semantica escape/unescape).
pub const URL_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto (Fase T): marcador das APIs nativas `calisto/url`
# (Rust no daemon). Fallback puro em --cold: CGI da stdlib.
if defined?(Calisto::URL)
  Calisto::URL
else
  require \"cgi\"
  module Calisto
    module URL
      def self.escape(s) = CGI.escape(s)
      def self.unescape(s) = CGI.unescape(s)
    end
  end
end
";



/// Shim de `require "calisto/html"` (Fase T): nativo no daemon (Bun.
/// escapeHTML do Ruby); em --cold cai no ERB::Util da stdlib.
pub const HTML_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto (Fase T): marcador das APIs nativas `calisto/html`
# (Rust no daemon). Fallback puro em --cold: ERB::Util da stdlib.
if defined?(Calisto::HTML)
  Calisto::HTML
else
  require \"erb\"
  module Calisto
    module HTML
      def self.escape(s) = ERB::Util.html_escape(s)
    end
  end
end
";



/// Shim de `require "pg"` (camada de compatibilidade com a gem pg): as
/// classes PG::Connection/PG::Result sao nativas (Rust sobre libpq,
/// registradas no boot do daemon); este arquivo completa o surface do
/// ActiveRecord (constantes, type maps/decoders, PG::Tuple, each/rows) e
/// torna o require resolvivel. O dir `gems/` so e injetado no daemon/child —
/// em --cold o require resolve a gem pg REAL (paridade cold/warm; sem o
/// modulo nativo o shim levanta LoadError claro, como o sqlite).
pub const PG_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto: camada de compatibilidade com a gem `pg` no daemon.
# PG::Connection/PG::Result sao nativos (Rust sobre libpq, registrados no
# boot do daemon — marcador PG::CALISTO_NATIVE); este arquivo completa o
# surface do ActiveRecord (constantes, type maps/decoders, PG::Tuple, each)
# e so torna o require resolvivel. O dir gems/ so e injetado no daemon/
# child — em --cold o require resolve a gem pg real (paridade). Sem libpq
# no daemon (register degradou) o shim CAI na gem pg real instalada (mesma
# semantica do cold) em vez de quebrar o app — e nao define os stubs.
unless defined?(PG::Connection)
  begin
    gem \"pg\"
    spec = Gem::Specification.find_by_name(\"pg\")
    require File.join(spec.full_gem_path, \"lib\", \"pg\")
  rescue Gem::LoadError
    raise LoadError, \"pg nativo requer o daemon calisto com libpq (CALISTO_LIBPQ/sistema/vendored) e a gem pg nao esta instalada p/ fallback\"
  end
end

if defined?(PG::CALISTO_NATIVE)
  module PG
  VERSION = \"1.6.0\"

  CONNECTION_OK = 0
  PQTRANS_IDLE = 0
  PQTRANS_ACTIVE = 1
  PQTRANS_INTRANS = 2
  PQTRANS_INERROR = 3
  PQTRANS_UNKNOWN = 4

  # --- type maps/decoders (stubs: aceitos e armazenados; a conversao
  # nativa devolve strings — o typecast do AR e a fonte da verdade) ---
  class SimpleDecoder
    attr_reader :oid, :name

    def initialize(oid: nil, name: nil, **)
      @oid = oid
      @name = name
    end

    def to_h = { oid: @oid, name: @name }
    def decode(value, _tuple = nil, _field = nil) = value
  end

  module TextEncoder
    class Integer < PG::SimpleDecoder; end
    class Boolean < PG::SimpleDecoder; end
  end

  module TextDecoder
    class Integer < PG::SimpleDecoder; end
    class Float < PG::SimpleDecoder; end
    class Numeric < PG::SimpleDecoder; end
    class Boolean < PG::SimpleDecoder; end
    class Date < PG::SimpleDecoder; end
    class TimestampUtc < PG::SimpleDecoder; end
    class TimestampWithoutTimeZone < PG::SimpleDecoder; end
    class TimestampWithTimeZone < PG::SimpleDecoder; end
    class Bytea < PG::SimpleDecoder; end
  end

  class TypeMapByOid
    def initialize
      @coders = []
    end

    attr_accessor :default_type_map

    def add_coder(coder)
      @coders << coder
      self
    end

    def clear = @coders.clear
    def each_coder(&blk) = @coders.each(&blk)
  end

  class TypeMapByClass
    def initialize
      @map = {}
    end

    def []=(klass, coder)
      @map[klass] = coder
    end

    def [](klass) = @map[klass]
  end

  # --- PG::Tuple: acesso por nome/indice (pg 1.5+; AR usa row[\"typname\"]) ---
  class Tuple
    def initialize(result, index)
      @result = result
      @index = index
    end

    def [](key)
      idx = key.is_a?(Integer) ? key : @result.fields.index(key)
      return nil unless idx
      @result.typed_getvalue(@index, idx)
    end

    def to_a
      Array.new(@result.nfields) { |j| @result.typed_getvalue(@index, j) }
    end

    def values = to_a
    def to_h = @result.fields.zip(to_a).to_h
    def length = @result.nfields
    alias size length
    def key?(k) = @result.fields.include?(k)
  end
end

class PG::Result
  include Enumerable

  # each com bloco entrega PG::Tuple (semantica pg >= 1.5, que o AR 7.1+
  # usa em add_pg_decoders: row[\"typname\"]); sem bloco, Enumerator.
  def each
    return enum_for(:each) unless block_given?
    n = ntuples
    i = 0
    while i < n
      yield PG::Tuple.new(self, i)
      i += 1
    end
    self
  end

  def each_row(&blk)
    return enum_for(:each_row) unless blk
    each(&blk)
  end
end

class PG::Connection
  # type maps armazenados como ivars (nunca usados na conversao — strings).
  def type_map_for_queries=(map)
    @calisto_type_map_for_queries = map
    map
  end

  def type_map_for_queries = @calisto_type_map_for_queries

  def type_map_for_results=(map)
    @calisto_type_map_for_results = map
    _calisto_result_type_map!(map) # marca o decode por OID no native
    map
  end

  def type_map_for_results = @calisto_type_map_for_results
end
end

PG
";



/// Escreve os shims nativos no dir (idempotente). O daemon chama no boot
/// (cobre run/test/task/serve/exec/repl/status/stop) e o cold mode chama
/// via `native_shims_dir` para o `-I`.
pub fn ensure_native_shims(dir: &Path) {
    let _ = fs::create_dir_all(dir.join("calisto"));
    let _ = fs::create_dir_all(dir.join("gems"));
    for (name, content) in [
        ("calisto/sqlite.rb", SQLITE_SHIM),
        ("calisto/hash.rb", HASH_SHIM),
        ("calisto/base64.rb", BASE64_SHIM),
        ("calisto/url.rb", URL_SHIM),
        ("calisto/html.rb", HTML_SHIM),
        ("gems/pg.rb", PG_SHIM),
    ] {
        let p = dir.join(name);
        // reescreve quando o conteudo difere (nao so quando falta): um dir de
        // runtime antigo com shim stale (ex.: antes de um bump do PG_SHIM)
        // silenciosamente nao aplica as mudancas — o daemon novo repara sozinho
        let stale = fs::read_to_string(&p).map(|old| old != content).unwrap_or(true);
        if stale {
            if let Err(e) = fs::write(&p, content) {
                eprintln!("calisto: warning: nao consegui escrever {}: {e}", p.display());
            }
        }
    }
}



/// Dir de runtime com os shims nativos (para o `-I` do cold mode).
pub fn native_shims_dir(ruby: &Path) -> PathBuf {
    let dir = daemon_dir_for(ruby);
    fs::create_dir_all(&dir).ok();
    ensure_native_shims(&dir);
    dir
}



// ---- Fase E: calisto serve ---------------------------------------------------

pub const SERVE_LAUNCHER: &str = "# frozen_string_literal: true
# Gerado pelo calisto: serve o config.ru do cwd (Rack app) no daemon quente,
# como child do fork — o boot da app ja foi pago no daemon.
require \"rack\"
begin
  require \"rackup\"
rescue LoadError
  # rack 2: Rack::Server continua no proprio rack
end
port = Integer(ENV.fetch(\"CALISTO_SERVE_PORT\", \"3000\"))
host = ENV.fetch(\"CALISTO_SERVE_HOST\", \"127.0.0.1\")
config = File.join(Dir.pwd, \"config.ru\")
abort \"calisto serve: #{config} nao existe (rode na raiz do projeto)\" unless File.file?(config)
app, = Rack::Builder.parse_file(config)
if defined?(Rackup::Server)
  Rackup::Server.start(app: app, Host: host, Port: port,
                       environment: ENV.fetch(\"RACK_ENV\", \"development\"))
elsif Rack::Server.respond_to?(:start)
  Rack::Server.start(app: app, Host: host, Port: port)
else
  abort \"calisto serve: precisa de rackup (rack 3) ou rack 2 no Gemfile\"
end
";



// ---- Fase G: calisto exec ----------------------------------------------------

pub const EXEC_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto: `calisto exec <bin>` — resolve como `bundle exec` e roda
# o binario no daemon quente (child do fork, Gemfile ja ativo). Resolucao:
#   1. argumento que e um caminho de arquivo executavel (ex.: ./bin/rails)
#   2. spec do bundle ativo (Gem.loaded_specs) cujo executavel e o nome — nao
#      depende de binstub (`bundle binstubs` desnecessario)
#   3. PATH (binarios de sistema)
# Binario ruby (shebang ruby) e carregado in-process, como o kernel_load do
# bundler: $0 = caminho, ARGV = args, `load` — sem depender de shebang no PATH
# nem de re-exec. Binario nativo e `exec` direto.
name = ARGV.shift
abort \"calisto exec: uso: calisto exec <bin> [args...]\" if name.nil? || name.empty?

def calisto_exec_resolve(name)
  # 1. caminho de arquivo (como Bundler.which): relativo/absoluto
  if name.include?(File::SEPARATOR)
    path = File.expand_path(name)
    return path if File.file?(path) && File.executable?(path)
  end
  # 2. specs ativadas pelo bundle (Bundler.setup ja rodou no child do fork).
  #    Default gems aparecem 2x (default + instalada) — dedup por nome; so e
  #    ambiguidade de verdade quando gems DIFERENTES fornecem o executavel.
  specs = Gem.loaded_specs.values.select { |s| s.executables.include?(name) }
  specs = specs.uniq(&:name)
  if specs.size == 1
    return Gem.bin_path(specs.first.name, name)
  elsif specs.size >= 2
    return specs # ambiguidade real: 2+ gems diferentes fornecem o executavel
  end
  # 0 specs: cai no PATH (binarios de sistema, como `bundle exec` fora de bundle)
  # 3. PATH
  ENV.fetch(\"PATH\", \"\").split(File::PATH_SEPARATOR).each do |dir|
    next if dir.empty?
    path = File.join(dir, name)
    return path if File.file?(path) && File.executable?(path)
  end
  nil
end

def calisto_exec_ruby?(file)
  first = File.open(file, \"rb\") { |f| f.read(64).to_s }
  first.start_with?(\"#!/usr/bin/env ruby\", \"#!/usr/bin/env jruby\",
                   \"#!/usr/bin/env truffleruby\", \"#!#{Gem.ruby}\")
end

found = calisto_exec_resolve(name)
case found
when nil
  warn \"calisto exec: comando nao encontrado: #{name}\"
  warn \"calisto exec: instale a gem que o fornece no Gemfile (bundle add ...) ou use o PATH\"
  exit 127
when Array
  # divergencia do bundle exec (que pega o primeiro do PATH): ambiguidade e
  # erro claro com os candidatos, em vez de ordem arbitraria
  warn \"calisto exec: '#{name}' ambiguo: #{found.map(&:name).join(', ')}\"
  exit 1
end

args = ARGV
if calisto_exec_ruby?(found)
  # kernel_load do bundler: in-process, sem shebang/re-exec
  $0 = found
  ARGV.replace(args)
  load found
else
  begin
    exec found, *args
  rescue Errno::EACCES, Errno::ENOEXEC
    warn \"calisto exec: nao executavel: #{name} (#{found})\"
    exit 126
  rescue Errno::ENOENT
    warn \"calisto exec: comando nao encontrado: #{name}\"
    exit 127
  end
end
";



// ---- Fase G: calisto repl ----------------------------------------------------

pub const REPL_SHIM: &str = "# frozen_string_literal: true
# Gerado pelo calisto: `calisto repl` — IRB no daemon quente, no contexto da
# app pre-carregada (fork do boot congelado). Args sao repassados ao IRB
# (IRB.setup faz parse de ARGV, como o binario `irb`).
require \"irb\"
IRB.start
";
