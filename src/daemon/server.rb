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
#   STOP                          -> BYE (daemon exits)
#   RUN <cwd> <env> <script> <args...>   (each field base64) -> STATUS <code>
#
# Env:
#   CALISTO_SOCKET   unix socket path
#   CALISTO_PIDFILE  pid file path
#   CALISTO_PRELOAD  comma-separated stdlib names required at boot (children inherit them)

require "socket"
require "base64"

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

File.write(PIDFILE, Process.pid) if PIDFILE

# Detach our own stdio: children get the client's real fds via SCM_RIGHTS.
# Without this, a long-lived daemon holds the spawning process's stdout pipe
# open forever (breaking `calisto run ... | head` and friends).
STDIN.reopen(File::NULL)
STDOUT.reopen(File::NULL)
STDERR.reopen(File::NULL)

trap("INT") { } # survive Ctrl-C; children reset to DEFAULT and die
trap("TERM") { shutdown }
trap("HUP") { shutdown }

def shutdown
  File.unlink(SOCKET) rescue nil
  File.unlink(PIDFILE) rescue nil if PIDFILE
  exit 0
end

# ---- request reader (first recvmsg captures SCM_RIGHTS fds) ------------------
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
      data, _addr, _flags, ancdata = @io.recvmsg(65_536, 0, scm_rights: true)
      @buf << data
      Array(ancdata).each do |a|
        next unless a.cmsg_is?(Socket::SOL_SOCKET, Socket::SCM_RIGHTS)
        fds = a.data
        fds = fds.unpack("i!*") if fds.is_a?(String)
        @fds.concat(fds)
      end
    else
      @buf << @io.readpartial(65_536)
    end
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

# ---- commands ----------------------------------------------------------------
def handle_run(io, reader, fields)
  cwd, env_blob, script, *args = fields.map { |f| Base64.strict_decode64(f) }
  env_pairs = env_blob.split("\u001e").reject(&:empty?).filter_map do |kv|
    i = kv.index("=") and [kv[0...i], kv[(i + 1)..]]
  end

  pid = Process.fork do
    # child: behave like `ruby <script> <args...>`
    trap("INT", "DEFAULT")
    trap("TERM", "DEFAULT")
    trap("HUP", "DEFAULT")
    dup_into_stdio(reader.fds)
    Dir.chdir(cwd)
    ENV.replace(env_pairs.to_h)
    $0 = script
    ARGV.replace(args)
    setup_data(script) if File.file?(script)
    begin
      load script
    rescue SystemExit
      raise # propaga: o runtime usa o status e roda at_exit hooks UMA vez
    rescue Exception => e # rubocop:disable Lint/RescueException -- mimic `ruby script`
      daemon_path = File.expand_path(__FILE__)
      bt = e.backtrace || []
      cut = bt.index { |l| l.include?(daemon_path) } || bt.size
      e.set_backtrace(bt[0...cut]) if cut < bt.size
      warn e.full_message(highlight: false, order: :top)
      exit 1
    end
  end

  reader.fds.each { |fd| IO.new(fd, autoclose: true).close rescue nil }
  status = wait_for(pid, io)
  if status
    code = status.exitstatus || (128 + (status.termsig || 0))
    respond(io, "STATUS #{code}\r\n")
  end
end

# Waits for the child; if the client disconnects first (calisto killed), kills the child.
def wait_for(pid, io)
  loop do
    done, status = Process.waitpid2(pid, Process::WNOHANG)
    return status if done
    # poll readability first -- a blocking recvmsg here would deadlock (the
    # client is waiting for STATUS and sends nothing more)
    if IO.select([io], nil, nil, 0)
      begin
        data, = io.recvmsg(1, Socket::MSG_PEEK)
        dead = data.nil? || data.empty? # EOF: recvmsg returns nil, not EOFError
      rescue EOFError
        dead = true
      end
      if dead
        Process.kill("TERM", pid) rescue nil
        sleep 0.2
        Process.kill("KILL", pid) rescue nil
        Process.wait2(pid) rescue nil
        return nil
      end
    end
    sleep 0.01
  end
end

# ---- main loop ---------------------------------------------------------------
loop do
  io = server.accept
  begin
    reader = RequestReader.new(io)
    op, fields = read_command(reader)
    case op
    when "PING" then respond(io, "OK\r\n")
    when "STOP" then respond(io, "BYE\r\n") && shutdown
    when "RUN"  then handle_run(io, reader, fields)
    else respond(io, "ERR unknown command: #{op.inspect}\r\n")
    end
  rescue StandardError => e
    respond(io, "ERR #{e.class}: #{e.message}\r\n")
  ensure
    io.close rescue nil
  end
end
