# frozen_string_literal: true

require_relative "lib/foo"
require "json"

puts "msg: #{Foo.msg}"
puts "file: #{__FILE__}"
puts "dir: #{__dir__}"
puts "json: #{JSON.generate(a: 1)}"
