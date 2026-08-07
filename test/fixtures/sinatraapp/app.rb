# frozen_string_literal: true

# App Sinatra minimo para o golden test HTTP (boot via calisto run).
# A porta vem do PORT env para o teste nao depender de porta fixa.
# O run! e condicional: o smoke do build --compile (Fase F) require este
# arquivo para testar a classe via rack-test sem subir servidor.

require "sinatra/base"

class CalistoApp < Sinatra::Base
  set :bind, "127.0.0.1"
  set :port, Integer(ENV.fetch("PORT", "4567"))
  set :environment, :production

  get("/") { "hello from sinatra" }
end

CalistoApp.run! if $PROGRAM_NAME == __FILE__
