# frozen_string_literal: true

# Verifica o carregamento do .env (Fase E) no modo warm e no --cold
# (paridade: o .env vive no cliente, os dois caminhos herdam o mesmo env).

puts "CALISTO_DOTENV=#{ENV['CALISTO_DOTENV']}"
puts "CALISTO_DOTENV_QUOTED=#{ENV['CALISTO_DOTENV_QUOTED']}"
puts "CALISTO_DOTENV_EXPORT=#{ENV['CALISTO_DOTENV_EXPORT']}"
