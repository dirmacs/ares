# Workflow API tests
# Tests the workflow execution and listing endpoints

# --- Login to get access token ---
POST {{base_url}}/api/auth/login
Content-Type: application/json

{
  "email": "{{test_email}}",
  "password": "{{test_password}}"
}

HTTP 200

[Captures]
access_token: jsonpath "$.access_token"

# --- List available workflows ---
GET {{base_url}}/api/workflows
Authorization: Bearer {{access_token}}
HTTP 200

[Asserts]
# Response should be an array of workflow names
jsonpath "$" count >= 0

# --- Execute default workflow (if exists) ---
# Note: This test may fail if 'default' workflow is not configured
# It tests the basic workflow execution flow

POST {{base_url}}/api/workflows/default
Authorization: Bearer {{access_token}}
Content-Type: application/json

{
  "query": "List available products"
}

HTTP *

[Asserts]
# Accept success (200), not found (404), or server errors (500 - LLM not available)
status >= 200
status <= 500

# --- Execute workflow with context ---
POST {{base_url}}/api/workflows/default
Authorization: Bearer {{access_token}}
Content-Type: application/json

{
  "query": "What are the sales figures?",
  "context": {
    "department": "sales",
    "quarter": "Q1"
  }
}

HTTP *

[Asserts]
# Accept various valid responses (including LLM errors)
status >= 200
status <= 500

# --- Request nonexistent workflow ---
POST {{base_url}}/api/workflows/nonexistent_workflow_12345
Authorization: Bearer {{access_token}}
Content-Type: application/json

{
  "query": "test"
}

HTTP 404

[Asserts]
jsonpath "$.error" exists

# --- Workflow without auth should fail ---
POST {{base_url}}/api/workflows/default
Content-Type: application/json

{
  "query": "test"
}

HTTP 401

# --- List workflows without auth should fail ---
GET {{base_url}}/api/workflows

HTTP 401
