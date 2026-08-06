# frozen_string_literal: true

puts "hello from calisto on Ruby #{RUBY_VERSION}"
puts "json: #{defined?(JSON) ? 'yes (preloaded)' : 'no'}"
puts "yaml: #{defined?(Psych) ? 'yes (preloaded)' : 'no'}"
puts "args: #{ARGV.inspect}"
