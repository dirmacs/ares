import requests
import json

BASE_URL = "http://localhost:3000"

# Register a user
def register_user():
    response = requests.post(
        f"{BASE_URL}/api/auth/register",
        json={
            "email": "agent@test.com",
            "password": "test12345678",
            "name": "Agent Tester"
        }
    )
    print("✓ User registered")
    return response.json()

# Login and get token
def login():
    response = requests.post(
        f"{BASE_URL}/api/auth/login",
        json={
            "email": "agent@test.com",
            "password": "test12345678"
        }
    )
    data = response.json()
    token = data.get("access_token") or data.get("token")
    print("✓ Logged in, got token")
    return token

# Chat with an agent
def chat_with_agent(token, agent_type, message):
    headers = {"Authorization": f"Bearer {token}"}
    response = requests.post(
        f"{BASE_URL}/api/chat",
        headers=headers,
        json={
            "message": message,
            "agent_type": agent_type
        }
    )
    return response.json()

# Run deep research
def deep_research(token, query):
    headers = {"Authorization": f"Bearer {token}"}
    response = requests.post(
        f"{BASE_URL}/api/research",
        headers=headers,
        json={
            "query": query,
            "depth": 2,
            "max_iterations": 3
        }
    )
    return response.json()

# Main test flow
def main():
    print("\n🚀 Testing A.R.E.S Agentic Framework\n")
    
    # Step 1: Register and login
    try:
        register_user()
    except:
        print("User already exists, continuing...")
    
    token = login()
    
    # Step 2: Test different agents
    print("\n📋 Testing Product Agent...")
    result = chat_with_agent(token, "product", "What products do we have?")
    print(f"Response: {result.get('response', 'No response')[:100]}...")
    
    print("\n💰 Testing Finance Agent...")
    result = chat_with_agent(token, "finance", "Calculate quarterly revenue projections")
    print(f"Response: {result.get('response', 'No response')[:100]}...")
    
    print("\n👥 Testing HR Agent...")
    result = chat_with_agent(token, "hr", "What's our hiring process?")
    print(f"Response: {result.get('response', 'No response')[:100]}...")
    
    # Step 3: Test deep research
    print("\n🔍 Testing Deep Research...")
    result = deep_research(token, "Analyze AI market trends")
    print(f"Research: {result.get('summary', 'No summary')[:100]}...")
    
    print("\n✅ All tests completed!\n")

if __name__ == "__main__":
    main()
