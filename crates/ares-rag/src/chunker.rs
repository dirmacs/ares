//! Text chunking for document processing.
//!
//! This module provides various chunking strategies for splitting documents
//! into manageable pieces for embedding and retrieval:
//! - **Word-based**: Simple word count chunking with overlap
//! - **Semantic**: Sentence/paragraph aware chunking using text-splitter
//! - **Token-based**: Token-aware chunking for LLM context limits

use std::str::FromStr;

use serde::{Deserialize, Serialize};
use text_splitter::TextSplitter;

use ares_types::types::{AppError, Result};

// ============================================================================
// Chunking Strategy Types
// ============================================================================

/// Available chunking strategies
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ChunkingStrategy {
    /// Simple word-based chunking with overlap
    #[default]
    Word,
    /// Semantic chunking using sentence/paragraph boundaries
    Semantic,
    /// Character-based chunking
    Character,
}

impl FromStr for ChunkingStrategy {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "word" | "words" => Ok(Self::Word),
            "semantic" | "sentence" | "paragraph" => Ok(Self::Semantic),
            "character" | "char" | "chars" => Ok(Self::Character),
            _ => Err(AppError::Internal(format!(
                "Unknown chunking strategy: {}. Use: word, semantic, character",
                s
            ))),
        }
    }
}

impl std::fmt::Display for ChunkingStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Word => "word",
            Self::Semantic => "semantic",
            Self::Character => "character",
        };
        write!(f, "{}", name)
    }
}

// ============================================================================
// Chunker Configuration
// ============================================================================

/// Configuration for the text chunker
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkerConfig {
    /// Chunking strategy to use
    #[serde(default)]
    pub strategy: ChunkingStrategy,
    /// Target chunk size (in words for word strategy, characters for others)
    #[serde(default = "default_chunk_size")]
    pub chunk_size: usize,
    /// Overlap between chunks (for word strategy)
    #[serde(default = "default_chunk_overlap")]
    pub chunk_overlap: usize,
    /// Minimum chunk size to keep
    #[serde(default = "default_min_chunk_size")]
    pub min_chunk_size: usize,
}

fn default_chunk_size() -> usize {
    512
}

fn default_chunk_overlap() -> usize {
    50
}

fn default_min_chunk_size() -> usize {
    20
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            strategy: ChunkingStrategy::default(),
            chunk_size: default_chunk_size(),
            chunk_overlap: default_chunk_overlap(),
            min_chunk_size: default_min_chunk_size(),
        }
    }
}

// ============================================================================
// Chunk Result
// ============================================================================

/// A single chunk with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// Chunk index (0-based)
    pub index: usize,
    /// Chunk content
    pub content: String,
    /// Start position in original text (character offset)
    pub start_offset: usize,
    /// End position in original text (character offset)
    pub end_offset: usize,
}

// ============================================================================
// Text Chunker
// ============================================================================

/// Text chunker for splitting documents
#[derive(Debug, Clone)]
pub struct TextChunker {
    config: ChunkerConfig,
}

impl TextChunker {
    /// Create a new text chunker with the given configuration
    pub fn new(config: ChunkerConfig) -> Self {
        Self { config }
    }

    /// Create with word-based chunking (backward compatible)
    pub fn with_word_chunking(chunk_size: usize, chunk_overlap: usize) -> Self {
        Self::new(ChunkerConfig {
            strategy: ChunkingStrategy::Word,
            chunk_size,
            chunk_overlap,
            min_chunk_size: default_min_chunk_size(),
        })
    }

    /// Create with semantic chunking
    pub fn with_semantic_chunking(max_chunk_size: usize) -> Self {
        Self::new(ChunkerConfig {
            strategy: ChunkingStrategy::Semantic,
            chunk_size: max_chunk_size,
            chunk_overlap: 0, // Not used for semantic
            min_chunk_size: default_min_chunk_size(),
        })
    }

    /// Create with character-based chunking
    pub fn with_character_chunking(chunk_size: usize, chunk_overlap: usize) -> Self {
        Self::new(ChunkerConfig {
            strategy: ChunkingStrategy::Character,
            chunk_size,
            chunk_overlap,
            min_chunk_size: default_min_chunk_size(),
        })
    }

    /// Chunk text and return simple string vector (backward compatible)
    pub fn chunk(&self, text: &str) -> Vec<String> {
        self.chunk_with_metadata(text)
            .into_iter()
            .map(|c| c.content)
            .collect()
    }

    /// Chunk text with full metadata
    pub fn chunk_with_metadata(&self, text: &str) -> Vec<Chunk> {
        match self.config.strategy {
            ChunkingStrategy::Word => self.chunk_by_words(text),
            ChunkingStrategy::Semantic => self.chunk_semantically(text),
            ChunkingStrategy::Character => self.chunk_by_characters(text),
        }
    }

    /// Word-based chunking with overlap
    fn chunk_by_words(&self, text: &str) -> Vec<Chunk> {
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut chunks = Vec::new();
        let step = self
            .config
            .chunk_size
            .saturating_sub(self.config.chunk_overlap)
            .max(1);

        let mut chunk_index = 0;
        let mut word_index = 0;

        while word_index < words.len() {
            let end = (word_index + self.config.chunk_size).min(words.len());
            let chunk_words = &words[word_index..end];
            let content = chunk_words.join(" ");

            if content.len() >= self.config.min_chunk_size {
                // Calculate approximate character offsets
                let start_offset = if word_index == 0 {
                    0
                } else {
                    words[..word_index]
                        .iter()
                        .map(|w| w.len() + 1)
                        .sum::<usize>()
                };
                let end_offset = start_offset + content.len();

                chunks.push(Chunk {
                    index: chunk_index,
                    content,
                    start_offset,
                    end_offset,
                });
                chunk_index += 1;
            }

            word_index += step;
        }

        chunks
    }

    /// Semantic chunking using text-splitter
    fn chunk_semantically(&self, text: &str) -> Vec<Chunk> {
        let splitter = TextSplitter::new(self.config.chunk_size);

        let mut chunks = Vec::new();
        let mut current_offset = 0;

        for (index, chunk_text) in splitter.chunks(text).enumerate() {
            // Find the actual position in the original text
            let start_offset = text[current_offset..]
                .find(chunk_text)
                .map(|pos| current_offset + pos)
                .unwrap_or(current_offset);
            let end_offset = start_offset + chunk_text.len();

            if chunk_text.len() >= self.config.min_chunk_size {
                chunks.push(Chunk {
                    index,
                    content: chunk_text.to_string(),
                    start_offset,
                    end_offset,
                });
            }

            current_offset = end_offset;
        }

        chunks
    }

    /// Character-based chunking with overlap
    fn chunk_by_characters(&self, text: &str) -> Vec<Chunk> {
        let chars: Vec<char> = text.chars().collect();
        let mut chunks = Vec::new();
        let step = self
            .config
            .chunk_size
            .saturating_sub(self.config.chunk_overlap)
            .max(1);

        let mut char_index = 0;
        let mut chunk_index = 0;

        while char_index < chars.len() {
            let end = (char_index + self.config.chunk_size).min(chars.len());
            let content: String = chars[char_index..end].iter().collect();

            if content.len() >= self.config.min_chunk_size {
                chunks.push(Chunk {
                    index: chunk_index,
                    content,
                    start_offset: char_index,
                    end_offset: end,
                });
                chunk_index += 1;
            }

            char_index += step;
        }

        chunks
    }

    /// Get the current configuration
    pub fn config(&self) -> &ChunkerConfig {
        &self.config
    }
}

impl Default for TextChunker {
    fn default() -> Self {
        Self::new(ChunkerConfig::default())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunking_strategy_from_str() {
        assert_eq!(
            "word".parse::<ChunkingStrategy>().unwrap(),
            ChunkingStrategy::Word
        );
        assert_eq!(
            "semantic".parse::<ChunkingStrategy>().unwrap(),
            ChunkingStrategy::Semantic
        );
        assert_eq!(
            "character".parse::<ChunkingStrategy>().unwrap(),
            ChunkingStrategy::Character
        );
    }

    #[test]
    fn test_chunking_strategy_from_str_aliases() {
        let aliases = [
            ("words", ChunkingStrategy::Word),
            ("sentence", ChunkingStrategy::Semantic),
            ("paragraph", ChunkingStrategy::Semantic),
            ("char", ChunkingStrategy::Character),
            ("chars", ChunkingStrategy::Character),
        ];
        for (input, expected) in aliases {
            let parsed: ChunkingStrategy = input.parse().unwrap();
            assert_eq!(parsed, expected, "alias {:?}", input);
        }
    }

    #[test]
    fn test_chunking_strategy_from_str_case_insensitive() {
        let parsed: ChunkingStrategy = "SEMANTIC".parse().unwrap();
        assert_eq!(parsed, ChunkingStrategy::Semantic);
    }

    #[test]
    fn test_chunking_strategy_from_str_invalid() {
        let result = "token-based".parse::<ChunkingStrategy>();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Unknown chunking strategy"));
    }

    #[test]
    fn test_word_chunking_basic() {
        let chunker = TextChunker::with_word_chunking(5, 2);
        let text = "one two three four five six seven eight nine ten";
        let chunks = chunker.chunk(text);

        assert!(!chunks.is_empty());
        assert!(chunks[0].split_whitespace().count() <= 5);
    }

    #[test]
    fn test_word_chunking_overlap() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Word,
            chunk_size: 4,
            chunk_overlap: 2,
            min_chunk_size: 5,
        };
        let chunker = TextChunker::new(config);
        let text = "alpha bravo charlie delta echo foxtrot golf hotel india juliet";
        let chunks = chunker.chunk(text);

        assert!(
            chunks.len() > 1,
            "Expected multiple chunks, got: {:?}",
            chunks
        );

        let overlap = 2usize;
        for window in chunks.windows(2) {
            let prev_words: Vec<&str> = window[0].split_whitespace().collect();
            let next_words: Vec<&str> = window[1].split_whitespace().collect();
            let shared: Vec<&str> = prev_words
                .iter()
                .copied()
                .filter(|w| next_words.contains(w))
                .collect();
            assert!(
                shared.len() >= overlap,
                "Expected at least {} shared words between {:?} and {:?}, got {:?}",
                overlap,
                window[0],
                window[1],
                shared
            );
        }
    }

    #[test]
    fn test_word_chunking_respects_max_words_per_chunk() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Word,
            chunk_size: 3,
            chunk_overlap: 0,
            min_chunk_size: 1,
        };
        let chunker = TextChunker::new(config);
        let text = "one two three four five six seven";
        let chunks = chunker.chunk(text);

        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(
                chunk.split_whitespace().count() <= 3,
                "chunk {:?} exceeds word limit",
                chunk
            );
        }
    }

    #[test]
    fn test_semantic_chunking() {
        let chunker = TextChunker::with_semantic_chunking(100);
        let text = "This is the first sentence. This is the second sentence. \
                    And here is a third one that is a bit longer.";
        let chunks = chunker.chunk(text);

        // Should create chunks respecting sentence boundaries
        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_character_chunking() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Character,
            chunk_size: 20,
            chunk_overlap: 5,
            min_chunk_size: 10,
        };
        let chunker = TextChunker::new(config);
        let text = "This is a test string that should be chunked by characters.";
        let chunks = chunker.chunk_with_metadata(text);

        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert!(chunk.content.len() <= 20);
        }
    }

    #[test]
    fn test_character_chunking_overlap_shares_characters() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Character,
            chunk_size: 12,
            chunk_overlap: 4,
            min_chunk_size: 4,
        };
        let chunker = TextChunker::new(config);
        let text = "abcdefghijklmnopqrstuvwxyz";
        let chunks = chunker.chunk(text);

        assert!(chunks.len() >= 2);
        for window in chunks.windows(2) {
            let (left, right) = (&window[0], &window[1]);
            let left_suffix = &left[left.len().saturating_sub(4)..];
            assert!(
                right.starts_with(left_suffix),
                "Expected {:?} to start with overlap {:?}",
                right,
                left_suffix
            );
        }
    }

    #[test]
    fn test_chunk_metadata() {
        let chunker = TextChunker::with_semantic_chunking(50);
        let text = "Hello world. This is a test.";
        let chunks = chunker.chunk_with_metadata(text);

        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].index, 0);
        assert!(chunks[0].start_offset < chunks[0].end_offset);
    }

    #[test]
    fn test_default_config() {
        let config = ChunkerConfig::default();
        assert_eq!(config.strategy, ChunkingStrategy::Word);
        assert_eq!(config.chunk_size, 512);
        assert_eq!(config.chunk_overlap, 50);
    }

    #[test]
    fn test_backward_compatible_api() {
        // Old API should still work
        let chunker = TextChunker::with_word_chunking(100, 10);
        let text = "Hello world. This is a test with multiple words.";
        let chunks = chunker.chunk(text);
        assert!(!chunks.is_empty());
    }

    #[test]
    fn test_empty_text() {
        let chunker = TextChunker::default();
        let chunks = chunker.chunk("");
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_unicode_emoji_chunking() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Character,
            chunk_size: 10,
            chunk_overlap: 2,
            min_chunk_size: 5,
        };
        let chunker = TextChunker::new(config);
        let text = "Hello 🌍 world — 你好";
        let chunks = chunker.chunk(text);
        assert!(!chunks.is_empty());
        let joined: String = chunks.join("");
        assert!(joined.contains("🌍"));
    }

    #[test]
    fn test_zero_chunk_size_produces_no_word_chunks() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Word,
            chunk_size: 0,
            chunk_overlap: 0,
            min_chunk_size: 1,
        };
        let chunker = TextChunker::new(config);
        let chunks = chunker.chunk("alpha beta gamma");
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_very_long_document_word_chunking() {
        let chunker = TextChunker::with_word_chunking(50, 10);
        let word = "word ";
        let text = word.repeat(10_000);
        let chunks = chunker.chunk(&text);
        assert!(chunks.len() > 100);
        for chunk in &chunks {
            assert!(chunk.split_whitespace().count() <= 50);
        }
    }

    #[test]
    fn test_chunking_strategy_display_roundtrip() {
        for strategy in [
            ChunkingStrategy::Word,
            ChunkingStrategy::Semantic,
            ChunkingStrategy::Character,
        ] {
            let s = strategy.to_string();
            let parsed: ChunkingStrategy = s.parse().unwrap();
            assert_eq!(parsed, strategy);
        }
    }

    #[test]
    fn test_small_text() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Word,
            chunk_size: 100,
            chunk_overlap: 10,
            min_chunk_size: 5,
        };
        let chunker = TextChunker::new(config);
        let text = "Short text";
        let chunks = chunker.chunk(text);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "Short text");
    }

    #[test]
    fn test_whitespace_only_text() {
        let chunker = TextChunker::default();
        assert!(chunker.chunk("   \n\t  ").is_empty());
    }

    #[test]
    fn test_min_chunk_size_filters_short_chunks() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Word,
            chunk_size: 2,
            chunk_overlap: 0,
            min_chunk_size: 50,
        };
        let chunker = TextChunker::new(config);
        let chunks = chunker.chunk("hi there");
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_overlap_ge_chunk_size_advances_one_word_at_a_time() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Word,
            chunk_size: 3,
            chunk_overlap: 10,
            min_chunk_size: 1,
        };
        let chunker = TextChunker::new(config);
        let text = "a b c d e f";
        let chunks = chunker.chunk(text);
        assert!(chunks.len() >= 4);
    }

    #[test]
    fn test_text_chunker_config_accessor() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Semantic,
            chunk_size: 128,
            chunk_overlap: 0,
            min_chunk_size: 10,
        };
        let chunker = TextChunker::new(config.clone());
        assert_eq!(chunker.config().strategy, ChunkingStrategy::Semantic);
        assert_eq!(chunker.config().chunk_size, 128);
    }

    #[test]
    fn test_chunking_strategy_serde_kebab_case() {
        let json = r#""semantic""#;
        let strategy: ChunkingStrategy = serde_json::from_str(json).unwrap();
        assert_eq!(strategy, ChunkingStrategy::Semantic);

        let serialized = serde_json::to_string(&ChunkingStrategy::Character).unwrap();
        assert_eq!(serialized, "\"character\"");
    }

    #[test]
    fn test_chunker_config_serde_defaults() {
        let json = r#"{
            "chunk_size": 64
        }"#;
        let config: ChunkerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.strategy, ChunkingStrategy::Word);
        assert_eq!(config.chunk_size, 64);
        assert_eq!(config.chunk_overlap, 50);
        assert_eq!(config.min_chunk_size, 20);
    }

    #[test]
    fn test_chunker_config_serde_roundtrip() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Character,
            chunk_size: 256,
            chunk_overlap: 32,
            min_chunk_size: 8,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ChunkerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.strategy, ChunkingStrategy::Character);
        assert_eq!(parsed.chunk_size, 256);
        assert_eq!(parsed.chunk_overlap, 32);
        assert_eq!(parsed.min_chunk_size, 8);
    }

    #[test]
    fn test_semantic_chunking_filters_below_min_size() {
        let config = ChunkerConfig {
            strategy: ChunkingStrategy::Semantic,
            chunk_size: 200,
            chunk_overlap: 0,
            min_chunk_size: 10_000,
        };
        let chunker = TextChunker::new(config);
        let text = "Short.";
        assert!(chunker.chunk(text).is_empty());
    }

    #[test]
    fn test_chunk_metadata_indices_are_sequential() {
        let chunker = TextChunker::with_character_chunking(8, 2);
        let chunks = chunker.chunk_with_metadata("0123456789abcdef");
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
            assert!(chunk.start_offset <= chunk.end_offset);
        }
    }
}
