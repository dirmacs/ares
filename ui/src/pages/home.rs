//! Home/landing page

use leptos::prelude::*;
use crate::components::Header;
use crate::state::AppState;
use crate::api::load_agents;

/// Home page with hero section
#[component]
pub fn HomePage() -> impl IntoView {
    let state = expect_context::<AppState>();
    let is_auth = move || state.token.get().is_some();
    
    // Load agents on mount
    let state_for_effect = state.clone();
    Effect::new(move |_| {
        load_agents(state_for_effect.clone());
    });

    view! {
        <div class="min-h-screen flex flex-col">
            <Header />
            
            // Hero section
            <section class="flex-1 flex items-center justify-center px-4 py-16">
                <div class="max-w-4xl mx-auto text-center">
                    // Logo
                    <div class="mb-8 animate-fade-in">
                        <div class="w-24 h-24 mx-auto rounded-2xl bg-gradient-to-br from-blue-500 via-violet-500 to-purple-600 
                                    flex items-center justify-center text-5xl shadow-2xl shadow-violet-500/25">
                            "🤖"
                        </div>
                    </div>
                    
                    // Title
                    <h1 class="text-5xl md:text-7xl font-bold mb-6 animate-slide-up">
                        <span class="gradient-text">"A.R.E.S"</span>
                    </h1>
                    
                    <p class="text-xl md:text-2xl text-slate-400 mb-4 animate-slide-up" style="animation-delay: 0.1s">
                        "Agentic Reasoning & Execution System"
                    </p>
                    
                    <p class="text-lg text-slate-500 mb-12 max-w-2xl mx-auto animate-slide-up" style="animation-delay: 0.2s">
                        "A production-grade AI agent platform with multi-provider LLM support, "
                        "intelligent routing, tool calling, and RAG capabilities."
                    </p>
                    
                    // CTA buttons
                    <div class="flex flex-col sm:flex-row gap-4 justify-center animate-slide-up" style="animation-delay: 0.3s">
                        <Show
                            when=is_auth
                            fallback=move || view! {
                                <a
                                    href="/login"
                                    class="px-8 py-4 bg-blue-600 hover:bg-blue-700 rounded-xl text-lg font-semibold 
                                           transition-all hover:scale-105 hover:shadow-lg hover:shadow-blue-500/25"
                                >
                                    "Get Started"
                                </a>
                            }
                        >
                            <a
                                href="/chat"
                                class="px-8 py-4 bg-blue-600 hover:bg-blue-700 rounded-xl text-lg font-semibold 
                                       transition-all hover:scale-105 hover:shadow-lg hover:shadow-blue-500/25"
                            >
                                "Open Chat"
                            </a>
                        </Show>
                        
                        <a
                            href="https://github.com/dirmacs/ares"
                            target="_blank"
                            class="px-8 py-4 bg-slate-800 hover:bg-slate-700 border border-slate-700 
                                   rounded-xl text-lg font-semibold transition-all hover:scale-105"
                        >
                            "View on GitHub"
                        </a>
                    </div>
                </div>
            </section>
            
            // Features section
            <section class="py-20 px-4 bg-slate-800/50">
                <div class="max-w-6xl mx-auto">
                    <h2 class="text-3xl font-bold text-center mb-12">"Powerful Features"</h2>
                    
                    <div class="grid md:grid-cols-3 gap-8">
                        <FeatureCard
                            icon="🧠"
                            title="Multi-Agent System"
                            description="Specialized agents for different tasks with intelligent routing"
                        />
                        <FeatureCard
                            icon="🔧"
                            title="Tool Calling"
                            description="Built-in calculator, web search, and extensible tool framework"
                        />
                        <FeatureCard
                            icon="📚"
                            title="RAG Support"
                            description="Retrieval-augmented generation for knowledge-grounded responses"
                        />
                        <FeatureCard
                            icon="🔌"
                            title="Multi-Provider"
                            description="Ollama, OpenAI, and LlamaCpp support out of the box"
                        />
                        <FeatureCard
                            icon="💾"
                            title="Memory System"
                            description="Persistent conversation history and user preferences"
                        />
                        <FeatureCard
                            icon="⚡"
                            title="High Performance"
                            description="Built in Rust for maximum speed and reliability"
                        />
                    </div>
                </div>
            </section>
            
            // Agent showcase
            <section class="py-20 px-4">
                <div class="max-w-6xl mx-auto">
                    <h2 class="text-3xl font-bold text-center mb-4">"Available Agents"</h2>
                    <p class="text-slate-400 text-center mb-12">
                        "Specialized AI agents for every business need"
                    </p>
                    
                    <div class="grid sm:grid-cols-2 lg:grid-cols-3 gap-6">
                        {move || {
                            let agents = state.agents.get();
                            if agents.is_empty() {
                                // Default agents when not loaded
                                vec![
                                    ("🎭", "Orchestrator", "Coordinates complex multi-step tasks"),
                                    ("📦", "Product", "Product information and catalog queries"),
                                    ("💰", "Sales", "Sales data and customer insights"),
                                    ("📊", "Finance", "Financial reports and analytics"),
                                    ("👥", "HR", "HR policies and employee information"),
                                    ("📄", "Invoice", "Invoice processing and queries"),
                                ].into_iter().map(|(emoji, name, desc)| view! {
                                    <AgentCard emoji=emoji name=name description=desc />
                                }).collect::<Vec<_>>()
                            } else {
                                agents.into_iter().map(|agent| {
                                    let emoji = match agent.agent_type.as_str() {
                                        "orchestrator" => "🎭",
                                        "product" => "📦",
                                        "sales" => "💰",
                                        "finance" => "📊",
                                        "hr" => "👥",
                                        "invoice" => "📄",
                                        _ => "🤖",
                                    };
                                    view! {
                                        <AgentCard
                                            emoji=emoji
                                            name=agent.name.leak()
                                            description=agent.description.leak()
                                        />
                                    }
                                }).collect::<Vec<_>>()
                            }
                        }}
                    </div>
                </div>
            </section>
            
            // Footer
            <footer class="py-8 px-4 border-t border-slate-800 text-center text-slate-500">
                <p>"Built with 🦀 Rust • MIT License • © 2025 Dirmacs"</p>
            </footer>
        </div>
    }
}

/// Feature card component
#[component]
fn FeatureCard(
    icon: &'static str,
    title: &'static str,
    description: &'static str,
) -> impl IntoView {
    view! {
        <div class="p-6 bg-slate-800 rounded-xl border border-slate-700 hover:border-slate-600 transition-colors">
            <div class="text-4xl mb-4">{icon}</div>
            <h3 class="text-xl font-semibold mb-2">{title}</h3>
            <p class="text-slate-400">{description}</p>
        </div>
    }
}

/// Agent card component
#[component]
fn AgentCard(
    emoji: &'static str,
    name: &'static str,
    description: &'static str,
) -> impl IntoView {
    view! {
        <div class="p-6 bg-slate-800/50 rounded-xl border border-slate-700 hover:border-blue-500/50 
                    hover:bg-slate-800 transition-all group">
            <div class="w-12 h-12 rounded-xl bg-gradient-to-br from-violet-500 to-purple-600 
                        flex items-center justify-center text-2xl mb-4 group-hover:scale-110 transition-transform">
                {emoji}
            </div>
            <h3 class="text-lg font-semibold mb-2">{name}</h3>
            <p class="text-sm text-slate-400">{description}</p>
        </div>
    }
}
