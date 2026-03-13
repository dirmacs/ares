"""
Advanced Agentic Framework with Tools
Demonstrates agents using tools and chain-of-thought reasoning
"""

import requests
import json
import re

OLLAMA_URL = "http://localhost:11434"

class Tool:
    """Base tool class"""
    def __init__(self, name, description):
        self.name = name
        self.description = description
    
    def execute(self, *args, **kwargs):
        raise NotImplementedError

class Calculator(Tool):
    """Calculator tool"""
    def __init__(self):
        super().__init__("calculator", "Performs mathematical calculations")
    
    def execute(self, expression):
        try:
            # Safe eval for basic math
            result = eval(expression, {"__builtins__": {}}, {})
            return f"Result: {result}"
        except:
            return "Error: Invalid expression"

class WebSearch(Tool):
    """Mock web search tool"""
    def __init__(self):
        super().__init__("web_search", "Searches the web for information")
    
    def execute(self, query):
        # Mock search results
        return f"Search results for '{query}': [Mock data about {query}]"

class AgentWithTools:
    """Agent that can use tools"""
    def __init__(self, name, role, tools=None, model="qwen3:1.7b"):
        self.name = name
        self.role = role
        self.model = model
        self.tools = tools or []
        self.memory = []
    
    def think(self, task):
        """Agent reasons about the task"""
        # Build context with tools
        tools_desc = "\n".join([f"- {t.name}: {t.description}" for t in self.tools])
        
        prompt = f"""You are {self.name}, a {self.role}.

Available tools:
{tools_desc if tools_desc else "No tools available"}

Task: {task}

Think step-by-step and provide your response. If you need to use a tool, mention it clearly."""
        
        response = requests.post(
            f"{OLLAMA_URL}/api/generate",
            json={
                "model": self.model,
                "prompt": prompt,
                "stream": False
            }
        )
        
        result = response.json()["response"]
        self.memory.append({"task": task, "response": result})
        
        # Check if agent wants to use tools
        if "calculator" in result.lower() and any(t.name == "calculator" for t in self.tools):
            # Extract calculation if mentioned
            calc_match = re.search(r'(\d+[\+\-\*\/]\d+)', result)
            if calc_match:
                calc_result = self.use_tool("calculator", calc_match.group(1))
                result += f"\n\n{calc_result}"
        
        return result
    
    def use_tool(self, tool_name, *args, **kwargs):
        """Use a specific tool"""
        for tool in self.tools:
            if tool.name == tool_name:
                return tool.execute(*args, **kwargs)
        return f"Tool '{tool_name}' not available"

class AdvancedOrchestrator:
    """Advanced orchestrator with workflow management"""
    def __init__(self):
        self.agents = {}
        self.workflow_history = []
    
    def register_agent(self, agent_type, agent):
        self.agents[agent_type] = agent
        print(f"✓ Registered {agent.name} with {len(agent.tools)} tools")
    
    def execute_workflow(self, steps):
        """Execute a multi-step workflow"""
        print("\n🔄 Executing workflow...")
        results = []
        
        for i, step in enumerate(steps, 1):
            print(f"\n📍 Step {i}/{len(steps)}: {step['description']}")
            agent_type = step["agent"]
            task = step["task"]
            
            if agent_type not in self.agents:
                results.append({"error": f"Agent {agent_type} not found"})
                continue
            
            agent = self.agents[agent_type]
            print(f"   🤖 {agent.name} is processing...")
            result = agent.think(task)
            results.append({"step": i, "agent": agent_type, "result": result})
            print(f"   ✅ Completed")
        
        return results

def main():
    print("\n🚀 Advanced Agentic Framework Demo\n")
    print("="*60)
    
    # Create orchestrator
    orchestrator = AdvancedOrchestrator()
    
    # Create tools
    calc = Calculator()
    search = WebSearch()
    
    # Register agents with tools
    orchestrator.register_agent(
        "analyst", 
        AgentWithTools("DataAnalyst", "data analyst", tools=[calc])
    )
    orchestrator.register_agent(
        "researcher", 
        AgentWithTools("Researcher", "research specialist", tools=[search])
    )
    orchestrator.register_agent(
        "planner", 
        AgentWithTools("StrategyPlanner", "strategic planner")
    )
    
    # Define a workflow
    workflow = [
        {
            "description": "Research market size",
            "agent": "researcher",
            "task": "What is the global SaaS market size?"
        },
        {
            "description": "Calculate revenue projection",
            "agent": "analyst",
            "task": "If market is $200B and we capture 0.1%, what's our revenue? Calculate: 200000000000*0.001"
        },
        {
            "description": "Create strategy",
            "agent": "planner",
            "task": "Based on $200M revenue potential, outline a 3-point go-to-market strategy"
        }
    ]
    
    # Execute workflow
    results = orchestrator.execute_workflow(workflow)
    
    # Display results
    print("\n" + "="*60)
    print("📊 WORKFLOW RESULTS")
    print("="*60)
    
    for result in results:
        if "error" in result:
            print(f"\n❌ Error: {result['error']}")
        else:
            print(f"\n📋 Step {result['step']} ({result['agent']}):")
            print(result['result'][:200] + "...")
    
    print("\n✅ Workflow complete!\n")

if __name__ == "__main__":
    main()
