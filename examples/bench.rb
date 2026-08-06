# frozen_string_literal: true

# Workload ~ "app boot with stdlib": parses/generates JSON, YAML, ERB.
# Cold: ruby pays require + parse per run. Fast: daemon preloaded everything.

require "json"
require "yaml"
require "erb"

payload = { name: "calisto", year: 2026, tags: %w[ruby fast fork] }
5_000.times { JSON.generate(payload) }
doc = YAML.safe_load("name: calisto\nfast: true\n")
erb = ERB.new("<%= name %>-<%= fast %>").result_with_hash(doc.transform_keys(&:to_sym))
abort "sanity check failed" unless erb == "calisto-true"

puts "bench ok (ruby #{RUBY_VERSION}, erb=#{erb})"
