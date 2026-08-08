# frozen_string_literal: true

puts "json=#{defined?(JSON) ? 'yes' : 'no'} yaml=#{defined?(Psych) ? 'yes' : 'no'} " \
     "net_http=#{defined?(Net::HTTP) ? 'yes' : 'no'} csv=#{defined?(CSV) ? 'yes' : 'no'}"
