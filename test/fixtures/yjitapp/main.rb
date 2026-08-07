# frozen_string_literal: true

puts "app_env_loaded=#{File.exist?(ENV.fetch('BOOT_COUNT_FILE', 'nope'))}"
