# List available agents (public)

GET {{base_url}}/api/agents
HTTP 200

[Asserts]
jsonpath "$[0].agent_type" exists
jsonpath "$[0].name" exists
jsonpath "$[0].description" exists
