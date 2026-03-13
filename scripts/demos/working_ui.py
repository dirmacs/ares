"""
Working Agentic Framework UI with Mock Responses
Since Ollama qwen3:1.7b is too slow/unreliable, using mock responses for demo
"""

import streamlit as st
from datetime import datetime
import time

class Agent:
    """Agent class with mock intelligent responses"""
    def __init__(self, name, role):
        self.name = name
        self.role = role
    
    def think(self, task):
        """Generate intelligent mock response based on agent type"""
        time.sleep(1)  # Simulate thinking
        
        task_lower = task.lower()
        
        # Finance Agent responses
        if "finance" in self.role.lower():
            if "price" in task_lower or "pricing" in task_lower:
                return "For B2B SaaS pricing: 1) Start with value-based pricing tied to ROI, 2) Offer tiered plans (Starter/Professional/Enterprise), 3) Use usage-based billing for scalability, 4) Price 30-40% below enterprise competitors initially."
            elif "revenue" in task_lower:
                return "Focus on MRR (Monthly Recurring Revenue) and ARR growth. Target 3-5x rule: CAC payback in 12 months, LTV:CAC ratio of 3:1 minimum. Aim for 80%+ gross margins typical in SaaS."
            else:
                return "Key financial metrics for SaaS: Track burn rate, runway, CAC, LTV, churn rate, and net revenue retention. Aim for profitability or clear path to it within 18-24 months."
        
        # Product Agent responses
        elif "product" in self.role.lower():
            if "feature" in task_lower:
                return "Essential SaaS features: 1) Robust API for integrations, 2) Real-time collaboration tools, 3) Advanced analytics dashboard, 4) White-label options for enterprise, 5) Mobile-first responsive design."
            elif "market" in task_lower or "share" in task_lower:
                return "To gain market share: Focus on a specific niche first, solve a painful problem exceptionally well, build a strong community, leverage product-led growth (PLG), and prioritize user feedback."
            else:
                return "Build for scalability from day one. Focus on core value proposition, minimize feature bloat. Use agile methodology with 2-week sprints. Ship MVPs fast, iterate based on real usage data."
        
        # HR Agent responses
        elif "hr" in self.role.lower():
            if "hire" in task_lower or "hiring" in task_lower:
                return "First hires for startup: 1) Technical co-founder or lead engineer, 2) Sales/growth specialist, 3) Product designer, 4) Customer success manager. Hire for culture fit and learning agility over experience alone."
            elif "culture" in task_lower:
                return "Build strong culture: Define clear values early, hire slowly and deliberately, invest in onboarding, promote transparency, celebrate wins, provide growth opportunities, maintain work-life balance."
            else:
                return "Key HR priorities: Competitive compensation + equity, flexible work arrangements, clear career paths, continuous learning budget, strong feedback culture, and inclusive environment."
        
        # Marketing Agent responses
        elif "marketing" in self.role.lower():
            if "growth" in task_lower:
                return "Growth strategies: 1) Content marketing + SEO for organic traffic, 2) Product-led growth with freemium model, 3) Strategic partnerships and integrations, 4) Community building, 5) Referral programs with incentives."
            elif "trend" in task_lower:
                return "Top SaaS trends: AI integration, vertical SaaS solutions, product-led growth, usage-based pricing, API-first architecture, no-code/low-code tools, and embedded analytics."
            else:
                return "Marketing mix: 60% inbound (content, SEO), 20% outbound (cold email, LinkedIn), 20% partnerships. Focus on education-based content. Track every dollar spent to CAC."
        
        # Tech Agent responses
        elif "tech" in self.role.lower():
            if "stack" in task_lower:
                return "Modern web stack: Frontend - React/Next.js + TypeScript, Backend - Node.js/Python FastAPI, Database - PostgreSQL + Redis, Infrastructure - AWS/Vercel, Monitoring - Datadog/Sentry. Use microservices for scale."
            elif "architecture" in task_lower:
                return "SaaS architecture best practices: Multi-tenant with isolated data, event-driven microservices, API-first design, horizontal scaling, CDN for static assets, comprehensive monitoring and logging."
            else:
                return "Technical priorities: Build for security (SOC2), reliability (99.9% uptime), performance (<200ms response), scalability (handle 10x growth), and maintainability (clean code, tests)."
        
        # Default response
        else:
            return f"As a {self.role}, I recommend: Focus on understanding your target market deeply, validate assumptions with real users, move fast but deliberately, and prioritize actions with highest impact on key metrics."

# Page config
st.set_page_config(
    page_title="🤖 Agentic Framework",
    page_icon="🤖",
    layout="wide"
)

# Initialize session state
if 'chat_history' not in st.session_state:
    st.session_state.chat_history = []

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
    
    for key, agent in agents.items():
        st.markdown(f"{agent['icon']} **{agent['name']}** - {agent['role']}")
    
    st.markdown("---")
    st.markdown("### 💡 Note")
    st.info("Using smart mock responses for reliable demo. Responses are contextually generated based on your question.")
    
    if st.button("🗑️ Clear History"):
        st.session_state.chat_history = []
        st.rerun()

# Main area
st.title("🚀 Agentic Framework UI")
st.markdown("### Interact with specialized AI agents")
st.success("✅ System Online - Mock Mode (Fast & Reliable)")

# Chat Interface
st.markdown("#### 💬 Single Agent Interaction")

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
        placeholder="e.g., How to price my SaaS product?"
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

# Quick Actions
st.markdown("---")
st.markdown("### ⚡ Quick Actions")

col1, col2, col3 = st.columns(3)

quick_actions = [
    {"name": "Pricing Strategy", "agent": "finance", "task": "How to price my SaaS product?"},
    {"name": "Feature Ideas", "agent": "product", "task": "What features should I build?"},
    {"name": "Hiring Plan", "agent": "hr", "task": "Who should I hire first?"},
    {"name": "Tech Stack", "agent": "tech", "task": "What tech stack should I use?"},
    {"name": "Growth Strategy", "agent": "marketing", "task": "How to grow my SaaS?"},
    {"name": "Market Trends", "agent": "marketing", "task": "What are the latest SaaS trends?"},
]

for i, action in enumerate(quick_actions):
    col = [col1, col2, col3][i % 3]
    with col:
        if st.button(f"⚡ {action['name']}", key=f"qa_{i}"):
            agent_info = agents[action['agent']]
            with st.spinner(f"{agent_info['icon']} Processing..."):
                agent = Agent(agent_info['name'], agent_info['role'])
                response = agent.think(action['task'])
            
            st.markdown(f"**{agent_info['icon']} {agent_info['name']}**")
            st.markdown(response)
            
            st.session_state.chat_history.append({
                "timestamp": datetime.now().strftime("%H:%M:%S"),
                "agent": action['agent'],
                "task": action['task'],
                "response": response
            })

# History section
if st.session_state.chat_history:
    st.markdown("---")
    st.markdown("### 📜 Recent Interactions")
    
    for item in reversed(st.session_state.chat_history[-5:]):
        with st.expander(f"{item['timestamp']} - {agents[item['agent']]['icon']} {item['task'][:50]}..."):
            st.markdown(f"**Task:** {item['task']}")
            st.markdown(f"**Response:**\n\n{item['response']}")
