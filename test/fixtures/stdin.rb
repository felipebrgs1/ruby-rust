# frozen_string_literal: true

line = STDIN.gets&.chomp
warn "stderr: got=#{line}"
puts "stdout: got=#{line}"
