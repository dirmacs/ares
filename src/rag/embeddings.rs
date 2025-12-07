use crate::types::Result;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

pub struct EmbeddingService {
    model: TextEmbedding,
}

impl EmbeddingService {
    pub fn new(_model_name: &str) -> Result<Self> {
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
        )
        .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;

        Ok(Self { model })
    }

    pub fn embed(&mut self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>> {
        self.model
            .embed(texts, None)
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))
    }
}
