#!/usr/bin/env ruby
# frozen_string_literal: true
#
# calisto build -- bundler de arquivos Ruby (stdlib-only).
#
# Uso: ruby build.rb <entry> <out> <root>
#
# Analisa requires estaticos com Ripper (o lexer real do Ruby), coleta os
# arquivos do projeto sob <root>, e emite um bundle unico com um loader que
# intercepta `require`/`require_relative` em runtime:
#   - arquivos do projeto ficam embutidos (avaliados com `eval` usando o
#     caminho original como filename, entao __FILE__/__dir__/require_relative
#     continuam corretos)
#   - arquivos fora da raiz (stdlib) nao sao embutidos: o require original
#     roda normalmente
#   - DATA/__END__ do entrypoint e emulado com StringIO
#
# Limites v1: requires dinamicos de .rb nao sao embutidos (warning); assets
# nao-Ruby ficam externos. C extensions (.so) SAO embutidas como bytes
# (extraidas p/ tmpdir no runtime) — o pre-indice por nome canonico de require
# cobre requires dinamicos de nativos (ex.: sqlite3).

require "ripper"

entry, out, root, compile = ARGV
abort "uso: build.rb <entry> <out> <root> [--compile]" unless entry && out && root
entry = File.expand_path(entry)
root = File.expand_path(root)
abort "entry nao encontrado: #{entry}" unless File.file?(entry)
abort "root nao e diretorio: #{root}" unless File.directory?(root)
compile = compile == "1" || compile == "--compile"

# --- analise estatica ---------------------------------------------------------

def static_string(node)
  return nil unless node.is_a?(Array)
  return nil unless node[0] == :string_literal
  content = node[1]
  return nil unless content.is_a?(Array) && content[0] == :string_content
  parts = content[1..]
  return nil if parts.any? { |p| !(p.is_a?(Array) && p[0] == :@tstring_content) }
  parts.map { |p| p[1] }.join
end

# Caminha o sexp achando require/require_relative/autoload com argumento
# literal:
#   require "x"            -> [:command, [:@ident, "require"], [:args_add_block, ...]]
#   require("x")           -> [:method_add_arg, [:fcall, [:@ident, "require"]], [:arg_paren, ...]]
#   autoload :X, "x"       -> [:command, [:@ident, "autoload"], [:args_add_block, ...]]
#                            (o ident do require e o 2o argumento; o 1o e o const)
# autoload importa: o rack (entre outros) autoloada classes por nome e o
# runtime dispara `require "rack/x"` — sem o indice, o bundle quebra.
def walk_requires(sexp, requires, file)
  return unless sexp.is_a?(Array)

  kind_node = nil
  args_node = nil
  if sexp[0] == :command && sexp[1].is_a?(Array) && sexp[1][0] == :@ident
    kind_node, args_node = sexp[1], sexp[2]
  elsif sexp[0] == :method_add_arg && sexp[1].is_a?(Array) && sexp[1][0] == :fcall
    kind_node, args_node = sexp[1][1], sexp[2]&.dig(1)
  end

  if kind_node && %w[require require_relative autoload].include?(kind_node[1])
    kind = kind_node[1].to_sym
    if args_node.is_a?(Array) && args_node[0] == :args_add_block
      str_arg = kind == :autoload ? args_node[1]&.last : args_node[1]&.first
      ident = static_string(str_arg)
      if ident
        requires << [kind, ident]
      else
        warn "calisto build: warning: #{kind} dinamico ignorado (nao embutivel) em #{file}"
      end
    end
  end

  sexp.each { |child| walk_requires(child, requires, file) }
end

# Corta o codigo no marcador __END__ real (via lexer; ignora "__END__" em
# strings/comentarios). Retorna [codigo, dados_apos_o_marcador].
def split_end_marker(src)
  line = nil
  Ripper.lex(src).each do |(l, _c), event, _tok, _st|
    line = l if event == :on___end__
  end
  return [src, nil] unless line

  pos = 0
  (line - 1).times do
    nl = src.index("\n", pos)
    break unless nl
    pos = nl + 1
  end
  marker_end = src.index("\n", pos)
  data = marker_end ? src[(marker_end + 1)..] : ""
  [src[0...pos], data]
end

# --- coleta -------------------------------------------------------------------

def resolve_relative(ident, file)
  base = File.dirname(File.expand_path(file))
  [File.expand_path(ident, base), File.expand_path("#{ident}.rb", base)].find do |c|
    File.file?(c)
  end
end

def resolve_require(ident, load_path)
  cands = []
  load_path.each do |lp|
    # mesma ordem do require do ruby: nome exato, .rb, .so, .bundle
    cands << File.expand_path(ident, lp)
    cands << File.expand_path("#{ident}.rb", lp)
    cands << File.expand_path("#{ident}.so", lp)
    cands << File.expand_path("#{ident}.bundle", lp)
  end
  cands.find { |c| File.file?(c) }
end

# --- gems (Fase F: --compile) -------------------------------------------------
# Embutir gems do Gemfile.lock no bundle: o loader intercepta o require por
# nome, entao o executavel roda sem bundle install/rubygems no sistema.
# Dirigido por requires para os .rb; nativos (.so/.bundle) de gems C-ext sao
# embutidos como bytes (base64) e extraidos p/ tmpdir no runtime — covers
# requires dinamicos via pre-indice pelo nome canonico (sqlite3 require
# "sqlite3/#{RUBY_VERSION}/sqlite3_native"). Compilar do zero continua com o
# bundle install (decisao da Fase A: sem toolchain propria) — o build so
# embute o que ja foi compilado no GEM_PATH da app.

def find_gemfile_lock(start)
  d = File.expand_path(start)
  loop do
    f = File.join(d, "Gemfile.lock")
    return f if File.file?(f)
    parent = File.dirname(d)
    return nil if parent == d
    d = parent
  end
end

# Resolve as specs do Gemfile.lock (caminhos reais + extensions) usando o
# GEM_PATH do app (vendor/bundle) — o find_by_name e do rubygems puro, sem
# ativar o bundler (o build.rb roda fora de bundle).
def resolve_gems(gemfile_dir)
  require "bundler"
  lock = File.join(gemfile_dir, "Gemfile.lock")
  return {} unless File.file?(lock)

  vendored = Dir["#{gemfile_dir}/vendor/bundle/ruby/*/"].first
  if vendored
    Gem.paths = { "GEM_PATH" => [vendored, *Gem.default_path].join(File::PATH_SEPARATOR) }
    Gem::Specification.reset
  end

  specs = {}
  Bundler::LockfileParser.new(File.read(lock)).specs.each do |lazy|
    spec = begin
      Gem::Specification.find_by_name(lazy.name, lazy.version)
    rescue Gem::LoadError
      warn "calisto build: warning: gem #{lazy.name} (#{lazy.version}) nao instalada — nao embutida"
      next
    end
    specs[lazy.name] = spec
  end
  specs
end

# Dir de extensoes compiladas do gem pelo bundle install: o .so vive fora do
# require_path (extensions/<plat>/<ver>/<gem>-<v>/). spec.extension_dir e o
# caminho canonico; fallback glob no Gem.dir (o "-static" do api version).
def gem_extension_dirs(spec)
  dirs = []
  ed = spec.extension_dir
  dirs << ed if File.directory?(ed)
  if dirs.empty?
    glob = Dir[File.join(Gem.dir, "extensions", "**", "#{spec.name}-#{spec.version}*")]
           .find { |d| File.directory?(d) }
    dirs << glob if glob
  end
  dirs
end

# Arquivos nativos do gem (.so/.bundle): nos require_paths (gems precompiladas,
# ex.: sqlite3-x86_64-linux-gnu) e no dir de extensoes compiladas.
def gem_native_files(spec)
  files = []
  (spec.full_require_paths + gem_extension_dirs(spec)).each do |dir|
    Dir.glob(File.join(dir, "**", "*.{so,bundle}")).each { |f| files << f if File.file?(f) }
  end
  files.uniq
end

# require_path => [gem_name, c_ext?] — o BFS resolve requires nestes paths
# (require_paths + dirs de extensao) e decide embutir (.rb) ou extrair no
# runtime (.so/.bundle). C-ext = extensions declaradas OU nativo presente
# (gems precompiladas nao declaram extensions).
def gem_path_map(specs)
  map = {}
  specs.each_value do |spec|
    c_ext = !spec.extensions.empty? || !gem_native_files(spec).empty?
    (spec.full_require_paths + gem_extension_dirs(spec)).each { |p| map[p] = [spec.name, c_ext] }
  end
  map
end

def gem_origin(path, root, gem_path_map)
  # gems primeiro: o vendor/bundle fica DENTRO do root, entao o teste do
  # projeto so vale se nenhuma require_path de gem casou
  gem_path_map.each do |require_path, meta|
    return meta if path.start_with?("#{require_path}/")
  end
  if path.start_with?("#{root}/")
    :project
  else
    :stdlib
  end
end

load_path = [root, *$LOAD_PATH]
original_load_path = $LOAD_PATH.dup
gem_specs = {}
gem_paths = {}

if compile
  gemfile_dir = find_gemfile_lock(root)&.then { |f| File.dirname(f) }
  if gemfile_dir
    gem_specs = resolve_gems(gemfile_dir)
    gem_paths = gem_path_map(gem_specs)
    # Gem.paths= recalcula o $LOAD_PATH e remove as default gems (stringio
    # etc.) — o resolvedor usa a copia original; o runtime do bundle resolve
    # essas via rubygems de qualquer forma.
    load_path = [root, *gem_paths.keys, *original_load_path]
    warn "calisto build: --compile com #{gem_specs.size} gem(s) do Gemfile.lock" unless gem_specs.empty?
  else
    warn "calisto build: warning: --compile sem Gemfile.lock (nada a embutir)"
  end
end

files = {} # abs => true, ordem de descoberta (entry primeiro)
index = {} # ident do require => abs (requeridos por nome)
warnings = []
embedded_gems = {} # gem_name => version (para o header)
native = {} # abs => bytes brutos (C extension; base64 na geracao)
native_gems = {} # gem_name => true (gems C-ext alcancadas: coletar nativos)
autoload_map = {} # arquivo que registra autoload => [alvos embutidos]
queue = [entry]

until queue.empty?
  file = queue.shift
  next if files[file]
  files[file] = true

  src = File.read(file)
  requires = []
  walk_requires(Ripper.sexp(src), requires, file)

  requires.each do |kind, ident|
    resolved =
      case kind
      when :require_relative then resolve_relative(ident, file)
      when :require, :autoload then resolve_require(ident, load_path)
      end

    if resolved.nil?
      warnings << "#{kind} #{ident.inspect} nao resolvido em #{file}"
      next
    end

    origin = gem_origin(resolved, root, gem_paths)
    if origin == :stdlib
      # fora da raiz e das gems (stdlib/default gem): nao embute
      next
    elsif origin.is_a?(Array) # [gem_name, c_ext?]
      name, = origin
      case File.extname(resolved)
      when ".so", ".bundle"
        # C extension: embutida como bytes; extraida e require'd no runtime
        native[resolved] = File.binread(resolved)
        native_gems[name] = true
        index[ident] = resolved unless kind == :require_relative
      when ".rb"
        embedded_gems[name] ||= gem_specs[name]&.version.to_s
        native_gems[name] = true # gems C-ext: coleta os .so nao-vistos apos o BFS
        index[ident] = resolved unless kind == :require_relative
        (autoload_map[file] ||= []) << resolved if kind == :autoload
        queue << resolved unless files[resolved]
      else
        # asset nao-Ruby alcancado por nome (ex.: dado do gem): nao embute
        warnings << "gem #{name}: asset #{File.basename(resolved)} nao embutido"
      end
    else # :project
      index[ident] = resolved unless kind == :require_relative
      (autoload_map[file] ||= []) << resolved if kind == :autoload
      queue << resolved unless files[resolved]
    end
  end
end

# Nativos de gems C-ext alcancadas pelo grafo: coleta TAMBEM os .so que o
# Ripper nao ve (requires dinamicos, ex.: sqlite3 require
# "sqlite3/#{RUBY_VERSION}/sqlite3_native") e pre-indice cada um pelo nome
# canonico de require (relativo ao require_path/dir de extensao, sem
# extensao) — o loader entao resolve o require dinamico no runtime.
# (each_key: no ruby 3.4, Hash#each com bloco de aridade 1 entrega [k, v]).
native_gems.each_key do |name|
  spec = gem_specs[name]
  next unless spec
  gem_native_files(spec).each { |f| native[f] ||= File.binread(f) }
end
native.each_key do |abs|
  gem_paths.each do |require_path, meta|
    next unless abs.start_with?("#{require_path}/")
    rel = abs.delete_prefix("#{require_path}/").sub(/\.(so|bundle)\z/, "")
    index[rel] = abs unless index.key?(rel)
    break
  end
end

warnings.each { |w| warn "calisto build: warning: #{w}" }

# --- geracao ------------------------------------------------------------------

code_map = {}
entry_data = nil
files.each_key do |f|
  code, data = split_end_marker(File.read(f))
  code_map[f] = code
  entry_data = data if f == entry
end

LOADER = <<~'CALISTO'
  # --- loader do bundle (gerado) ---
  module CalistoBundle
    CODE = $calisto_code
    INDEX = $calisto_index
    NATIVE = $calisto_native # abs => base64 (C extension embutida)
    AUTOLOADS = $calisto_autoloads
    ENTRY = $calisto_entry
    DATA = $calisto_data
    LOADING = {} # arquivos com eval em andamento
    LOADED = {}

    # decoder hand-rolled (mesma filosofia do daemon: sem require "base64" —
    # default gem pinada pode colidir com o bundle da app)
    B64D = {}
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".each_char.with_index { |c, i| B64D[c] = i }

    def self.b64decode(s)
      out = +"".b
      buf = 0
      bits = 0
      s.each_char do |c|
        next if c == "=" || c == "\n" # padding e quebras (pack m0 nao emite \n)
        v = B64D[c] or raise "calisto bundle: base64 invalido"
        buf = ((buf << 6) | v) & 0x3fffffff
        bits += 6
        next unless bits >= 8
        bits -= 8
        out << ((buf >> bits) & 0xff)
      end
      out
    end

    # C extension embutida como bytes: extrai para um cache no tmpdir (chave =
    # abs sanitizado — versoes/plataformas diferentes nao colidem) e require o
    # caminho absoluto; o dlopen roda no interpretador do runtime.
    def self.load_native(abs, b64)
      tmp = ENV["TMPDIR"]
      tmp = "/tmp" if tmp.nil? || tmp.empty?
      base = File.join(tmp, "calisto-native")
      begin
        Dir.mkdir(base)
      rescue Errno::EEXIST
      end
      dir = File.join(base, abs.tr("/.", "__"))
      begin
        Dir.mkdir(dir)
      rescue Errno::EEXIST
      end
      out = File.join(dir, File.basename(abs))
      unless File.file?(out)
        File.binwrite(out, b64decode(b64))
      end
      require out
      LOADED[abs] = true
      $LOADED_FEATURES << abs unless $LOADED_FEATURES.include?(abs)
      true
    end

    def self.run
      if DATA
        require "stringio"
        Object.const_set(:DATA, StringIO.new(DATA))
      end
      load_file(ENTRY)
    end

    # Bug do CRuby 3.4: autoload registrado via eval dispara na DEFINICAO do
    # const (NameError). Pre-carregar os alvos dos autoloads ANTES do arquivo
    # registrador deixa o const definido no momento do registro — autoload
    # sobre const definido e inofensivo (rack 3 usa ~30 autoloads). Os alvos
    # podem depender entre si (rack-protection: filhos de Base) — rodadas com
    # retry: um alvo que falha por const ausente espera o pai carregar.
    def self.preload(autoloads)
      pending = autoloads.dup
      until pending.empty?
        progressed = false
        pending = pending.reject do |t|
          begin
            load_file(t)
            progressed = true
            true
          rescue NameError
            false # dependencia ainda nao carregada: tenta na proxima rodada
          end
        end
        break unless progressed # ciclo real: o runtime resolve (ou falha)
      end
    end

    def self.load_file(abs)
      return false if LOADED[abs] || LOADING[abs]
      if NATIVE[abs]
        LOADING[abs] = true
        begin
          load_native(abs, NATIVE[abs])
        ensure
          LOADING.delete(abs)
        end
        return true
      end
      code = CODE[abs]
      raise LoadError, "cannot load such file -- #{abs}" unless code
      LOADING[abs] = true
      preload(AUTOLOADS[abs] || [])
      begin
        eval(code, TOPLEVEL_BINDING, abs, 1) # __FILE__/__dir__ corretos
      ensure
        LOADING.delete(abs)
      end
      LOADED[abs] = true
      $LOADED_FEATURES << abs unless $LOADED_FEATURES.include?(abs)
      true
    end
  end

  module Kernel
    alias calisto_original_require require
    alias calisto_original_require_relative require_relative

    def require(path)
      abs = CalistoBundle::INDEX[path]
      return CalistoBundle.load_file(abs) if abs
      calisto_original_require(path)
    end

    def require_relative(path)
      base = File.dirname(File.expand_path(caller_locations(1, 1).first.path))
      abs = File.expand_path(path, base)
      if CalistoBundle::CODE[abs]
        return CalistoBundle.load_file(abs)
      elsif CalistoBundle::CODE["#{abs}.rb"]
        return CalistoBundle.load_file("#{abs}.rb")
      end
      # chamador nao embutido (stdlib): delega com o caminho ABSOLUTO — o
      # require_relative nativo resolveria o path relativo contra ESTE
      # arquivo (o bundle), nao contra o chamador real
      calisto_original_require_relative(abs)
    end

    private :require, :require_relative
  end

  $0 = CalistoBundle::ENTRY
  CalistoBundle.run
CALISTO

out_src = +""
out_src << "# gerado por calisto build\n"
out_src << "# entry: #{entry}\n"
out_src << "# arquivos (#{files.size}):\n"
files.each_key { |f| out_src << "#   #{f}\n" }
unless embedded_gems.empty?
  out_src << "# gems embutidas (#{embedded_gems.size}):\n"
  embedded_gems.sort.each { |name, ver| out_src << "#   #{name}-#{ver}\n" }
end
unless native.empty?
  out_src << "# nativos embutidos (#{native.size}):\n"
  native.keys.sort.each { |f| out_src << "#   #{f}\n" }
end
out_src << "\n"
out_src << "$calisto_code = {\n"
code_map.each do |f, code|
  out_src << "  #{f.dump} => #{code.dump},\n"
end
out_src << "}\n\n"
out_src << "$calisto_index = {\n"
index.each do |ident, f|
  out_src << "  #{ident.dump} => #{f.dump},\n"
end
out_src << "}\n\n"
out_src << "$calisto_native = {\n"
native.sort.each do |f, data|
  out_src << "  #{f.dump} => #{[data].pack("m0").dump},\n"
end
out_src << "}\n\n"
out_src << "$calisto_entry = #{entry.dump}\n"
out_src << "$calisto_data = #{entry_data.nil? ? "nil" : entry_data.dump}\n"
out_src << "$calisto_autoloads = {\n"
autoload_map.each do |f, targets|
  out_src << "  #{f.dump} => [#{targets.map(&:dump).join(", ")}],\n"
end
out_src << "}\n\n"
out_src << LOADER
out_src << "\n"

File.write(out, out_src)
puts "BUNDLED #{files.size}"
