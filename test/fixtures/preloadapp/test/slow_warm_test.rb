# frozen_string_literal: true

# Teste "caro" (0.6s): com paralelismo de arquivos + daemon quente, a suite
# inteira (2 arquivos) termina em ~0.6s; serializado levaria ~1.3s.

require "minitest/autorun"

class SlowWarmTest < Minitest::Test
  def test_slow_but_warm
    sleep 0.6
    assert true
  end
end
