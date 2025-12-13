# Negative auth tests

# Register invalid (short password)
POST {{base_url}}/api/auth/register
Content-Type: application/json

{
  "email": "baduser@example.com",
  "password": "short",
  "name": "Bad User"
}

HTTP 400

[Asserts]
jsonpath "$.error" exists

# Login invalid credentials
POST {{base_url}}/api/auth/login
Content-Type: application/json

{
  "email": "doesnotexist@example.com",
  "password": "wrongpassword"
}

HTTP 401

[Asserts]
jsonpath "$.error" exists

# Refresh missing token
POST {{base_url}}/api/auth/refresh
Content-Type: application/json

{}

HTTP 400

[Asserts]
jsonpath "$.error" exists
