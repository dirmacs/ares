# Negative tests for protected routes (must require Bearer token)

# Chat without Authorization
POST {{base_url}}/api/chat
Content-Type: application/json

{
  "message": "Hello"
}

HTTP 401

[Asserts]
# Don't assume JSON error format
body != ""

# Memory without Authorization
GET {{base_url}}/api/memory

HTTP 401

[Asserts]
body != ""

# Research without Authorization
POST {{base_url}}/api/research
Content-Type: application/json

{
  "query": "test"
}

HTTP 401

[Asserts]
body != ""
