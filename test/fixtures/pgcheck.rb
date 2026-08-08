# frozen_string_literal: true
# Verificacao da camada Calisto::PG (test/pgnative.rs, gated em
# CALISTO_TEST_PG — precisa de um postgres vivo). Roda no daemon quente com
# o shim gems/pg.rb; os asserts cobrem o surface que o ActiveRecord chama.
require "pg"

conn = PG.connect(ENV.fetch("CALISTO_TEST_PG"))
conn.exec("DROP TABLE IF EXISTS calisto_pg_check")
conn.exec("CREATE TABLE calisto_pg_check (id serial PRIMARY KEY, name text, n int, note text)")

r = conn.exec_params("INSERT INTO calisto_pg_check (name, n, note) VALUES ($1, $2, $3) RETURNING id",
                     ["z\u00e9", 7, nil])
raise "cmd_tuples=#{r.cmd_tuples.inspect}" unless r.cmd_tuples == 1
raise "ntuples=#{r.ntuples}" unless r.ntuples == 1

conn.prepare("calisto_pg_check_sel", "SELECT name, n, note FROM calisto_pg_check WHERE n = $1")
r2 = conn.exec_prepared("calisto_pg_check_sel", [7])
raise "fields=#{r2.fields.inspect}" unless r2.fields == %w[name n note]
raise "values=#{r2.values.inspect}" unless r2.values == [["z\u00e9", "7", nil]]

rows = conn.exec("SELECT * FROM calisto_pg_check").each.to_a
raise "tuple name=#{rows.first.inspect}" unless rows.first["name"] == "z\u00e9"
raise "tuple nil" unless rows.first["note"].nil?

begin
  conn.exec("SELECT * FROM calisto_pg_no_such_table")
  raise "erro: PG::Error nao levantou"
rescue PG::Error => e
  raise "erro errado: #{e.class}: #{e.message}" unless e.message.include?("does not exist")
end

raise "tx=#{conn.transaction_status}" unless conn.transaction_status == PG::PQTRANS_IDLE
conn.exec("BEGIN")
raise "tx in=#{conn.transaction_status}" unless conn.transaction_status == PG::PQTRANS_INTRANS
conn.exec("ROLLBACK")

raise "escape=#{conn.escape("o'x")}" unless conn.escape("o'x") == "o''x"
raise "escape_literal" unless conn.escape_literal("o'x") == "'o''x'"
raise "escape_identifier" unless conn.escape_identifier("select") == '"select"'
raise "server_version=#{conn.server_version}" unless conn.server_version > 150_000
raise "status=#{conn.status}" unless conn.status == PG::CONNECTION_OK

puts "PG_CHECK_OK #{r2.values.inspect} fields=#{r2.fields.inspect}"
