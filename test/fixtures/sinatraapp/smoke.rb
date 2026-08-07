# frozen_string_literal: true

# Smoke do marco Fase F: `calisto build --compile` embute as gems pure-Ruby
# (sinatra, rack, rack-test, ...) e este script roda SEM bundle install no
# sistema (o teste roda o bundle com GEM_HOME/GEM_PATH vazios).

require "sinatra/base"
require "rack/test"
require_relative "app"

include Rack::Test::Methods

def app
  CalistoApp
end

get "/"
status = last_response.status
body = last_response.body
puts "HTTP #{status}: #{body}"
exit(status == 200 && body == "hello from sinatra" ? 0 : 1)
