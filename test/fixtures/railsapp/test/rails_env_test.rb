# frozen_string_literal: true

# Golden do marco Fase E: `calisto test` deve rodar no daemon de teste da app
# (RAILS_ENV=test no boot, socket proprio) — se o daemon dev fosse usado, o
# Rails.env ficaria "development" e este teste falharia.

require "minitest/autorun"
require_relative "../config/environment"

class RailsEnvTest < Minitest::Test
  def test_runs_in_test_environment
    assert_equal "test", Rails.env, "calisto test deve usar o daemon RAILS_ENV=test"
  end
end
