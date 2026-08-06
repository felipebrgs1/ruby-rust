# frozen_string_literal: true

$LOAD_PATH.unshift(__dir__)
require "lib/foo"
puts "msg: #{Foo.msg}"
