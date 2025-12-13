# Shared variables and defaults for the Hurl suite.
# Override any of these with environment variables when running hurl.

# Base URL of the running server
{{$base_url := getEnv("ARES_BASE_URL", "http://127.0.0.1:3000")}}

# Static API key (used only if you enable API-key auth somewhere; current routes use Bearer JWT)
{{$api_key := getEnv("ARES_API_KEY", "")}}

# Deterministic test user (change if you want)
{{$test_email := getEnv("ARES_TEST_EMAIL", "hurl.user1@example.com")}}
{{$test_password := getEnv("ARES_TEST_PASSWORD", "correcthorsebatterystaple")}}
{{$test_name := getEnv("ARES_TEST_NAME", "Hurl User")}}

# Second user for conflict tests
{{$test_email2 := getEnv("ARES_TEST_EMAIL2", "hurl.user2@example.com")}}
{{$test_password2 := getEnv("ARES_TEST_PASSWORD2", "correcthorsebatterystaple")}}
{{$test_name2 := getEnv("ARES_TEST_NAME2", "Hurl User 2")}}
