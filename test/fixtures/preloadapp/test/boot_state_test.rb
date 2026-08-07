# frozen_string_literal: true

# Fase E: `calisto test` roda no daemon quente da app. Este teste prova que o
# boot (que escreve boot_count e dorme 2s) rodou UMA vez no daemon — cada
# arquivo de teste e um fork, nao um re-boot.

require "minitest/autorun"

class BootStateTest < Minitest::Test
  def test_boot_ran_once_in_daemon
    count_file = ENV.fetch("BOOT_COUNT_FILE", File.expand_path("../config/boot_count", __dir__))
    assert File.exist?(count_file), "boot da app deveria ter rodado no daemon"
    assert_equal "1", File.read(count_file).strip, "boot nao pode re-rodar por arquivo"
  end
end
