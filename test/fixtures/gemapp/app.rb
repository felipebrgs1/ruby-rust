# frozen_string_literal: true

# Fase A: prova que o Gemfile do cwd foi ativado (semantica de `bundle exec`).
# Sem bundler ativo estas linhas divergem: as gems do Gemfile nao entram no
# $LOAD_PATH, e a saida do teste de golden falharia.

puts "gemfile=#{Bundler.default_gemfile.basename}"
puts "minitest_in_loadpath=#{$LOAD_PATH.any? { |p| p.include?("minitest-") }}"
puts "testunit_in_loadpath=#{$LOAD_PATH.any? { |p| p.include?("test-unit-") }}"
puts "rake_in_loadpath=#{$LOAD_PATH.any? { |p| p.include?("rake-") }}"
puts "rdoc_in_loadpath=#{$LOAD_PATH.any? { |p| p.include?("rdoc-") }}"

# usa as gems de verdade (nao so checa o load path)
require "csv"
require "rake"
require "rdoc"
rows = CSV.parse("a,b\n1,2")
puts "csv_rows=#{rows.length}"
puts "rake_version=#{Rake::VERSION}"
puts "rdoc_version=#{RDoc::VERSION}"
