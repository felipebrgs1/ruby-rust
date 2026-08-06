# frozen_string_literal: true

def boom
  raise ArgumentError, "exploded"
end

boom
