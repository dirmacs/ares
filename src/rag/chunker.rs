pub struct TextChunker {
    chunk_size: usize,
    chunk_overlap: usize,
}

impl TextChunker {
    pub fn new(chunk_size: usize, chunk_overlap: usize) -> Self {
        Self {
            chunk_size,
            chunk_overlap,
        }
    }

    pub fn chunk(&self, text: &str) -> Vec<String> {
        let words: Vec<&str> = text.split_whitespace().collect();
        let mut chunks = Vec::new();
        let step = self.chunk_size - self.chunk_overlap;

        for i in (0..words.len()).step_by(step) {
            let end = (i + self.chunk_size).min(words.len());
            let chunk = words[i..end].join(" ");
            chunks.push(chunk);
        }

        chunks
    }
}
