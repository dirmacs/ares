"""
Fast Agentic Framework Web UI
Connects to A.R.E.S server API
"""

import streamlit as st
import requests
import json
from datetime import datetime

ARES_URL = "http://localhost:3000"

# Map UI agents to A.R.E.S agent types
AGENT_TYPE_MAP = {
    "product": "product",
    "finance": "finance", 
    "hr": "hr",
    "marketing": "sales",  # Map to sales agent
    "tech": "product"  # Map to product agent
}

class AresAgent:
    """Agent that connects to A.R.E.S API"""
    def __init__(self, name, role, agent_type):
        self.name = name
        self.role = role
        self.agent_type = agent_type
    
    @staticmethod
    def get_token():
        """Get authentication token from session or login"""
        # Check session cache
        if 'ares_token' in st.session_state:
            return st.session_state.ares_token
        
        try:
            # Try to login
            response = requests.post(
                f"{ARES_URL}/api/auth/login",
                json={
                    "email": "agent@test.com",
                    "password": "test12345678"
                },
                timeout=10
            )
            if response.status_code == 200:
                data = response.json()
                token = data.get("access_token") or data.get("token")
                st.session_state.ares_token = token
                return token
            else:
                # Try to register if login fails
                reg_response = requests.post(
                    f"{ARES_URL}/api/auth/register",
                    json={
                        "email": "agent@test.com",
                        "password": "test12345678",
                        "name": "Agent"
                    },
                    timeout=10
                )
                if reg_response.status_code == 200:
                    reg_data = reg_response.json()
                    token = reg_data.get("access_token") or reg_data.get("token")
                    st.session_state.ares_token = token
                    return token
        except Exception as e:
            st.error(f"Auth error: {str(e)}")
        return None
    
    def chat(self, task):
        """Send chat request to A.R.E.S API"""
        try:
            # Get token
            token = self.get_token()
            if not token:
                yield "❌ Failed to authenticate with A.R.E.S server. Check if server is running."
                return
            
            # Send chat request
            headers = {"Authorization": f"Bearer {token}"}
            response = requests.post(
                f"{ARES_URL}/api/chat",
                headers=headers,
                json={
                    "message": task,
                    "agent_type": self.agent_type
                },
                timeout=120
            )
            
            if response.status_code == 200:
                data = response.json()
                result = data.get("response", "No response from agent")
                # Yield the full response (A.R.E.S doesn't stream)
                yield result
            elif response.status_code == 401:
                # Token expired, clear cache and retry
                if 'ares_token' in st.session_state:
                    del st.session_state.ares_token
                yield "⚠️ Session expired. Please try again."
            else:
                yield f"❌ Error: {response.status_code} - {response.text[:200]}"
            
        except requests.exceptions.Timeout:
            yield "⏱️ Request timed out. The agent is taking too long to respond."
        except Exception as e:
            yield f"❌ Error connecting to A.R.E.S: {str(e)}"

# Page config
st.set_page_config(
    page_title="⚡ Fast Agentic Framework",
    page_icon="🤖",
    layout="wide"
)

# Custom CSS
st.markdown("""
    <style>
    .agent-card {
        padding: 1rem;
        border-radius: 10px;
        border: 2px solid #e0e0e0;
        margin: 0.5rem 0;
    }
    .success-card {
        background-color: #d4edda;
        border-color: #c3e6cb;
    }
    .stButton>button {
        width: 100%;
    }
    </style>
""", unsafe_allow_html=True)

# Initialize session state
if 'chat_history' not in st.session_state:
    st.session_state.chat_history = []

# Header
st.title("⚡ Fast Agentic Framework")
st.markdown("**Connected to A.R.E.S Server API**")

# Check A.R.E.S connection
try:
    health = requests.get(f"{ARES_URL}/health", timeout=2)
    if health.status_code == 200:
        st.success("✅ A.R.E.S Server: Online")
    else:
        st.error("❌ A.R.E.S Server: Offline")
except:
    st.error("❌ A.R.E.S Server: Cannot connect to http://localhost:3000")

# Sidebar
with st.sidebar:
    st.markdown("### 🤖 Available Agents")
    
    agents = {
        "product": {"name": "ProductBot", "role": "Product Specialist", "icon": "📦", "color": "#4CAF50"},
        "finance": {"name": "FinanceBot", "role": "Financial Analyst", "icon": "💰", "color": "#2196F3"},
        "hr": {"name": "HRBot", "role": "HR Manager", "icon": "👥", "color": "#FF9800"},
        "marketing": {"name": "MarketingBot", "role": "Marketing Expert", "icon": "📈", "color": "#9C27B0"},
        "tech": {"name": "TechBot", "role": "Technical Advisor", "icon": "💻", "color": "#00BCD4"}
    }
    
    for key, agent in agents.items():
        st.markdown(f"{agent['icon']} **{agent['name']}** - {agent['role']}")
    
    st.markdown("---")
    st.markdown("### ⚙️ Settings")
    show_history = st.checkbox("Show History", value=True)
    
    if st.button("🗑️ Clear History"):
        st.session_state.chat_history = []
        st.rerun()

# Main interface
tab1, tab2 = st.tabs(["💬 Chat with Agent", "🔄 Quick Actions"])

with tab1:
    col1, col2 = st.columns([1, 3])
    
    with col1:
        selected_agent = st.selectbox(
            "Select Agent",
            options=list(agents.keys()),
            format_func=lambda x: f"{agents[x]['icon']} {agents[x]['name']}"
        )
    
    with col2:
        user_task = st.text_input(
            "What do you need help with?",
            placeholder="e.g., How to price my SaaS product?",
            key="task_input"
        )
    
    if st.button("⚡ Ask Agent", type="primary"):
        if user_task:
            agent_info = agents[selected_agent]
            
            # Create placeholder for streaming
            with st.container():
                st.markdown(f"### {agent_info['icon']} {agent_info['name']}")
                response_placeholder = st.empty()
                
                # Send request to A.R.E.S
                ares_agent_type = AGENT_TYPE_MAP.get(selected_agent, "product")
                agent = AresAgent(agent_info['name'], agent_info['role'], ares_agent_type)
                full_response = ""
                
                with st.spinner("🤔 Agent is thinking..."):
                    try:
                        for chunk in agent.chat(user_task):
                            if chunk:
                                full_response += chunk
                                response_placeholder.markdown(full_response)
                    except Exception as e:
                        full_response = f"Error: {str(e)}"
                
                # Display final response
                if full_response:
                    response_placeholder.markdown(full_response)
                else:
                    response_placeholder.warning("⚠️ No response generated")
                
                # Add to history
                st.session_state.chat_history.append({
                    "timestamp": datetime.now().strftime("%H:%M:%S"),
                    "agent": selected_agent,
                    "task": user_task,
                    "response": full_response
                })
                
                st.success("✅ Complete!")
        else:
            st.warning("⚠️ Please enter a question")

with tab2:
    st.markdown("### 🚀 Quick Actions")
    
    col1, col2, col3 = st.columns(3)
    
    quick_actions = {
        "Market Analysis": {"agent": "marketing", "task": "What are the top 3 SaaS market trends?"},
        "Pricing Strategy": {"agent": "finance", "task": "Suggest a pricing model for a B2B SaaS"},
        "Hiring Plan": {"agent": "hr", "task": "What roles to hire first for a startup?"},
        "Feature Ideas": {"agent": "product", "task": "Suggest 3 innovative SaaS features"},
        "Tech Stack": {"agent": "tech", "task": "Recommend a modern web app tech stack"},
        "Growth Hacks": {"agent": "marketing", "task": "Share 3 growth hacking strategies"}
    }
    
    actions_list = list(quick_actions.items())
    
    with col1:
        for i in range(0, len(actions_list), 3):
            if i < len(actions_list):
                action_name, action = actions_list[i]
                if st.button(f"⚡ {action_name}", key=f"action_{i}"):
                    st.session_state.quick_action = action
                    st.rerun()
    
    with col2:
        for i in range(1, len(actions_list), 3):
            if i < len(actions_list):
                action_name, action = actions_list[i]
                if st.button(f"⚡ {action_name}", key=f"action_{i}"):
                    st.session_state.quick_action = action
                    st.rerun()
    
    with col3:
        for i in range(2, len(actions_list), 3):
            if i < len(actions_list):
                action_name, action = actions_list[i]
                if st.button(f"⚡ {action_name}", key=f"action_{i}"):
                    st.session_state.quick_action = action
                    st.rerun()
    
    # Execute quick action if set
    if hasattr(st.session_state, 'quick_action'):
        action = st.session_state.quick_action
        agent_info = agents[action['agent']]
        
        with st.container():
            st.markdown(f"### {agent_info['icon']} {agent_info['name']}")
            st.markdown(f"**Task:** {action['task']}")
            response_placeholder = st.empty()
            
            ares_agent_type = AGENT_TYPE_MAP.get(action['agent'], "product")
            agent = AresAgent(agent_info['name'], agent_info['role'], ares_agent_type)
            full_response = ""
            
            with st.spinner("🤔 Agent is thinking..."):
                try:
                    for chunk in agent.chat(action['task']):
                        if chunk:
                            full_response += chunk
                            response_placeholder.markdown(full_response)
                except Exception as e:
                    full_response = f"Error: {str(e)}"
            
            if full_response:
                response_placeholder.markdown(full_response)
            else:
                response_placeholder.warning("⚠️ No response generated")
            st.success("✅ Complete!")
            
            # Add to history
            st.session_state.chat_history.append({
                "timestamp": datetime.now().strftime("%H:%M:%S"),
                "agent": action['agent'],
                "task": action['task'],
                "response": full_response
            })
        
        del st.session_state.quick_action

# History section
if show_history and st.session_state.chat_history:
    st.markdown("---")
    st.markdown("### 📜 Recent History")
    
    for item in reversed(st.session_state.chat_history[-5:]):
        with st.expander(f"{item['timestamp']} - {agents[item['agent']]['icon']} {item['task'][:60]}..."):
            st.markdown(f"**Agent:** {agents[item['agent']]['name']}")
            st.markdown(f"**Task:** {item['task']}")
            st.markdown(f"**Response:**\n\n{item['response']}")
