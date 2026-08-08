# frozen_string_literal: true
# Decoders por OID (type_map_for_results do AR): valores tipados com map,
# strings sem map, getvalue cru, Tuple tipado, nil desliga.
require "pg"

conn = PG.connect(ENV.fetch("CALISTO_TEST_PG"))

# sem type map: tudo string (paridade com o pg gem default)
r = conn.exec("SELECT 42::int4 AS i, 1.5::float8 AS f, true AS b, 'x'::text AS t")
raise "sem map: #{r.values.inspect}" unless r.values == [["42", "1.5", "t", "x"]]
raise "getvalue cru: #{r.getvalue(0, 0).inspect}" unless r.getvalue(0, 0) == "42"

# com type map (como o add_pg_decoders do AR): int/float/bool tipados;
# text/numeric/date seguem string (o typecast do AR cobre)
map = PG::TypeMapByOid.new
map.add_coder(PG::TextDecoder::Integer.new(oid: 21, name: "int2"))
map.add_coder(PG::TextDecoder::Integer.new(oid: 23, name: "int4"))
map.add_coder(PG::TextDecoder::Integer.new(oid: 20, name: "int8"))
map.add_coder(PG::TextDecoder::Integer.new(oid: 26, name: "oid"))
map.add_coder(PG::TextDecoder::Float.new(oid: 700, name: "float4"))
map.add_coder(PG::TextDecoder::Float.new(oid: 701, name: "float8"))
map.add_coder(PG::TextDecoder::Boolean.new(oid: 16, name: "bool"))
conn.type_map_for_results = map

r2 = conn.exec("SELECT 42::int4 AS i, -7::int2 AS s, 9007199254740993::int8 AS big, 1.5::float8 AS f, true AS b, 'x'::text AS t, 3.14::numeric AS n")
v = r2.values.first
raise "tipado: #{v.inspect}" unless v == [42, -7, 9_007_199_254_740_993, 1.5, true, "x", "3.14"]
raise "getvalue cru apos map: #{r2.getvalue(0, 0).inspect}" unless r2.getvalue(0, 0) == "42"
raise "[] tipado: #{r2[0].inspect}" unless r2[0] == [42, -7, 9_007_199_254_740_993, 1.5, true, "x", "3.14"]
row = r2.each.first
raise "tuple tipado: #{row.inspect}" unless row["i"] == 42 && row["t"] == "x" && row["n"] == "3.14"

# NULL continua nil
r3 = conn.exec_params("SELECT $1::int4 AS i, $2::text AS t", [nil, nil])
raise "null: #{r3.values.inspect}" unless r3.values == [[nil, nil]]

# nil no type_map_for_results desliga (volta a string)
conn.type_map_for_results = nil
r4 = conn.exec("SELECT 42::int4 AS i")
raise "nil desliga: #{r4.values.inspect}" unless r4.values == [["42"]]

puts "PG_DECODE_OK"
