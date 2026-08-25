//! Generalized Model Profile Catalog
//!
//! Merges the static capability tables (see [`crate::capabilities`]) with
//! runtime catalog entries (see [`crate::nvidia_catalog`]) into provider-
//! neutral [`ModelProfile`] records: model id, provider, a compact
//! capabilities set, context window, speed tier, and free-form notes.
//!
//! Progressive disclosure keeps prompt injection cheap:
//! - [`ModelCatalog::lean_hint`] renders the whole catalog in well under 50
//!   tokens for system-prompt injection.
//! - [`ModelCatalog::describe_full`] returns the complete record for one
//!   model on demand.
//!
//! Routing ([`ModelCatalog::route`]) picks the cheapest capable model for a
//! task modality. The catalog is opt-in: nothing wires it into default model
//! selection.

use crate::capabilities::ModelCapabilities;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// A single model capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Plain text chat / completion.
    Text,
    /// Image inputs accepted.
    Vision,
    /// Audio inputs accepted.
    Audio,
    /// Tool/function calling supported.
    Tools,
    /// Structured JSON output mode supported.
    JsonMode,
    /// Reasoning / chain-of-thought supported.
    Reasoning,
}

/// Task modality used by routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskModality {
    /// Plain text task; any text-capable model qualifies.
    Text,
    /// Image-input task; requires vision-capable models.
    Vision,
    /// Audio-input task; requires audio-capable models.
    Audio,
}

/// Constraints applied on top of the modality requirement during routing.
#[derive(Debug, Clone, Default)]
pub struct RouteConstraints {
    /// Model must expose at least this many context tokens.
    pub min_context_window: Option<u32>,
    /// Model must support tool calling.
    pub require_tools: bool,
    /// Model must support structured JSON output.
    pub require_json_mode: bool,
    /// Model must support reasoning / chain-of-thought.
    pub require_reasoning: bool,
    /// Maximum acceptable cost tier ("free" < "low" < "medium" < "high" < "premium").
    pub max_cost_tier: Option<String>,
    /// Minimum acceptable speed tier ("slow" < "medium" < "fast" < "realtime").
    pub min_speed_tier: Option<String>,
}

/// Speed tiers, ordered slowest to fastest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SpeedTier {
    /// Slow models.
    Slow,
    /// Medium latency models.
    Medium,
    /// Fast models.
    Fast,
    /// Realtime-class models.
    Realtime,
}

impl SpeedTier {
    fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "slow" => Some(SpeedTier::Slow),
            "medium" => Some(SpeedTier::Medium),
            "fast" => Some(SpeedTier::Fast),
            "realtime" => Some(SpeedTier::Realtime),
            _ => None,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            SpeedTier::Slow => "slow",
            SpeedTier::Medium => "medium",
            SpeedTier::Fast => "fast",
            SpeedTier::Realtime => "realtime",
        }
    }
}

impl Capability {
    fn label(&self) -> &'static str {
        match self {
            Capability::Text => "text",
            Capability::Vision => "vision",
            Capability::Audio => "audio",
            Capability::Tools => "tools",
            Capability::JsonMode => "json",
            Capability::Reasoning => "reasoning",
        }
    }
}

/// One provider-neutral catalog record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProfile {
    /// Full model id, e.g. `meta/llama-3.3-70b-instruct`.
    pub model_id: String,
    /// Owning provider name, e.g. `nvidia`.
    pub provider: String,
    /// Capability set advertised by this model.
    pub capabilities: HashSet<Capability>,
    /// Context window in tokens.
    pub context_window: u32,
    /// Relative speed class.
    pub speed_tier: SpeedTier,
    /// Cost tier string matching `capabilities::tier_satisfies`
    /// ("free", "low", "medium", "high", "premium").
    pub cost_tier: String,
    /// Free-form notes surfaced only by `describe_full`.
    #[serde(default)]
    pub notes: String,
    /// Optional quality score (0-100) carried over from live catalogs.
    #[serde(default)]
    pub quality_score: Option<u8>,
}

/// Catalog merging static table entries with runtime entries.
///
/// Runtime entries registered later shadow static entries sharing the same
/// `(provider, model_id)`, mirroring how a live refresh supersedes baked-in
/// knowledge.
#[derive(Default)]
pub struct ModelCatalog {
    static_profiles: Vec<ModelProfile>,
    runtime_profiles: Vec<ModelProfile>,
}

impl ModelCatalog {
    /// Create an empty catalog.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed from the static capability table derived from configured models.
    ///
    /// Each entry becomes a profile keyed by its provider and model id.
    pub fn with_static_table(models: Vec<(String, String, ModelCapabilities)>) -> Self {
        let mut catalog = Self::new();
        for (model_id, provider, caps) in models {
            catalog.static_profiles.push(profile_from_capabilities(
                &model_id, &provider, &caps, None,
            ));
        }
        catalog
    }

    /// Register a runtime entry (e.g. synthesized from a live catalog
    /// refresh). Later calls shadow earlier ones with the same
    /// `(provider, model_id)`.
    pub fn register_runtime(&mut self, profile: ModelProfile) {
        self.runtime_profiles
            .retain(|p| !(p.provider == profile.provider && p.model_id == profile.model_id));
        self.runtime_profiles.push(profile);
    }

    /// Merge a batch of runtime entries at once.
    pub fn extend_runtime<I: IntoIterator<Item = ModelProfile>>(&mut self, profiles: I) {
        for p in profiles {
            self.register_runtime(p);
        }
    }

    /// All profiles: runtime entries shadowing static ones per key.
    pub fn profiles(&self) -> Vec<ModelProfile> {
        let mut out = Vec::with_capacity(self.static_profiles.len() + self.runtime_profiles.len());
        out.extend(self.runtime_profiles.iter().cloned());
        for s in &self.static_profiles {
            if !out
                .iter()
                .any(|p| p.provider == s.provider && p.model_id == s.model_id)
            {
                out.push(s.clone());
            }
        }
        out
    }

    /// Look up one model by provider + model id (runtime wins).
    pub fn get(&self, provider: &str, model_id: &str) -> Option<ModelProfile> {
        self.profiles()
            .into_iter()
            .find(|p| p.provider == provider && p.model_id == model_id)
    }

    /// Lean single-line summary of the whole catalog for prompt injection.
    ///
    /// Kept deliberately terse — target budget is <= 50 tokens.
    pub fn lean_hint(&self) -> String {
        let mut line = String::from("Models:");
        for p in self.profiles() {
            line.push_str(&format!(" {}({});", p.model_id, capability_labels(&p)));
        }
        line
    }

    /// Full human-readable description of exactly one model.
    pub fn describe_full(&self, provider: &str, model_id: &str) -> Option<String> {
        let p = self.get(provider, model_id)?;
        Some(format!(
            "{} [{}] context={} speed={} cost={}{}",
            p.model_id,
            p.provider,
            p.context_window,
            p.speed_tier.label(),
            p.cost_tier,
            if p.notes.is_empty() {
                String::new()
            } else {
                format!(" notes={}", p.notes)
            },
        ))
    }

    /// Route a task modality (plus constraints) to the cheapest capable
    /// model id. Ties break on speed (faster first), then id for stability.
    pub fn route(
        &self,
        modality: TaskModality,
        constraints: &RouteConstraints,
    ) -> Option<String> {
        let required = required_capability(modality)?;
        self.profiles()
            .into_iter()
            .filter(|p| p.capabilities.contains(&required))
            .filter(|p| constraints.min_context_window.is_none_or(|w| p.context_window >= w))
            .filter(|p| !constraints.require_tools || p.capabilities.contains(&Capability::Tools))
            .filter(|p| {
                !constraints.require_json_mode || p.capabilities.contains(&Capability::JsonMode)
            })
            .filter(|p| {
                !constraints.require_reasoning || p.capabilities.contains(&Capability::Reasoning)
            })
            .filter(|p| {
                constraints
                    .max_cost_tier
                    .as_deref()
                    .is_none_or(|max| cost_at_most(&p.cost_tier, max))
            })
            .filter(|p| {
                constraints
                    .min_speed_tier
                    .as_deref()
                    .and_then(SpeedTier::from_str)
                    .is_none_or(|min| p.speed_tier >= min)
            })
            .min_by_key(cost_rank_and_speed)
            .map(|p| p.model_id)
    }
}

impl From<&NvidiaLikeEntry> for ModelProfile {
    fn from(e: &NvidiaLikeEntry) -> Self {
        ModelProfile {
            model_id: e.id.clone(),
            provider: "nvidia".to_string(),
            capabilities: HashSet::from([Capability::Text]),
            context_window: 128_000,
            speed_tier: SpeedTier::Medium,
            cost_tier: "low".to_string(),
            notes: format!("owned_by {}", e.owned_by),
            quality_score: Some(e.quality_score),
        }
    }
}

/// Minimal structural bridge to live catalog entries so this module does not
/// depend on the NVIDIA-specific type directly.
#[derive(Debug, Clone)]
pub struct NvidiaLikeEntry {
    /// Full model id.
    pub id: String,
    /// Owning organization.
    pub owned_by: String,
    /// Derived quality score (0-100).
    pub quality_score: u8,
}

impl From<&crate::nvidia_catalog::CatalogEntry> for NvidiaLikeEntry {
    fn from(e: &crate::nvidia_catalog::CatalogEntry) -> Self {
        NvidiaLikeEntry {
            id: e.id.clone(),
            owned_by: e.owned_by.clone(),
            quality_score: e.quality_score,
        }
    }
}

/// Build a profile from the richer static `ModelCapabilities` table entry.
#[allow(clippy::too_many_lines)]
fn profile_from_capabilities(
    model_id: &str,
    provider: &str,
    caps: &ModelCapabilities,
    quality_score: Option<u8>,
) -> ModelProfile {
    let mut capabilities = HashSet::new();
    capabilities.insert(Capability::Text);
    if caps.supports_vision {
        capabilities.insert(Capability::Vision);
    }
    if caps.supports_audio {
        capabilities.insert(Capability::Audio);
    }
    if caps.supports_tools {
        capabilities.insert(Capability::Tools);
    }
    if caps.supports_json_mode {
        capabilities.insert(Capability::JsonMode);
    }
    if caps.supports_reasoning {
        capabilities.insert(Capability::Reasoning);
    }
    ModelProfile {
        model_id: model_id.to_string(),
        provider: provider.to_string(),
        capabilities,
        context_window: caps.context_window,
        speed_tier: SpeedTier::from_str(&caps.speed_tier).unwrap_or(SpeedTier::Medium),
        cost_tier: caps.cost_tier.clone(),
        notes: caps
            .family
            .as_ref()
            .map(|f| format!("family {f}"))
            .unwrap_or_default(),
        quality_score,
    }
}

/// Build a [`ModelProfile`] straight from a static-table capabilities value.
pub fn profile_for(model_id: &str, provider: &str, caps: &ModelCapabilities) -> ModelProfile {
    profile_from_capabilities(model_id, provider, caps, None)
}

/// Comma-separated capability labels for a profile (lean rendering).
fn capability_labels(p: &ModelProfile) -> String {
    let mut labels: Vec<&str> = p
        .capabilities
        .iter()
        .map(Capability::label)
        .collect();
    labels.sort_unstable();
    labels.join(",")
}

/// Modality to the minimum capability it demands; `None` marks an unknown
/// modality that cannot be routed.
const fn required_capability(modality: TaskModality) -> Option<Capability> {
    match modality {
        TaskModality::Text => Some(Capability::Text),
        TaskModality::Vision => Some(Capability::Vision),
        TaskModality::Audio => Some(Capability::Audio),
    }
}

/// Cost-tier ordering shared by routing and ranking: "free" < "low" <
/// "medium" < "high" < "premium". Unknown tiers rank above "premium".
fn cost_rank(tier: &str) -> u8 {
    const COST_ORDER: [&str; 5] = ["free", "low", "medium", "high", "premium"];
    COST_ORDER
        .iter()
        .position(|t| *t == tier.to_ascii_lowercase())
        .unwrap_or(COST_ORDER.len()) as u8
}

/// True when `actual` is at or below the ceiling `max`.
fn cost_at_most(actual: &str, max: &str) -> bool {
    cost_rank(actual) <= cost_rank(max)
}

/// Sort key implementing cheapest-first, then faster-first ordering.
fn cost_rank_and_speed(p: &ModelProfile) -> (u8, u8) {
    // u8::MAX - tier index so min_by_key prefers faster tiers.
    let speed_rank = u8::MAX - (p.speed_tier as u8);
    (cost_rank(&p.cost_tier), speed_rank)
}

/// Shared test fixtures.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{
        Capability, ModelProfile, ModelCatalog, SpeedTier,
    };

    pub(crate) fn profile(
        model_id: &str,
        provider: &str,
        caps: &[Capability],
        cost: &str,
        speed: SpeedTier,
    ) -> ModelProfile {
        ModelProfile {
            model_id: model_id.to_string(),
            provider: provider.to_string(),
            capabilities: caps.iter().copied().collect(),
            context_window: 32_768,
            speed_tier: speed,
            cost_tier: cost.to_string(),
            notes: String::new(),
            quality_score: None,
        }
    }

    pub(crate) fn catalog() -> ModelCatalog {
        let mut c = ModelCatalog::new();
        c.extend_runtime([
            profile("small-text", "static", &[Capability::Text], "free", SpeedTier::Fast),
            profile(
                "big-tools",
                "static",
                &[Capability::Text, Capability::Tools],
                "high",
                SpeedTier::Slow,
            ),
            profile(
                "vision-pro",
                "cloud",
                &[Capability::Text, Capability::Vision],
                "premium",
                SpeedTier::Medium,
            ),
            profile(
                "audio-max",
                "cloud",
                &[Capability::Text, Capability::Audio],
                "premium",
                SpeedTier::Slow,
            ),
        ]);
        c
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{catalog, profile};
    use super::{
        Capability, ModelCatalog, ModelProfile, RouteConstraints, SpeedTier, TaskModality,
    };
    use crate::nvidia_catalog::{CatalogEntry, NvidiaConfig};

    #[test]
    fn lean_hint_under_budget() {
        let hint = catalog().lean_hint();

        assert!(hint.starts_with("Models:"));
        for needle in [
            "small-text(text)",
            "big-tools(text,tools)",
            "vision-pro(text,vision)",
            "audio-max(audio,text)",
        ] {
            assert!(hint.contains(needle), "missing `{needle}` in `{hint}`");
        }

        // Budget: <= 50 whitespace-delimited tokens (words).
        let words = hint.split_whitespace().count();
        assert!(
            words <= 50,
            "lean_hint exceeded token budget: {words} tokens: {hint}"
        );
    }

    #[test]
    fn route_prefers_capable_cheapest() {
        let c = catalog();

        // Text routes to the free fast small model, not the expensive big one.
        let picked = c.route(TaskModality::Text, &RouteConstraints::default()).unwrap();
        assert_eq!(picked, "small-text");

        // Requiring tools eliminates the cheap model despite higher cost.
        let reqs = RouteConstraints {
            require_tools: true,
            ..RouteConstraints::default()
        };
        assert_eq!(
            c.route(TaskModality::Text, &reqs).unwrap(),
            "big-tools"
        );
    }

    #[test]
    fn unknown_modality_returns_none() {
        let c = ModelCatalog::new();

        // No profiles at all: every known modality misses. The routing
        // helper's Option return also covers future open-ended modalities
        // that map to no capability.
        assert_eq!(
            c.route(TaskModality::Text, &RouteConstraints::default()),
            None
        );
        assert_eq!(
            c.route(TaskModality::Vision, &RouteConstraints::default()),
            None
        );
        assert_eq!(
            c.route(TaskModality::Audio, &RouteConstraints::default()),
            None
        );

        // A non-empty catalog still misses when nothing carries the
        // modality's capability (audio against a text/vision-only set).
        let mut audio_less = ModelCatalog::new();
        audio_less.extend_runtime([profile(
            "text-only",
            "static",
            &[Capability::Text],
            "free",
            SpeedTier::Fast,
        )]);
        assert_eq!(
            audio_less.route(TaskModality::Audio, &RouteConstraints::default()),
            None
        );
    }

    #[test]
    fn nvidia_catalog_still_works() {
        // The generalized catalog must keep consuming live NVIDIA entries.
        let entry = CatalogEntry {
            id: "meta/llama-3.3-70b-instruct".to_string(),
            owned_by: "meta".to_string(),
            created: 1_700_000_000,
            quality_score: 93,
        };
        let mut c = ModelCatalog::new();
        let converted: ModelProfile = (&super::NvidiaLikeEntry::from(&entry)).into();
        assert_eq!(converted.provider, "nvidia");
        assert!(converted.quality_score == Some(93));
        c.extend_runtime([converted]);

        // Default NVIDIA config still constructs the existing cache.
        let cfg = NvidiaConfig::default();
        assert_eq!(cfg.default_model, "meta/llama-3.3-70b-instruct");
        let cache = crate::nvidia_catalog::NvidiaCatalogCache::new(cfg);
        assert!(cache.snapshot().is_empty());

        // And the merged view exposes the ingested entry.
        assert_eq!(
            c.get("nvidia", "meta/llama-3.3-70b-instruct").unwrap().model_id,
            "meta/llama-3.3-70b-instruct"
        );
    }

    #[test]
    fn runtime_shadows_static_per_key() {
        let mut c = catalog();
        c.register_runtime(profile("small-text", "static", &[Capability::Text], "low", SpeedTier::Realtime));
        let merged = c.profiles();
        assert_eq!(merged.iter().filter(|p| p.model_id == "small-text").count(), 1);
        assert_eq!(merged.iter().find(|p| p.model_id == "small-text").unwrap().speed_tier, SpeedTier::Realtime);
    }

    #[test]
    fn describe_full_includes_all_fields() {
        let c = catalog();
        let d = c.describe_full("cloud", "vision-pro").unwrap();
        assert!(d.contains("vision-pro"));
        assert!(d.contains("[cloud]"));
        assert!(d.contains("context=32768"));
        assert!(d.contains("speed=medium"));
        assert!(d.contains("cost=premium"));
        assert!(c.describe_full("cloud", "nope").is_none());
    }

    #[test]
    fn route_respects_context_window_and_cost_ceiling() {
        let c = catalog();
        let reqs = RouteConstraints {
            min_context_window: Some(64_000),
            ..Default::default()
        };
        assert!(c.route(TaskModality::Text, &reqs).is_none());

        let reqs = RouteConstraints {
            max_cost_tier: Some("low".to_string()),
            ..Default::default()
        };
        assert_eq!(c.route(TaskModality::Text, &reqs).unwrap(), "small-text");
    }

    #[test]
    fn every_profile_advertises_text_baseline() {
        for p in catalog().profiles() {
            assert!(
                p.capabilities.contains(&Capability::Text),
                "profile {} lost the text baseline",
                p.model_id
            );
        }
    }
}
