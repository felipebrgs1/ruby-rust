# frozen_string_literal: true

# Warmup da Fase N: roda UMA vez no daemon, pos-boot (depois do preload,
# antes da compactacao/bind). Aquece um metodo CPU-bound — com --yjit, a
# primeira chamada compila e as seguintes reusam o codigo; o child de fork
# herda o codigo compilado (paginas CoW).

warmup_count_file = ENV.fetch("WARMUP_COUNT_FILE", File.expand_path("warmup_count", __dir__))
n = File.exist?(warmup_count_file) ? File.read(warmup_count_file).to_i + 1 : 1
File.write(warmup_count_file, n.to_s)

def burn(n)
  s = 0
  n.times { s = (s + 1) * 7 % 1_000_003 }
  s
end

200.times { burn(1000) }
