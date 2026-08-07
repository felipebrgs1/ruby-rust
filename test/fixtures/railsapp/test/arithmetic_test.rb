# frozen_string_literal: true

require "minitest/autorun"

class ArithmeticTest < Minitest::Test
  def test_addition
    assert_equal 4, 2 + 2
  end
end
