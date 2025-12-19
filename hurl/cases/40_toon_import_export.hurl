# TOON Import/Export API Tests
# 
# Tests the TOON-specific features:
# - POST /api/user/agents/import (import from TOON format)
# - GET /api/user/agents/{name}/export (export to TOON format)
# - GET /api/user/agents/{name}?format=toon (query param format)
# - toon_preview field in create response
#
# Prerequisites: Authenticated user (run 10_auth_register_login_refresh.hurl first)

# --- Login to get tokens ---
POST {{base_url}}/api/auth/login
Content-Type: application/json
{
  "email": "{{test_email}}",
  "password": "{{test_password}}"
}
HTTP 200
[Captures]
access_token: jsonpath "$.access_token"

# --- Test 1: Create agent and verify toon_preview is returned ---
POST {{base_url}}/api/user/agents
Content-Type: application/json
Authorization: Bearer {{access_token}}
{
  "name": "toon-test-agent",
  "model": "balanced",
  "system_prompt": "You are a test agent for TOON format.",
  "tools": [],
  "max_tool_iterations": 5,
  "parallel_tools": false,
  "is_public": false
}
HTTP 201
[Captures]
created_agent_id: jsonpath "$.id"
toon_preview: jsonpath "$.toon_preview"

[Asserts]
jsonpath "$.name" == "toon-test-agent"
jsonpath "$.toon_preview" exists
jsonpath "$.toon_preview" isString
# TOON format should contain the agent name
jsonpath "$.toon_preview" contains "toon-test-agent"

# --- Test 2: Export agent as TOON format (via /export endpoint) ---
GET {{base_url}}/api/user/agents/toon-test-agent/export
Authorization: Bearer {{access_token}}
HTTP 200

[Asserts]
header "Content-Type" contains "text/x-toon"
body contains "name: toon-test-agent"
body contains "model: balanced"
body contains "system_prompt:"

# --- Test 3: Get agent with ?format=toon query param ---
GET {{base_url}}/api/user/agents/toon-test-agent?format=toon
Authorization: Bearer {{access_token}}
HTTP 200

[Asserts]
header "Content-Type" contains "text/x-toon"
body contains "name: toon-test-agent"

# --- Test 4: Import agent from TOON format ---
POST {{base_url}}/api/user/agents/import
Content-Type: text/x-toon
Authorization: Bearer {{access_token}}
```
name: toon-imported-agent
model: balanced
max_tool_iterations: 3
parallel_tools: true
tools[0]:
system_prompt: This agent was imported from TOON format.
```
HTTP 201

[Asserts]
jsonpath "$.name" == "toon-imported-agent"
jsonpath "$.toon_preview" exists

# --- Test 5: Verify imported agent exists and has correct config ---
GET {{base_url}}/api/user/agents/toon-imported-agent
Authorization: Bearer {{access_token}}
HTTP 200

[Asserts]
jsonpath "$.name" == "toon-imported-agent"
jsonpath "$.model" == "balanced"
jsonpath "$.max_tool_iterations" == 3
jsonpath "$.parallel_tools" == true

# --- Test 6: Import with invalid TOON format ---
POST {{base_url}}/api/user/agents/import
Content-Type: text/x-toon
Authorization: Bearer {{access_token}}
```
this is not valid toon format
missing required fields
```
HTTP 400

[Asserts]
jsonpath "$.error" exists

# --- Test 7: Export non-existent agent ---
GET {{base_url}}/api/user/agents/nonexistent-toon-agent-xyz/export
Authorization: Bearer {{access_token}}
HTTP 404

# --- Cleanup: Delete test agents ---
DELETE {{base_url}}/api/user/agents/toon-test-agent
Authorization: Bearer {{access_token}}
HTTP 204

DELETE {{base_url}}/api/user/agents/toon-imported-agent
Authorization: Bearer {{access_token}}
HTTP 204

