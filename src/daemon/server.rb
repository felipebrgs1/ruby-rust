#!/usr/bin/env ruby
# frozen_string_literal: true

# calisto daemon -- a warmed Ruby VM that forks one child per run request.
#
# Wire protocol (RESP-style bulk strings over a unix socket):
#   client -> daemon: "<OP> <n>\r\n" then n fields, each "$<len>\r\n<data>\r\n"
#   daemon -> client: "OK\r\n" | "ERR <msg>\r\n" | "STATUS <code>\r\n" | "BYE\r\n"
#
# The RUN request carries the client's stdin/stdout/stderr fds via SCM_RIGHTS
# ancillary data on the first packet; the daemon dup2()s them into the child,
# so stdio semantics match `ruby <script>` exactly (streaming + interactive).
#
# Commands:
#   PING                          -> OK
#   STOP                          -> BYE (daemon exits; children rodando sao mortos)
#   RUN <cwd> <env> <script> <args...>   (each field base64) -> STATUS <code>
#   EVAL <cwd> <env> <code> <args...>    (like RUN, but evals code as `ruby -e`)
#
# Loop multi-conexao (Fase E): select sobre o listener + conexoes ativas e
# waitpid WNOHANG a cada tick. Um child de longa duracao (server, sidekiq,
# suite lenta) NAO bloqueia novos RUNs — cada conexao e atendida no seu tempo,
# cada child e reapado ao terminar, e cliente morto com child rodando leva o
# mesmo TERM -> KILL do wait_for antigo, agora por conexao.
#
# Env:
#   CALISTO_SOCKET   unix socket path
#   CALISTO_PIDFILE  pid file path
#   CALISTO_PRELOAD  comma-separated stdlib names required at boot (children inherit them)

require "socket"

# SEM `require "base64"` aqui: ativar a default gem antes do Bundler.setup do
# child/preload dispararia o "already activated" classico quando o bundle da
# app pinar a gem numa versao diferente da empacotada no ruby em uso (ex.:
# base64 0.3.0 do 3.4.10 vs 0.2.0 do 3.4.4 — Fase I). O decoder e
# hand-rolled, mesmo alfabeto/encoding do encoder do cliente Rust.
B64_TABLE = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".bytes
B64_INDEX = Array.new(256, -1)
B64_TABLE.each_with_index { |b, i| B64_INDEX[b] = i }

def b64_decode(s)
  bytes = s.bytes
  out = String.new(encoding: Encoding::BINARY)
  i = 0
  len = bytes.length
  while i + 1 < len
    c0 = B64_INDEX[bytes[i]]
    c1 = B64_INDEX[bytes[i + 1]]
    break if c0 < 0 || c1 < 0
    out << ((c0 << 2) | (c1 >> 4))
    c2 = bytes[i + 2] == 61 ? -1 : B64_INDEX[bytes[i + 2]] # 61 == "="
    if c2 >= 0
      out << (((c1 & 0x0f) << 4) | (c2 >> 2))
      c3 = bytes[i + 3] == 61 ? -1 : B64_INDEX[bytes[i + 3]]
      out << (((c2 & 0x03) << 6) | c3) if c3 >= 0
    end
    i += 4
  end
  out
end

SOCKET = ENV.fetch("CALISTO_SOCKET")
PIDFILE = ENV["CALISTO_PIDFILE"]

# ---- boot: preload -----------------------------------------------------------
preload = ENV.fetch("CALISTO_PRELOAD", "").split(",").map(&:strip).reject(&:empty?)
preload.each do |name|
  begin
    require name
  rescue LoadError, StandardError => e
    warn "calisto: preload '#{name}' failed: #{e.class}: #{e.message}"
  end
end

# ---- boot: app preload (Fase B) ----------------------------------------------
# Com calisto.toml o daemon e o daemon DA APP: o entrypoint (ex.:
# config/environment.rb) e carregado aqui no boot, com o Gemfile da app ativo
# (o cliente spawna com `-rbundler/setup` e cwd na raiz da app). Cada RUN e um
# fork deste boot congelado — por isso e que um comando Rails cai de ~2s para
# centenas de ms. Conexoes de DB abertas pelo boot sao desconectadas em
# seguida (padrao Zeus/Spring): o fork nao herda sockets do banco, e o child
# reconecta lazy no primeiro uso.
APP_PRELOAD = ENV["CALISTO_APP_PRELOAD"]
if APP_PRELOAD
  begin
    load APP_PRELOAD
  rescue SystemExit
    raise
  rescue Exception => e # rubocop:disable Lint/RescueException -- boot da app
    warn "calisto: app preload falhou (#{APP_PRELOAD}): #{e.class}: #{e.message}"
    warn(e.backtrace.first(8).join("\n")) if e.backtrace
    exit 1
  end
  if defined?(ActiveRecord::Base) && ActiveRecord::Base.respond_to?(:connection_handler)
    ActiveRecord::Base.connection_handler.clear_all_connections!
  end
  # `load` nao registra em $LOADED_FEATURES. No child, o Rails re-roda o boot
  # via require_environment! (`require config/environment`) — sem este registro
  # ele executaria environment.rb de novo e o initialize! duplo aborta com
  # "Application has been already initialized".
  $LOADED_FEATURES << File.expand_path(APP_PRELOAD)
end

# ---- boot: compactacao pre-fork (Fase M) --------------------------------------
# GC.start + GC.compact apos o boot: heap denso -> os children (fork do
# daemon) nascem com quase todas as paginas compartilhadas via CoW. Espelho
# do daemon embutido (main.rs): best-effort — falha avisa e segue.
if ENV["CALISTO_COMPACT"] == "1"
  begin
    GC.start
    GC.compact
  rescue StandardError, NotImplementedError => e
    warn "calisto: compact falhou: #{e.class}: #{e.message}"
  end
end

# ---- boot: bind --------------------------------------------------------------
def live?
  UNIXSocket.new(SOCKET).close
  true
rescue SystemCallError
  false
end

server =
  begin
    UNIXServer.new(SOCKET)
  rescue Errno::EADDRINUSE
    exit 0 if live? # another daemon already owns the socket
    File.unlink(SOCKET) # stale socket from a dead daemon
    UNIXServer.new(SOCKET)
  end
$server = server

File.write(PIDFILE, Process.pid) if PIDFILE

# Detach our own stdio: children get the client's real fds via SCM_RIGHTS.
# Without this, a long-lived daemon holds the spawning process's stdout pipe
# open forever (breaking `calisto run ... | head` and friends).
STDIN.reopen(File::NULL)
STDOUT.reopen(File::NULL)
STDERR.reopen(File::NULL)

# Registro global de conexoes/filhos: traps de shutdown e o handler de STOP
# precisam derrubar children rodando antes de sair (nada de orfaos).
$clients = {}  # io.object_id => Client
$children = {} # pid => Client

def kill_child(pid)
  Process.kill("TERM", pid) rescue nil
  sleep 0.2
  Process.kill("KILL", pid) rescue nil
  Process.wait2(pid) rescue nil
end

def kill_all_children
  # shutdown/STOP: derruba children e devolve STATUS aos clientes (um
  # `calisto serve`/`run` morto pelo stop nao pode ficar sem resposta)
  $children.each_value do |c|
    _, status = kill_child(c.pid)
    next unless status
    code = status.exitstatus || (128 + (status.termsig || 0))
    respond(c.io, "STATUS #{code}\r\n")
  end
end

trap("INT") { } # survive Ctrl-C; children reset to DEFAULT and die
trap("TERM") { shutdown }
trap("HUP") { shutdown }

def shutdown
  kill_all_children
  File.unlink(SOCKET) rescue nil
  File.unlink(PIDFILE) rescue nil if PIDFILE
  exit 0
end

# ---- request reader (first recvmsg captures SCM_RIGHTS fds) ------------------
# Leitura nao-bloqueante: um comando parcial nao trava o loop multi-conexao —
# `fill` levanta PartialRead e o cliente volta a ser atendido no proximo
# select (o buffer parcial fica no reader).
class PartialRead < StandardError; end

class RequestReader
  def initialize(io)
    @io = io
    @buf = +""
    @fds = []
    @first = true
  end

  attr_reader :fds

  def fill
    if @first
      @first = false
      data, _addr, _flags, ancdata = @io.recvmsg_nonblock(65_536, 0, scm_rights: true)
      @buf << data
      Array(ancdata).each do |a|
        next unless a.cmsg_is?(Socket::SOL_SOCKET, Socket::SCM_RIGHTS)
        fds = a.data
        fds = fds.unpack("i!*") if fds.is_a?(String)
        @fds.concat(fds)
      end
    else
      @buf << @io.read_nonblock(65_536)
    end
  rescue IO::WaitReadable, Errno::EAGAIN
    raise PartialRead
  rescue EOFError
    raise "eof"
  end

  def read_until_crlf
    fill until (i = @buf.index("\r\n"))
    @buf.slice!(0, i + 2).chomp("\r\n")
  end

  def read_exact(n)
    fill while @buf.bytesize < n
    @buf.slice!(0, n)
  end
end

def read_command(reader)
  head = reader.read_until_crlf
  op, count = head.split(" ", 2)
  raise "bad command: #{head.inspect}" unless op && count
  fields = Integer(count).times.map do
    len = Integer(reader.read_until_crlf.delete_prefix("$"))
    reader.read_exact(len)
  end
  [op, fields]
end

def respond(io, msg)
  io.write(msg)
  io.flush
rescue Errno::EPIPE, Errno::ECONNRESET, IOError
end

def finish_client(io)
  $clients.delete(io.object_id)
  io.close rescue nil
end

# ---- child stdio --------------------------------------------------------------
# `load` (diferente do main script do CLI) nao define DATA. Emula o CLI:
# acha o marcador __END__ com o mesmo lexer do ruby (Ripper) e abre o IO
# apontando para o conteudo apos a linha do marcador.
def setup_data(script)
  src = File.binread(script)
  return unless src.include?("__END__")
  require "ripper"
  line = nil
  Ripper.lex(src).each { |(l, _c), event, _tok, _st| line = l if event == :on___end__ }
  return unless line
  pos = 0
  line.times do
    nl = src.index("\n", pos)
    pos = nl ? nl + 1 : src.bytesize
  end
  io = File.open(script, "rb")
  io.seek(pos)
  Object.const_set(:DATA, io)
end

def dup_into_stdio(fds)
  stdin_fd, stdout_fd, stderr_fd = fds
  STDIN.reopen(IO.new(stdin_fd, autoclose: false)) if stdin_fd
  if stdout_fd
    STDOUT.reopen(IO.new(stdout_fd, autoclose: false))
    $stdout.sync = true if STDOUT.tty?
  end
  if stderr_fd
    STDERR.reopen(IO.new(stderr_fd, autoclose: false))
    $stderr.sync = true if STDERR.tty?
  end
end

# ---- child bootstrap ----------------------------------------------------------
# Corpo comum do child (RUN e EVAL): traps default, stdio do cliente, hygiene
# de fds (nao segura o socket de controle nem o listener), cwd, env do RUN e
# ativacao do Gemfile do cwd (walk up, como `bundle exec`): fora de bundle e
# no-op, entao rodar sem Gemfile continua identico a `ruby <script>`.
# Nao da para usar RUBYOPT=-rbundler/setup aqui: RUBYOPT so e lido no boot do
# interpretador, e um child de fork nao re-boota.
def child_enter(io, reader, cwd, env_blob)
  trap("INT", "DEFAULT")
  trap("TERM", "DEFAULT")
  trap("HUP", "DEFAULT")
  dup_into_stdio(reader.fds)
  io.close rescue nil
  $server.close rescue nil
  Dir.chdir(cwd)
  env_pairs = env_blob.split("\u001e").reject(&:empty?).filter_map do |kv|
    i = kv.index("=") and [kv[0...i], kv[(i + 1)..]]
  end
  ENV.replace(env_pairs.to_h)
  require "bundler/setup"
  # -I do child: usado por `calisto test` para `require "test_helper"` /
  # `require "rails_helper"` funcionar sem o runner do framework. Depois
  # do Bundler.setup: ele limpa o $LOAD_PATH, entao o unshift e pos-setup.
  if (lp = ENV["CALISTO_LOAD_PATH"])
    lp.split(":").reject(&:empty?).each do |p|
      $LOAD_PATH.unshift(p) unless $LOAD_PATH.include?(p)
    end
  end
end

def report_child_error(e)
  daemon_path = File.expand_path(__FILE__)
  bt = e.backtrace || []
  cut = bt.index { |l| l.include?(daemon_path) } || bt.size
  e.set_backtrace(bt[0...cut]) if cut < bt.size
  warn e.full_message(highlight: false, order: :top)
end

# ---- commands ----------------------------------------------------------------
class Client
  attr_reader :io, :reader
  attr_accessor :pid

  def initialize(io)
    @io = io
    @reader = RequestReader.new(io)
    @pid = nil
  end

  def waiting?
    !@pid.nil?
  end
end

def close_client_fds(reader)
  reader.fds.each { |fd| IO.new(fd, autoclose: true).close rescue nil }
end

def start_child(io, reader, fields)
  cwd, env_blob, script, *args = fields.map { |f| b64_decode(f) }

  pid = Process.fork do
    # child: behave like `ruby <script> <args...>`
    child_enter(io, reader, cwd, env_blob)
    $0 = script
    ARGV.replace(args)
    setup_data(script) if File.file?(script)
    begin
      load script
    rescue SystemExit
      raise # propaga: o runtime usa o status e roda at_exit hooks UMA vez
    rescue Exception => e # rubocop:disable Lint/RescueException -- mimic `ruby script`
      report_child_error(e)
      exit 1
    end
  end

  close_client_fds(reader)
  pid
end

# EVAL: como RUN, mas o "script" e codigo inline — paridade com `ruby -e`:
# $0 = "-e", ARGV = args, eval no TOPLEVEL_BINDING com nome de arquivo "-e"
# (backtraces idem `ruby -e`), sem DATA. Multiplos -e chegam concatenados
# com "\n" (o cliente junta), entao __LINE__ segue a concatenacao.
def start_child_eval(io, reader, fields)
  cwd, env_blob, code, *args = fields.map { |f| b64_decode(f) }

  pid = Process.fork do
    # child: behave like `ruby -e '<code>' <args...>`
    child_enter(io, reader, cwd, env_blob)
    $0 = "-e"
    ARGV.replace(args)
    begin
      eval(code, TOPLEVEL_BINDING, "-e", 1)
    rescue SystemExit
      raise # propaga: o runtime usa o status e roda at_exit hooks UMA vez
    rescue Exception => e # rubocop:disable Lint/RescueException -- mimic `ruby -e`
      report_child_error(e)
      exit 1
    end
  end

  close_client_fds(reader)
  pid
end

# ---- main loop ---------------------------------------------------------------
loop do
  begin
    ready = IO.select([$server] + $clients.values.map(&:io), nil, nil, 0.01) || [[], [], []]
  rescue Errno::EBADF
    # fd invalido no set (numero de fd reaproveitado por outro io — o objeto
    # Ruby nao sabe que o fd foi fechado por baixo). O cliente afetado e
    # irrecuperavel: derruba clientes + children e segue — melhor perder
    # conexoes ativas do que o daemon inteiro (o cliente stop re-tenta).
    $children.each_value do |c|
      _, status = kill_child(c.pid)
      next unless status
      code = status.exitstatus || (128 + (status.termsig || 0))
      respond(c.io, "STATUS #{code}\r\n") rescue nil
    end
    $children.clear
    $clients.each_value { |c| c.io.close rescue nil }
    $clients.clear
    retry
  end
  readables = ready[0] || []

  if readables.include?($server)
    begin
      io = $server.accept_nonblock
      $clients[io.object_id] = Client.new(io)
    rescue IO::WaitReadable, Errno::EINTR
    end
  end

  readables.each do |io|
    next if io == $server
    client = $clients[io.object_id]
    next unless client

    if client.waiting?
      # child rodando: readable aqui so pode ser EOF (cliente morto) ou dados
      # espurios — o cliente espera STATUS e nao envia mais nada.
      begin
        data, = io.recvmsg_nonblock(1, Socket::MSG_PEEK)
        dead = data.nil? || data.empty?
      rescue EOFError
        dead = true
      rescue IO::WaitReadable, Errno::EAGAIN
        dead = false
      end
      if dead
        pid = client.pid
        $children.delete(pid)
        kill_child(pid)
        finish_client(io)
      end
      next
    end

    begin
      op, fields = read_command(client.reader)
      case op
      when "PING"
        respond(io, "OK\r\n")
      when "STOP"
        respond(io, "BYE\r\n")
        shutdown # derruba children antes de sair (sem orfaos)
      when "RUN"
        pid = start_child(io, client.reader, fields)
        client.pid = pid
        $children[pid] = client
      when "EVAL"
        pid = start_child_eval(io, client.reader, fields)
        client.pid = pid
        $children[pid] = client
      else
        respond(io, "ERR unknown command: #{op.inspect}\r\n")
        finish_client(io)
      end
    rescue PartialRead
      # comando incompleto: aguarda mais dados (select re-marca readable)
    rescue StandardError => e
      respond(io, "ERR #{e.class}: #{e.message}\r\n")
      finish_client(io)
    end
  end

  # children terminados -> STATUS para o cliente (se ainda vivo)
  $children.keys.each do |pid|
    done, status = Process.waitpid2(pid, Process::WNOHANG)
    next unless done
    client = $children.delete(pid)
    next unless client
    code = status.exitstatus || (128 + (status.termsig || 0))
    respond(client.io, "STATUS #{code}\r\n")
    finish_client(client.io)
  end
end
