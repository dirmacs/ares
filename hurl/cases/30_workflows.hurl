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

# --- Execute default workflow ---
# Note: This test requires the 'default' workflow to be configured
# and the LLM provider to be available

POST {{base_url}}/api/workflows/default
Authorization: Bearer {{access_token}}
Content-Type: application/json

{
  "query": "List available products"
}

HTTP 200

[Asserts]
# Validate workflow output structure
jsonpath "$.final_response" exists
jsonpath "$.steps_executed" >= 1
jsonpath "$.agents_used" isCollection
jsonpath "$.reasoning_path" isCollection
# Reasoning path should have at least one step
jsonpath "$.reasoning_path" count >= 1
# Each step should have required fields
jsonpath "$.reasoning_path[0].agent_name" exists
jsonpath "$.reasoning_path[0].input" exists
jsonpath "$.reasoning_path[0].output" exists
jsonpath "$.reasoning_path[0].timestamp" exists
jsonpath "$.reasoning_path[0].duration_ms" >= 0

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

HTTP 200

[Asserts]
# Validate response structure for contextual query
jsonpath "$.final_response" exists
jsonpath "$.steps_executed" >= 1
jsonpath "$.agents_used" isCollection
jsonpath "$.reasoning_path" isCollection

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
