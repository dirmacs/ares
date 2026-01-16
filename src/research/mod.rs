//! Multi-Agent Research Coordination
//!
//! This module provides infrastructure for coordinating multiple agents
//! to perform complex research tasks that require gathering information
//! from multiple sources, synthesizing findings, and producing comprehensive reports.
//!
//! # Architecture
//!
//! The research system uses a coordinator pattern:
//! - [`coordinator::ResearchCoordinator`] - Orchestrates research tasks
//! - Spawns specialized sub-agents for different research aspects
//! - Aggregates and synthesizes results from multiple agents
//!
//! # Usage
//!
//! ```ignore
//! use ares::research::coordinator::ResearchCoordinator;
//!
//! let coordinator = ResearchCoordinator::new(agent_registry, config);
//!
//! let report = coordinator
//!     .research("What are the latest developments in quantum computing?")
//!     .await?;
//!
//! println!("Research Report:\n{}", report.summary);
//! for source in report.sources {
//!     println!("- {}", source.url);
//! }
//! ```
//!
//! # Research Workflow
//!
//! 1. **Query Analysis** - Break down the research question
//! 2. **Information Gathering** - Dispatch agents to search and retrieve
//! 3. **Fact Extraction** - Extract key facts from gathered information
//! 4. **Synthesis** - Combine findings into a coherent report
//! 5. **Citation** - Track and attribute sources

/// Research task coordination and multi-source aggregation.
pub mod coordinator;
