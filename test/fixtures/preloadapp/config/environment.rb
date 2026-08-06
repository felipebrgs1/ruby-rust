# frozen_string_literal: true

# Entrypoint da app de teste da Fase B: simula um boot caro (>=2s, como um
# Rails/Spring boot) e registra quantas vezes o boot rodou de verdade — no
# daemon o boot roda UMA vez; cada `calisto run` e um fork, nao um re-boot.

boot_count_file = ENV.fetch("BOOT_COUNT_FILE", File.expand_path("boot_count", __dir__))
n = File.exist?(boot_count_file) ? File.read(boot_count_file).to_i + 1 : 1
File.write(boot_count_file, n.to_s)

sleep 2 # boot caro
