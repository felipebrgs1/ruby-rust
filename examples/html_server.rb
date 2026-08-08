# frozen_string_literal: true
# Servidor HTTP puro (stdlib only — TCPServer) servindo HTML. Base de
# comparacao `ruby` vs `calisto run` por request: num servidor de longa
# duracao o request roda no MESMO processo (latencia identica nos dois);
# a diferenca aparece no startup/1o request e no modelo processo-por-
# request (o `calisto run -e` forka de um daemon quente).
#
# Uso: ruby examples/html_server.rb  |  calisto run examples/html_server.rb
# Env: HTML_PORT (default 0 = efemera; imprime "READY <port>" e flush),
#      HTML_LOG=1 (timing por request no stderr).
require "socket"

port = Integer(ENV.fetch("HTML_PORT", "0"))
server = TCPServer.new("127.0.0.1", port)
port = server.addr[1]

body = +"<!doctype html>\n<html><head><title>calisto html</title></head>\n"
body << "<body><h1>calisto</h1><p>#{'x' * 1024}</p></body>\n</html>\n"
response = "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n" \
           "Content-Length: #{body.bytesize}\r\nConnection: close\r\n\r\n#{body}"

puts "READY #{port}"
$stdout.flush

loop do
  client = server.accept
  begin
    # consome a request (headers ate a linha vazia)
    while (line = client.gets)
      break if line == "\r\n" || line == "\n"
    end
    t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC)
    client.write(response)
    if ENV["HTML_LOG"]
      ms = (Process.clock_gettime(Process::CLOCK_MONOTONIC) - t0) * 1000
      warn format("req %.3fms", ms)
    end
  rescue IOError, Errno::EPIPE, Errno::ECONNRESET
    # cliente sumiu no meio — segue
  ensure
    begin
      client.close
    rescue IOError
    end
  end
end
