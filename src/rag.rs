pub use ares_rag::*;

#[cfg(test)]
mod tests {
    use super::chunker::ChunkingStrategy;
    use std::str::FromStr;

    #[test]
    fn rag_crate_reexport_exposes_chunking_strategy() {
        let strategy = ChunkingStrategy::from_str("word").expect("parse");
        assert_eq!(strategy, ChunkingStrategy::Word);
    }
}
