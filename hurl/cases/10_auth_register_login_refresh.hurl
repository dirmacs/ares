# Auth flow: register -> login -> refresh
# Notes:
# - Register may return 200 or 400 if user already exists (local DB persisted).
#   We handle both by attempting login afterward.

# --- Register (best-effort) ---
POST {{base_url}}/api/auth/register
Content-Type: application/json

{
  "email": "{{test_email}}",
  "password": "{{test_password}}",
  "name": "{{test_name}}"
}

HTTP *

[Asserts]
# Accept that register may return 200 (created) or 400 (already exists)
status >= 200
status < 500

# Register is best-effort; tokens are captured from the login step below.

# --- Login (authoritative) ---
POST {{base_url}}/api/auth/login
Content-Type: application/json

{
  "email": "{{test_email}}",
  "password": "{{test_password}}"
}

HTTP 200

[Captures]
access_token: jsonpath "$.access_token"
refresh_token: jsonpath "$.refresh_token"
expires_in: jsonpath "$.expires_in"

[Asserts]
jsonpath "$.access_token" exists
jsonpath "$.refresh_token" exists
jsonpath "$.expires_in" >= 1

# --- Refresh ---
POST {{base_url}}/api/auth/refresh
Content-Type: application/json

{
  "refresh_token": "{{refresh_token}}"
}

HTTP 200

[Asserts]
jsonpath "$.access_token" exists
jsonpath "$.refresh_token" exists
jsonpath "$.expires_in" >= 1
