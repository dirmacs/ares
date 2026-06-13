//! Azure AI Foundry OpenAI-compatible provider helpers.
//!
//! Foundry's `/openai/v1` endpoint accepts the standard OpenAI
//! chat-completions wire format, including `tools`. The provider therefore
//! reuses [`crate::openai::OpenAIClient`] and only supplies Azure-specific
//! environment defaults and headers.

use std::collections::HashMap;

/// Prefix accepted in ARES model config strings for direct Azure routing.
pub const MODEL_PREFIX: &str = "azure/";

/// Environment variable containing the Foundry API key.
pub const DEFAULT_API_KEY_ENV: &str = "AZURE_FOUNDRY_API_KEY";

/// Environment variable containing the Foundry OpenAI-compatible base URL.
pub const DEFAULT_BASE_URL_ENV: &str = "AZURE_FOUNDRY_BASE_URL";

/// Optional environment variable for the default Foundry model.
pub const DEFAULT_MODEL_ENV: &str = "AZURE_FOUNDRY_MODEL";

/// Default model used when no model override is provided.
pub const DEFAULT_MODEL: &str = "DeepSeek-V4-Flash";

/// Strip the `azure/` direct-routing prefix before sending the model id to Foundry.
pub fn strip_model_prefix(model: &str) -> &str {
    let trimmed = model.trim();
    trimmed
        .strip_prefix(MODEL_PREFIX)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(trimmed)
}

/// Normalize the configured base URL for OpenAI-compatible path joining.
pub fn normalize_base_url(api_base: &str) -> String {
    api_base.trim().trim_end_matches('/').to_string()
}

/// Headers accepted by Azure AI Foundry and classic Azure OpenAI gateways.
pub fn foundry_headers(api_key: &str) -> HashMap<String, String> {
    let mut headers = HashMap::with_capacity(2);
    headers.insert("api-key".to_string(), api_key.to_string());
    headers.insert("Authorization".to_string(), format!("Bearer {api_key}"));
    headers
}
