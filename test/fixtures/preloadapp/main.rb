# frozen_string_literal: true

count_file = ENV.fetch("BOOT_COUNT_FILE", File.expand_path("boot_count", __dir__))
puts "running app script"
puts "app_env_loaded=#{File.exist?(count_file)}"
