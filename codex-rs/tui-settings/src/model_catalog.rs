use codex_protocol::openai_models::ModelPreset;
use std::convert::Infallible;

#[derive(Debug, Clone)]
pub struct ModelCatalog {
    models: Vec<ModelPreset>,
}

impl ModelCatalog {
    pub fn new(models: Vec<ModelPreset>) -> Self {
        Self { models }
    }

    pub fn try_list_models(&self) -> Result<Vec<ModelPreset>, Infallible> {
        Ok(self.models.clone())
    }
}
