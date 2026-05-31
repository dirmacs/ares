¶/opt/ares/crates/ares-agents/src/research/mod.rs#11B9
1://! Multi-Agent Research Coordination
2://!
3://! This module provides infrastructure for coordinating multiple agents
4://! to perform complex research tasks that require gathering information
5://! from multiple sources, synthesizing findings, and producing comprehensive reports.
6://!
7://! # Architecture
8://!
9://! The research system uses a coordinator pattern:
10://! - [`research::coordinator::ResearchCoordinator`](crate::research::coordinator::ResearchCoordinator) - Orchestrates research tasks
11://! - Spawns specialized sub-agents for different research aspects
12://! - Aggregates and synthesizes results from multiple agents
13://!
14://! # Usage
15://!
16://! ```ignore
17://! use ares::research::coordinator::ResearchCoordinator;
18://!
19://! let coordinator = ResearchCoordinator::new(agent_registry, config);
20://!
21://! let report = coordinator
22://!     .research("What are the latest developments in quantum computing?")
23://!     .await?;
24://!
25://! println!("Research Report:\n{}", report.summary);
26://! for source in report.sources {
27://!     println!("- {}", source.url);
28://! }
29://! ```
30://!
31://! # Research Workflow
32://!
33://! 1. **Query Analysis** - Break down the research question
34://! 2. **Information Gathering** - Dispatch agents to search and retrieve
35://! 3. **Fact Extraction** - Extract key facts from gathered information
36://! 4. **Synthesis** - Combine findings into a coherent report
37://! 5. **Citation** - Track and attribute sources
38:
39:/// Research task coordination and multi-source aggregation.
40:pub mod coordinator;

#[cfg(test)]
mod tests {
    use super::coordinator::ResearchUsage;

    #[test]
    fn coordinator_module_is_reachable() {
        let usage = ResearchUsage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
    }
}
41: