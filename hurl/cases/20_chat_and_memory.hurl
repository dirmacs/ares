# Protected endpoints: chat + memory
# This file logs in to obtain a Bearer token.

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

# Chat (creates a new conversation)
# NOTE: may return 500 if Ollama/LLM is unavailable — we just assert a JSON response for graceful degradation.
POST {{base_url}}/api/chat
Content-Type: application/json
Authorization: Bearer {{access_token}}

{
  "message": "Hello! Reply with a short greeting.",
  "agent_type": "product"
}

HTTP *

[Asserts]
# Success path or graceful 500 error
status >= 200
status < 600
header "content-type" contains "application/json"

# Memory endpoint (does not require LLM)
GET {{base_url}}/api/memory
Authorization: Bearer {{access_token}}

HTTP 200

[Asserts]
jsonpath "$.user_id" exists
jsonpath "$.preferences" exists
jsonpath "$.facts" exists
