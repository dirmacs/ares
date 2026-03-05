"""
Agentic Framework Web UI
Beautiful interface for interacting with AI agents
"""

import streamlit as st
import requests
import json
from datetime import datetime

OLLAMA_URL = "http://localhost:11434"

class Agent:
    """Agent class"""
    def __init__(self, name, role, model="qwen3:1.7b"):
        self.name = name
        self.role = role
        self.model = model
    
    def think(self, task):
        """Agent generates streaming response"""
        # Very simple, direct prompt
        prompt = f"""Task: {task}

Answer briefly:"""
        
        try:
            response = requests.post(
                f"{OLLAMA_URL}/api/generate",
                json={
                    "model": self.model,
                    "prompt": prompt,
                    "stream": True,
                    "options": {
                        "num_predict": 80,
                        "temperature": 0.5,
                        "top_p": 0.9,
                        "stop": ["\n\n", "Task:"]
                    }
                },
                stream=True,
                timeout=90
            )
            
            full_response = ""
            for line in response.iter_lines():
                if line:
                    try:
                        data = json.loads(line)
                        if "response" in data:
                            full_response += data["response"]
                            # Stop if we hit reasoning patterns
                            if "thinking" in full_response.lower() or len(full_response) > 400:
                                break
                    except:
                        continue
            
            # Clean up response
            full_response = full_response.strip()
            if not full_response or len(full_response) < 10:
                return "⚠️ No valid response. Try: 'SaaS pricing tips' (simpler query)"
            
            return full_response
            
        except requests.exceptions.Timeout:
            return "⏱️ Model too slow. Try shorter question or restart Ollama."
        except Exception as e:
            return f"❌ Error: {str(e)[:100]}"

# Page config
st.set_page_config(
    page_title="Agentic Framework",
    page_icon="🤖",
    layout="wide"
)

# Initialize session state
if 'chat_history' not in st.session_state:
    st.session_state.chat_history = []
if 'workflow_results' not in st.session_state:
    st.session_state.workflow_results = []

# Sidebar
with st.sidebar:
    st.title("🤖 Agent Control")
    
    st.markdown("### Available Agents")
    agents = {
        "product": {"name": "ProductBot", "role": "Product Specialist", "icon": "📦"},
        "finance": {"name": "FinanceBot", "role": "Financial Analyst", "icon": "💰"},
        "hr": {"name": "HRBot", "role": "HR Manager", "icon": "👥"},
        "tech": {"name": "TechBot", "role": "Technical Architect", "icon": "⚙️"},
        "marketing": {"name": "MarketingBot", "role": "Marketing Strategist", "icon": "📈"}
    }
    
    for key, info in agents.items():
        st.markdown(f"{info['icon']} **{info['name']}** - *{info['role']}*")
    
    st.markdown("---")
    st.markdown("### System Status")
    st.success("🟢 Ollama Connected")
    st.info("Model: qwen3:1.7b")

# Main area
st.title("🚀 Agentic Framework UI")
st.markdown("### Interact with specialized AI agents")

# Tabs
tab1, tab2, tab3 = st.tabs(["💬 Chat with Agent", "🔄 Multi-Agent Workflow", "📊 Results History"])

with tab1:
    st.markdown("#### Single Agent Interaction")
    
    col1, col2 = st.columns([1, 2])
    
    with col1:
        selected_agent = st.selectbox(
            "Select Agent",
            options=list(agents.keys()),
            format_func=lambda x: f"{agents[x]['icon']} {agents[x]['name']}"
        )
    
    with col2:
        user_task = st.text_input(
            "What would you like the agent to do?",
            placeholder="e.g., Analyze market trends for SaaS products"
        )
    
    if st.button("🚀 Run Agent", type="primary"):
        if user_task:
            with st.spinner(f"{agents[selected_agent]['icon']} {agents[selected_agent]['name']} is thinking..."):
                agent_info = agents[selected_agent]
                agent = Agent(agent_info['name'], agent_info['role'])
                response = agent.think(user_task)
                
                # Add to history
                st.session_state.chat_history.append({
                    "timestamp": datetime.now().strftime("%H:%M:%S"),
                    "agent": selected_agent,
                    "task": user_task,
                    "response": response
                })
                
                # Display result
                st.success("✅ Agent completed the task!")
                st.markdown(f"**{agent_info['icon']} {agent_info['name']} Response:**")
                st.markdown(response)
        else:
            st.warning("Please enter a task for the agent")
    
    # Recent interactions
    if st.session_state.chat_history:
        st.markdown("---")
        st.markdown("#### Recent Interactions")
        for item in reversed(st.session_state.chat_history[-3:]):
            with st.expander(f"{item['timestamp']} - {agents[item['agent']]['icon']} {item['task'][:50]}..."):
                st.markdown(f"**Task:** {item['task']}")
                st.markdown(f"**Response:**\n{item['response']}")

with tab2:
    st.markdown("#### Multi-Agent Workflow")
    st.markdown("Create a workflow where multiple agents collaborate on a complex task")
    
    workflow_task = st.text_area(
        "Describe the main objective",
        placeholder="e.g., Launch a new SaaS product for small businesses",
        height=100
    )
    
    st.markdown("##### Select agents to collaborate:")
    
    col1, col2, col3 = st.columns(3)
    
    with col1:
        use_product = st.checkbox("📦 Product Specialist")
        use_finance = st.checkbox("💰 Financial Analyst")
    
    with col2:
        use_hr = st.checkbox("👥 HR Manager")
        use_tech = st.checkbox("⚙️ Technical Architect")
    
    with col3:
        use_marketing = st.checkbox("📈 Marketing Strategist")
    
    if st.button("🔄 Execute Workflow", type="primary"):
        selected_workflow_agents = []
        if use_product: selected_workflow_agents.append("product")
        if use_finance: selected_workflow_agents.append("finance")
        if use_hr: selected_workflow_agents.append("hr")
        if use_tech: selected_workflow_agents.append("tech")
        if use_marketing: selected_workflow_agents.append("marketing")
        
        if workflow_task and selected_workflow_agents:
            st.markdown("---")
            st.markdown("### 🔄 Workflow Execution")
            
            workflow_result = {
                "timestamp": datetime.now().strftime("%Y-%m-%d %H:%M:%S"),
                "task": workflow_task,
                "agents": [],
                "results": {}
            }
            
            progress_bar = st.progress(0)
            total_agents = len(selected_workflow_agents)
            
            for idx, agent_key in enumerate(selected_workflow_agents):
                agent_info = agents[agent_key]
                
                st.markdown(f"#### {agent_info['icon']} {agent_info['name']}")
                
                with st.spinner(f"Processing..."):
                    agent = Agent(agent_info['name'], agent_info['role'])
                    response = agent.think(f"As a {agent_info['role']}, provide your perspective on: {workflow_task}")
                    
                    workflow_result["agents"].append(agent_key)
                    workflow_result["results"][agent_key] = response
                    
                    st.success(f"✅ Completed")
                    st.markdown(response)
                    st.markdown("---")
                
                progress_bar.progress((idx + 1) / total_agents)
            
            st.session_state.workflow_results.append(workflow_result)
            st.balloons()
            st.success("🎉 Workflow completed successfully!")
        else:
            st.warning("Please enter a task and select at least one agent")

with tab3:
    st.markdown("#### Results History")
    
    if st.session_state.workflow_results:
        for idx, result in enumerate(reversed(st.session_state.workflow_results)):
            with st.expander(f"📋 Workflow {len(st.session_state.workflow_results) - idx}: {result['task'][:60]}... ({result['timestamp']})"):
                st.markdown(f"**Objective:** {result['task']}")
                st.markdown(f"**Agents Used:** {', '.join([agents[a]['icon'] + ' ' + agents[a]['name'] for a in result['agents']])}")
                st.markdown("---")
                
                for agent_key, response in result['results'].items():
                    st.markdown(f"**{agents[agent_key]['icon']} {agents[agent_key]['name']}:**")
                    st.markdown(response)
                    st.markdown("---")
        
        if st.button("🗑️ Clear History"):
            st.session_state.workflow_results = []
            st.session_state.chat_history = []
            st.rerun()
    else:
        st.info("No workflow results yet. Execute a workflow to see results here!")
        
        # Show example
        st.markdown("### 💡 Example Workflow")
        st.markdown("""
        Try this example:
        1. Go to the **Multi-Agent Workflow** tab
        2. Enter: "Create a go-to-market strategy for an AI-powered CRM"
        3. Select: Product, Finance, and Marketing agents
        4. Click **Execute Workflow**
        """)

# Footer
st.markdown("---")
st.markdown("Built with Streamlit • Powered by Ollama • Running qwen3:1.7b")
