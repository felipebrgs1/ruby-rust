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
# Limites v1: requires dinamicos nao sao embutidos (warning); assets nao-Ruby
# ficam externos; C extensions (.so) nao embutidas.

require "ripper"

entry, out, root = ARGV
abort "uso: build.rb <entry> <out> <root>" unless entry && out && root
entry = File.expand_path(entry)
root = File.expand_path(root)
abort "entry nao encontrado: #{entry}" unless File.file?(entry)
abort "root nao e diretorio: #{root}" unless File.directory?(root)

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

# Caminha o sexp achando require/require_relative com argumento literal:
#   require "x"            -> [:command, [:@ident, "require"], [:args_add_block, ...]]
#   require("x")           -> [:method_add_arg, [:fcall, [:@ident, "require"]], [:arg_paren, ...]]
def walk_requires(sexp, requires, file)
  return unless sexp.is_a?(Array)

  kind_node = nil
  args_node = nil
  if sexp[0] == :command && sexp[1].is_a?(Array) && sexp[1][0] == :@ident
    kind_node, args_node = sexp[1], sexp[2]
  elsif sexp[0] == :method_add_arg && sexp[1].is_a?(Array) && sexp[1][0] == :fcall
    kind_node, args_node = sexp[1][1], sexp[2]&.dig(1)
  end

  if kind_node && %w[require require_relative].include?(kind_node[1])
    kind = kind_node[1].to_sym
    if args_node.is_a?(Array) && args_node[0] == :args_add_block
      ident = static_string(args_node[1]&.first)
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
    cands << File.expand_path(ident, lp)
    cands << File.expand_path("#{ident}.rb", lp)
  end
  cands.find { |c| File.file?(c) }
end

load_path = [root, *$LOAD_PATH]

files = {} # abs => true, ordem de descoberta (entry primeiro)
index = {} # ident do require => abs (requeridos por nome)
warnings = []
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
      when :require then resolve_require(ident, load_path)
      end

    if resolved.nil?
      warnings << "#{kind} #{ident.inspect} nao resolvido em #{file}"
    elsif resolved.start_with?("#{root}/")
      index[ident] = resolved unless kind == :require_relative
      queue << resolved unless files[resolved]
    end
    # resolvido fora da raiz (stdlib): nao embute
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
    ENTRY = $calisto_entry
    DATA = $calisto_data
    LOADED = {}

    def self.run
      if DATA
        require "stringio"
        Object.const_set(:DATA, StringIO.new(DATA))
      end
      load_file(ENTRY)
    end

    def self.load_file(abs)
      return false if LOADED[abs]
      code = CODE[abs]
      raise LoadError, "cannot load such file -- #{abs}" unless code
      LOADED[abs] = true
      $LOADED_FEATURES << abs unless $LOADED_FEATURES.include?(abs)
      eval(code, TOPLEVEL_BINDING, abs, 1) # __FILE__/__dir__ corretos
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
      calisto_original_require_relative(path)
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
out_src << "$calisto_entry = #{entry.dump}\n"
out_src << "$calisto_data = #{entry_data.nil? ? "nil" : entry_data.dump}\n\n"
out_src << LOADER
out_src << "\n"

File.write(out, out_src)
puts "BUNDLED #{files.size}"
