# frozen_string_literal: true

# Entrypoint da app de teste da Fase N: registra quantas vezes o boot rodou
# de verdade (UMA vez no daemon; cada `calisto run` e um fork, nao um re-boot).

boot_count_file = ENV.fetch("BOOT_COUNT_FILE", File.expand_path("boot_count", __dir__))
n = File.exist?(boot_count_file) ? File.read(boot_count_file).to_i + 1 : 1
File.write(boot_count_file, n.to_s)
