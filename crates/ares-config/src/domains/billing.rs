use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============= Billing Configuration =============

/// Billing and estimated-cost configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BillingConfig {
    /// Explicit provider/model pricing entries keyed by an operator-friendly name.
    #[serde(default)]
    pub model_pricing: HashMap<String, ModelPricingConfig>,
}

impl BillingConfig {
    /// Find pricing by runtime provider/model identifiers.
    pub fn pricing_for(
        &self,
        provider_name: &str,
        model_name: &str,
    ) -> Option<&ModelPricingConfig> {
        let provider_key = pricing_key(provider_name);
        let model_key = pricing_key(model_name);
        self.model_pricing.values().find(|pricing| {
            pricing_key(&pricing.provider) == provider_key
                && pricing_key(&pricing.model) == model_key
        })
    }
}

/// Pricing for a single runtime provider/model pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricingConfig {
    /// Runtime provider name, such as `openai` or `ollama-local`.
    pub provider: String,
    /// Runtime model identifier.
    pub model: String,
    /// USD per one million prompt/input tokens, if known.
    pub input_usd_per_million_tokens: Option<f64>,
    /// USD per one million completion/output tokens, if known.
    pub output_usd_per_million_tokens: Option<f64>,
    /// Currency for this estimate. Defaults to USD.
    #[serde(default = "default_billing_currency")]
    pub currency: String,
}

fn default_billing_currency() -> String {
    "USD".to_string()
}

fn pricing_key(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}
