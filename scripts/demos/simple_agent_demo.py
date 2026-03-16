"""
Simple Agentic Framework Demo
Uses Ollama directly to demonstrate multi-agent workflow
"""

import requests
import json

OLLAMA_URL = "http://localhost:11434"

class Agent:
    """Base agent class"""
    def __init__(self, name, role, model="qwen3:1.7b"):
        self.name = name
        self.role = role
        self.model = model
    
    def think(self, task):
        """Agent thinks and generates response"""
        prompt = f"""You are {self.name}, a {self.role}.
Your task: {task}

Provide a concise, professional response."""
        
        response = requests.post(
            f"{OLLAMA_URL}/api/generate",
            json={
                "model": self.model,
                "prompt": prompt,
                "stream": False
            }
        )
        return response.json()["response"]

class Orchestrator:
    """Orchestrates multiple agents"""
    def __init__(self):
        self.agents = {}
    
    def register_agent(self, agent_type, agent):
        """Register a new agent"""
        self.agents[agent_type] = agent
        print(f"✓ Registered {agent.name}")
    
    def delegate(self, agent_type, task):
        """Delegate task to specific agent"""
        if agent_type not in self.agents:
            return f"Error: No agent of type '{agent_type}' found"
        
        agent = self.agents[agent_type]
        print(f"\n🤖 {agent.name} is working...")
        response = agent.think(task)
        return response
    
    def collaborate(self, task, agent_types):
        """Multiple agents collaborate on a task"""
        print(f"\n🔄 Collaborative task: {task}")
        results = {}
        for agent_type in agent_types:
            result = self.delegate(agent_type, task)
            results[agent_type] = result
        return results

def main():
    print("\n🚀 Simple Agentic Framework Demo\n")
    
    # Create orchestrator
    orchestrator = Orchestrator()
    
    # Register agents
    orchestrator.register_agent("product", Agent("ProductBot", "product specialist"))
    orchestrator.register_agent("finance", Agent("FinanceBot", "financial analyst"))
    orchestrator.register_agent("hr", Agent("HRBot", "human resources manager"))
    
    # Test 1: Single agent task
    print("\n" + "="*50)
    print("TEST 1: Single Agent Task")
    print("="*50)
    result = orchestrator.delegate("product", "List 3 key features of a modern SaaS product")
    print(f"\n💡 Response:\n{result[:200]}...")
    
    # Test 2: Collaborative task
    print("\n" + "="*50)
    print("TEST 2: Multi-Agent Collaboration")
    print("="*50)
    results = orchestrator.collaborate(
        "How to launch a new product?",
        ["product", "finance", "hr"]
    )
    
    for agent_type, response in results.items():
        print(f"\n📋 {agent_type.upper()}:")
        print(response[:150] + "...")
    
    print("\n✅ Demo complete!\n")

if __name__ == "__main__":
    main()
