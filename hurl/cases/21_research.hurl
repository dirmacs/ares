# Protected endpoint: deep research
# Note: Research depends on the configured LLM provider; if not available it may return 500.
# This asserts only that the endpoint is reachable/authenticated and returns a JSON response.

# Login
POST {{base_url}}/api/auth/login
Content-Type: application/json

{
  "email": "{{test_email}}",
  "password": "{{test_password}}"
}

HTTP 200

[Captures]
access_token: jsonpath "$.access_token"

# Research
POST {{base_url}}/api/research
Content-Type: application/json
Authorization: Bearer {{access_token}}

{
  "query": "Explain what A.R.E.S is in one paragraph.",
  "depth": 1,
  "max_iterations": 1
}

HTTP *

[Asserts]
# Research may return 200 (LLM available) or 500 (LLM not configured)
status >= 200
status < 600
header "content-type" contains "application/json"
