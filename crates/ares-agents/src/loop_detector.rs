//! Loop detection for ARES agents.
//!
//! Detects when an agent is producing repetitive outputs and intervenes
//! to break the loop. Uses a sliding window of recent outputs with
//! similarity hashing, optional fuzzy matching, and iteration limits.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Configuration for loop detection and iteration limits.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopConfig {
    /// Maximum number of recent outputs to track.
    #[serde(default = "default_window_size")]
    pub window_size: usize,
    /// Number of identical hashes that trigger loop detection.
    #[serde(default = "default_repeat_threshold")]
    pub repeat_threshold: usize,
    /// Minimum output length to consider for loop detection.
    #[serde(default = "default_min_output_len")]
    pub min_output_len: usize,
    /// Maximum agent iterations before halting. `None` = unbounded.
    #[serde(default)]
    pub max_iterations: Option<u64>,
    /// When true, halt once [`max_iterations`] is reached.
    #[serde(default)]
    pub halt_on_max: bool,
    /// Whether failed iterations count against [`max_iterations`].
    #[serde(default = "default_count_failures")]
    pub count_failed_iterations: bool,
    /// Enable fuzzy signature matching (whitespace variants, near-duplicates).
    #[serde(default)]
    pub fuzzy_match: bool,
    /// Halt after this many consecutive iteration failures.
    #[serde(default = "default_halt_on_consecutive_failures")]
    pub halt_on_consecutive_failures: u32,
}

fn default_window_size() -> usize {
    10
}
fn default_repeat_threshold() -> usize {
    3
}
fn default_min_output_len() -> usize {
    20
}
fn default_count_failures() -> bool {
    true
}
fn default_halt_on_consecutive_failures() -> u32 {
    3
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            window_size: default_window_size(),
            repeat_threshold: default_repeat_threshold(),
            min_output_len: default_min_output_len(),
            max_iterations: None,
            halt_on_max: false,
            count_failed_iterations: default_count_failures(),
            fuzzy_match: false,
            halt_on_consecutive_failures: default_halt_on_consecutive_failures(),
        }
    }
}

/// Backward-compatible alias for [`LoopConfig`].
pub type LoopDetectorConfig = LoopConfig;

/// Runtime state for loop detection and iteration tracking.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopState {
    /// Sliding window of output similarity hashes (oldest first).
    pub recent_hashes: Vec<u64>,
    /// Sliding window of normalized signatures for fuzzy matching.
    pub recent_signatures: Vec<String>,
    /// Total outputs checked (including short/ignored).
    pub total_outputs: u64,
    /// Number of loops detected.
    pub loops_detected: u64,
    /// Total agent iterations run.
    pub iterations_run: u64,
    pub iterations_succeeded: u64,
    pub iterations_failed: u64,
    pub consecutive_failures: u32,
}

impl LoopState {
    /// Record a successful iteration. Resets the consecutive-failure counter.
    pub fn record_success(&mut self) {
        self.iterations_run += 1;
        self.iterations_succeeded += 1;
        self.consecutive_failures = 0;
    }

    /// Record a failed iteration. Increments the consecutive-failure counter.
    pub fn record_failure(&mut self) {
        self.iterations_run += 1;
        self.iterations_failed += 1;
        self.consecutive_failures += 1;
    }

    fn push_output(&mut self, config: &LoopConfig, hash: u64, signature: String) {
        if self.recent_hashes.len() >= config.window_size {
            self.recent_hashes.remove(0);
            self.recent_signatures.remove(0);
        }
        self.recent_hashes.push(hash);
        self.recent_signatures.push(signature);
    }
}

/// Result of checking an output for loops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopStatus {
    /// No loop detected, proceed normally.
    Ok,
    /// Loop detected — agent is repeating itself.
    LoopDetected {
        /// Number of consecutive repeats in the window.
        repeats: usize,
        /// Suggested action.
        action: LoopAction,
        /// Whether the match was exact-hash or fuzzy-signature.
        kind: LoopMatchKind,
    },
}

/// How a repetitive output was matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopMatchKind {
    Exact,
    Fuzzy,
}

/// Actions to take when a loop is detected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopAction {
    /// Inject a "you are repeating yourself" prompt.
    InjectWarning,
    /// Force the agent to try a different approach.
    ForceAlternative,
    /// Stop the agent entirely.
    HaltAgent,
}

/// Pure detection result from [`detect_loop`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopDetection {
    pub repeats: usize,
    pub kind: LoopMatchKind,
}

/// Hash normalized output content for exact duplicate detection.
pub fn similarity_hash(output: &str) -> u64 {
    let normalized = loop_signature(output);
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

/// Canonical normalized signature (whitespace-stripped, lowercased, capped).
pub fn loop_signature(output: &str) -> String {
    output
        .chars()
        .take(500)
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

/// Count how many entries in `hashes` equal `hash`.
pub fn count_repetitions(hashes: &[u64], hash: u64) -> usize {
    hashes.iter().filter(|&&h| h == hash).count()
}

/// Count signatures in `signatures` that fuzzy-match `current`.
pub fn count_fuzzy_repetitions(signatures: &[String], current: &str) -> usize {
    signatures
        .iter()
        .filter(|s| signatures_similar(s, current))
        .count()
}

fn signatures_similar(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let min_len = a.len().min(b.len());
    if min_len < 10 {
        return false;
    }
    let common = a
        .chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .count();
    common * 100 / min_len >= 80
}

/// Detect a repetitive output against recent window state (does not mutate `state`).
pub fn detect_loop(config: &LoopConfig, state: &LoopState, output: &str) -> Option<LoopDetection> {
    if output.len() < config.min_output_len {
        return None;
    }

    let hash = similarity_hash(output);
    let signature = loop_signature(output);
    let exact = count_repetitions(&state.recent_hashes, hash);
    let fuzzy = if config.fuzzy_match {
        count_fuzzy_repetitions(&state.recent_signatures, &signature)
    } else {
        0
    };

    let repeats = exact.max(fuzzy);
    if repeats < config.repeat_threshold {
        return None;
    }

    let kind = if exact >= config.repeat_threshold {
        LoopMatchKind::Exact
    } else {
        LoopMatchKind::Fuzzy
    };
    Some(LoopDetection { repeats, kind })
}

/// Returns true when [`LoopConfig::halt_on_max`] is set and iteration limits are reached.
pub fn should_halt_on_max_iterations(state: &LoopState, config: &LoopConfig) -> bool {
    if !config.halt_on_max {
        return false;
    }
    let Some(max) = config.max_iterations else {
        return false;
    };
    let counted = if config.count_failed_iterations {
        state.iterations_run
    } else {
        state.iterations_succeeded
    };
    counted >= max
}

/// Whether the agent should halt due to iteration limits or consecutive failures.
pub fn should_halt(state: &LoopState, config: &LoopConfig) -> bool {
    should_halt_on_max_iterations(state, config)
        || state.consecutive_failures >= config.halt_on_consecutive_failures
}

fn action_for_repeats(config: &LoopConfig, repeats: usize) -> LoopAction {
    if repeats >= config.repeat_threshold * 2 {
        LoopAction::HaltAgent
    } else if repeats > config.repeat_threshold {
        LoopAction::ForceAlternative
    } else {
        LoopAction::InjectWarning
    }
}

/// Tracks agent outputs and detects repetitive loops.
#[derive(Clone, Debug)]
pub struct LoopDetector {
    config: LoopConfig,
    state: LoopState,
}

impl LoopDetector {
    /// Create a new loop detector with default config.
    pub fn new() -> Self {
        Self::with_config(LoopConfig::default())
    }

    /// Create a new loop detector with custom config.
    pub fn with_config(config: LoopConfig) -> Self {
        Self {
            state: LoopState::default(),
            config,
        }
    }

    /// Check if the given output indicates a loop.
    pub fn check(&mut self, output: &str) -> LoopStatus {
        self.state.total_outputs += 1;

        if let Some(detection) = detect_loop(&self.config, &self.state, output) {
            self.state.loops_detected += 1;
            let action = action_for_repeats(&self.config, detection.repeats);
            let status = LoopStatus::LoopDetected {
                repeats: detection.repeats,
                action,
                kind: detection.kind,
            };
            // Still record the output in the window after detection.
            let hash = similarity_hash(output);
            let signature = loop_signature(output);
            self.state.push_output(&self.config, hash, signature);
            return status;
        }

        if output.len() >= self.config.min_output_len {
            let hash = similarity_hash(output);
            let signature = loop_signature(output);
            self.state.push_output(&self.config, hash, signature);
        }

        LoopStatus::Ok
    }

    /// Record iteration success and return whether the agent should halt.
    pub fn record_success(&mut self) -> bool {
        self.state.record_success();
        should_halt(&self.state, &self.config)
    }

    /// Record iteration failure and return whether the agent should halt.
    pub fn record_failure(&mut self) -> bool {
        self.state.record_failure();
        should_halt(&self.state, &self.config)
    }

    /// Reset the detector (e.g., on new conversation).
    pub fn reset(&mut self) {
        self.state = LoopState::default();
    }

    /// Get statistics: (total outputs, loops detected).
    pub fn stats(&self) -> (usize, usize) {
        (
            self.state.total_outputs as usize,
            self.state.loops_detected as usize,
        )
    }

    /// Access current configuration.
    pub fn config(&self) -> &LoopConfig {
        &self.config
    }

    /// Access current state.
    pub fn state(&self) -> &LoopState {
        &self.state
    }
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> LoopConfig {
        LoopConfig {
            window_size: 10,
            repeat_threshold: 3,
            min_output_len: 20,
            max_iterations: None,
            halt_on_max: false,
            count_failed_iterations: true,
            fuzzy_match: false,
            halt_on_consecutive_failures: 3,
        }
    }

    // --- Serde roundtrips ---

    #[test]
    fn loop_config_serde_roundtrip_json() {
        let cfg = LoopConfig {
            max_iterations: Some(50),
            halt_on_max: true,
            fuzzy_match: true,
            ..sample_config()
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: LoopConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cfg, back);
    }

    #[test]
    fn loop_state_serde_roundtrip_json() {
        let state = LoopState {
            recent_hashes: vec![1, 2, 3],
            recent_signatures: vec!["abc".into(), "def".into()],
            total_outputs: 5,
            loops_detected: 1,
            iterations_run: 10,
            iterations_succeeded: 8,
            iterations_failed: 2,
            consecutive_failures: 1,
        };
        let json = serde_json::to_string(&state).expect("serialize");
        let back: LoopState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(state, back);
    }

    // --- Pure helpers ---

    #[test]
    fn similarity_hash_normalizes_whitespace_and_case() {
        let a = similarity_hash("Hello   WORLD");
        let b = similarity_hash("hello\nworld");
        assert_eq!(a, b);
    }

    #[test]
    fn loop_signature_is_stable_and_normalized() {
        let sig = loop_signature("  Foo\tBAR  ");
        assert_eq!(sig, "foobar");
    }

    #[test]
    fn count_repetitions_counts_exact_matches() {
        let hashes = vec![10, 20, 10, 30, 10];
        assert_eq!(count_repetitions(&hashes, 10), 3);
        assert_eq!(count_repetitions(&hashes, 99), 0);
    }

    #[test]
    fn count_fuzzy_repetitions_detects_near_duplicates() {
        let sigs = vec![
            "searchingthecodebaseforauthhandlers".into(),
            "searchingthecodebaseforauthhandlerz".into(),
        ];
        let current = "searchingthecodebaseforauthhandlernow";
        assert_eq!(count_fuzzy_repetitions(&sigs, current), 2);
    }

    // --- detect_loop exact / fuzzy ---

    #[test]
    fn detect_loop_exact_repetition() {
        let config = LoopConfig {
            repeat_threshold: 2,
            min_output_len: 10,
            ..sample_config()
        };
        let repeated = "This is a repeated agent response for testing.";
        let mut state = LoopState::default();
        let hash = similarity_hash(repeated);
        let sig = loop_signature(repeated);
        state.push_output(&config, hash, sig.clone());
        state.push_output(&config, hash, sig);

        assert_eq!(
            detect_loop(&config, &state, repeated),
            Some(LoopDetection {
                repeats: 2,
                kind: LoopMatchKind::Exact,
            })
        );
    }

    #[test]
    fn detect_loop_no_loop_for_varied_outputs() {
        let config = sample_config();
        let state = LoopState::default();
        assert!(detect_loop(
            &config,
            &state,
            "First unique output that is definitely long enough."
        )
        .is_none());
    }

    #[test]
    fn detect_loop_skips_short_output() {
        let config = sample_config();
        let state = LoopState::default();
        assert!(detect_loop(&config, &state, "short").is_none());
    }

    #[test]
    fn detect_loop_fuzzy_match_when_enabled() {
        let config = LoopConfig {
            repeat_threshold: 2,
            min_output_len: 10,
            fuzzy_match: true,
            ..sample_config()
        };
        let base = "I am searching the codebase for authentication handlers now";
        let near = "I am searching the codebase for authentication handlers today";
        let mut state = LoopState::default();
        state.push_output(&config, similarity_hash(base), loop_signature(base));
        state.push_output(
            &config,
            similarity_hash("different hash variant"),
            loop_signature(base),
        );

        let detection = detect_loop(&config, &state, near).expect("fuzzy loop");
        assert_eq!(detection.kind, LoopMatchKind::Fuzzy);
        assert!(detection.repeats >= 2);
    }

    #[test]
    fn detect_loop_cache_key_pattern_repetition() {
        let config = LoopConfig {
            repeat_threshold: 2,
            min_output_len: 15,
            ..sample_config()
        };
        let key_a = "cache:get:user-session:abc123-def456-789";
        let key_b = "cache:get:user-session:abc123-def456-790";
        let mut state = LoopState::default();
        state.push_output(&config, similarity_hash(key_a), loop_signature(key_a));
        state.push_output(&config, similarity_hash(key_b), loop_signature(key_b));

        // Same prefix-heavy cache keys share a signature prefix → exact hash may differ
        // but message-pattern style repetition is caught via similar signatures when fuzzy on.
        let config_fuzzy = LoopConfig {
            fuzzy_match: true,
            ..config
        };
        let detection = detect_loop(&config_fuzzy, &state, key_b);
        assert!(detection.is_some());
    }

    #[test]
    fn detect_loop_message_pattern_repetition() {
        let config = LoopConfig {
            repeat_threshold: 2,
            min_output_len: 20,
            fuzzy_match: true,
            ..sample_config()
        };
        let msg1 = "Let me try running the tests again to see what fails.";
        let msg2 = "Let me try running the tests again to see what breaks.";
        let mut state = LoopState::default();
        state.push_output(&config, similarity_hash(msg1), loop_signature(msg1));
        state.push_output(&config, similarity_hash(msg1), loop_signature(msg1));

        let detection = detect_loop(&config, &state, msg2).expect("message pattern loop");
        assert!(detection.repeats >= 2);
    }

    // --- max_iterations / halt_on_max ---

    #[test]
    fn should_halt_on_max_iterations_when_at_limit() {
        let config = LoopConfig {
            max_iterations: Some(3),
            halt_on_max: true,
            count_failed_iterations: true,
            ..sample_config()
        };
        let mut state = LoopState::default();
        state.record_success();
        state.record_success();
        assert!(!should_halt_on_max_iterations(&state, &config));
        state.record_success();
        assert!(should_halt_on_max_iterations(&state, &config));
    }

    #[test]
    fn should_halt_on_max_iterations_boundary_below_limit() {
        let config = LoopConfig {
            max_iterations: Some(2),
            halt_on_max: true,
            ..sample_config()
        };
        let mut state = LoopState::default();
        state.record_success();
        assert!(!should_halt_on_max_iterations(&state, &config));
    }

    #[test]
    fn should_halt_on_max_iterations_ignores_when_halt_on_max_disabled() {
        let config = LoopConfig {
            max_iterations: Some(1),
            halt_on_max: false,
            ..sample_config()
        };
        let mut state = LoopState::default();
        state.record_success();
        assert!(!should_halt_on_max_iterations(&state, &config));
    }

    #[test]
    fn should_halt_on_max_iterations_counting_failures() {
        let config = LoopConfig {
            max_iterations: Some(2),
            halt_on_max: true,
            count_failed_iterations: true,
            ..sample_config()
        };
        let mut state = LoopState::default();
        state.record_failure();
        assert!(!should_halt_on_max_iterations(&state, &config));
        state.record_failure();
        assert!(should_halt_on_max_iterations(&state, &config));
    }

    #[test]
    fn should_halt_on_max_iterations_ignoring_failures() {
        let config = LoopConfig {
            max_iterations: Some(2),
            halt_on_max: true,
            count_failed_iterations: false,
            ..sample_config()
        };
        let mut state = LoopState::default();
        state.record_failure();
        state.record_failure();
        assert!(!should_halt_on_max_iterations(&state, &config));
        state.record_success();
        state.record_success();
        assert!(should_halt_on_max_iterations(&state, &config));
    }

    #[test]
    fn loop_detector_record_success_resets_consecutive_failures() {
        let mut detector = LoopDetector::with_config(sample_config());
        detector.state.consecutive_failures = 2;
        detector.record_success();
        assert_eq!(detector.state().consecutive_failures, 0);
        assert_eq!(detector.state().iterations_succeeded, 1);
    }

    #[test]
    fn loop_detector_record_failure_increments_consecutive_failures() {
        let mut detector = LoopDetector::with_config(LoopConfig {
            halt_on_consecutive_failures: 5,
            ..sample_config()
        });
        detector.record_failure();
        detector.record_failure();
        assert_eq!(detector.state().consecutive_failures, 2);
        assert_eq!(detector.state().iterations_failed, 2);
    }

    #[test]
    fn loop_detector_halt_on_max_via_record_success() {
        let mut detector = LoopDetector::with_config(LoopConfig {
            max_iterations: Some(2),
            halt_on_max: true,
            count_failed_iterations: false,
            ..sample_config()
        });
        assert!(!detector.record_success());
        assert!(detector.record_success());
    }

    // --- LoopDetector integration (existing behavior) ---

    #[test]
    fn test_no_loop() {
        let mut detector = LoopDetector::new();
        assert_eq!(detector.check("Hello, how can I help?"), LoopStatus::Ok);
        assert_eq!(detector.check("I can assist with that."), LoopStatus::Ok);
        assert_eq!(detector.check("Here's what I found."), LoopStatus::Ok);
    }

    #[test]
    fn test_loop_detected() {
        let mut detector = LoopDetector::new();
        let repeated = "I'm sorry, I cannot help with that request at this time.";
        assert_eq!(detector.check(repeated), LoopStatus::Ok);
        assert_eq!(detector.check(repeated), LoopStatus::Ok);
        assert_eq!(detector.check(repeated), LoopStatus::Ok);
        match detector.check(repeated) {
            LoopStatus::LoopDetected {
                repeats,
                action,
                kind,
            } => {
                assert!(repeats >= 3);
                assert_eq!(action, LoopAction::InjectWarning);
                assert_eq!(kind, LoopMatchKind::Exact);
            }
            _ => panic!("should detect loop"),
        }
    }

    #[test]
    fn test_short_output_ignored() {
        let mut detector = LoopDetector::new();
        assert_eq!(detector.check("ok"), LoopStatus::Ok);
        assert_eq!(detector.check("ok"), LoopStatus::Ok);
        assert_eq!(detector.check("ok"), LoopStatus::Ok);
        assert_eq!(detector.check("ok"), LoopStatus::Ok);
    }

    #[test]
    fn test_escalation() {
        let mut detector = LoopDetector::with_config(LoopConfig {
            window_size: 20,
            repeat_threshold: 2,
            min_output_len: 10,
            ..LoopConfig::default()
        });
        let repeated = "This is a repeated response that keeps coming back.";
        detector.check(repeated);
        detector.check(repeated);
        match detector.check(repeated) {
            LoopStatus::LoopDetected { action, .. } => {
                assert_eq!(action, LoopAction::InjectWarning)
            }
            _ => panic!("should warn"),
        }
        match detector.check(repeated) {
            LoopStatus::LoopDetected { action, .. } => {
                assert_eq!(action, LoopAction::ForceAlternative)
            }
            _ => panic!("should force alternative"),
        }
    }

    #[test]
    fn test_reset() {
        let mut detector = LoopDetector::new();
        let repeated = "A repeated output that should trigger detection.";
        detector.check(repeated);
        detector.check(repeated);
        detector.check(repeated);
        detector.reset();
        assert_eq!(detector.check(repeated), LoopStatus::Ok);
    }

    #[test]
    fn test_stats() {
        let mut detector = LoopDetector::new();
        detector.check("First unique output here and now.");
        detector.check("Second unique output here and now.");
        let (total, loops) = detector.stats();
        assert_eq!(total, 2);
        assert_eq!(loops, 0);
    }

    #[test]
    fn test_whitespace_normalization() {
        let mut detector = LoopDetector::with_config(LoopConfig {
            repeat_threshold: 2,
            ..LoopConfig::default()
        });
        detector.check("Hello   world,  how are you doing today?");
        detector.check("Hello world, how are you doing today?");
        match detector.check("Hello\n\tworld,\thow are you doing today?") {
            LoopStatus::LoopDetected { kind, .. } => {
                assert_eq!(kind, LoopMatchKind::Exact);
            }
            _ => panic!("whitespace-normalized duplicates should match"),
        }
    }

    #[test]
    fn test_config_default_values() {
        let cfg = LoopConfig::default();
        assert_eq!(cfg.window_size, 10);
        assert_eq!(cfg.repeat_threshold, 3);
        assert_eq!(cfg.min_output_len, 20);
    }

    #[test]
    fn test_config_custom_values() {
        let mut detector = LoopDetector::with_config(LoopConfig {
            window_size: 5,
            repeat_threshold: 2,
            min_output_len: 30,
            ..LoopConfig::default()
        });
        assert_eq!(detector.check("short"), LoopStatus::Ok);
        assert_eq!(detector.check("short"), LoopStatus::Ok);
        let long = "This output is long enough for custom config.";
        assert_eq!(detector.check(&long), LoopStatus::Ok);
        assert_eq!(detector.check(&long), LoopStatus::Ok);
        match detector.check(&long) {
            LoopStatus::LoopDetected { .. } => {}
            other => panic!("custom threshold should detect loop, got {other:?}"),
        }
    }

    #[test]
    fn test_case_insensitive_duplicate_detection() {
        let mut detector = LoopDetector::with_config(LoopConfig {
            repeat_threshold: 2,
            min_output_len: 10,
            ..LoopConfig::default()
        });
        detector.check("HELLO WORLD, this is a long enough output.");
        detector.check("hello world, this is a long enough output.");
        match detector.check("Hello\nWorld, this is a long enough output.") {
            LoopStatus::LoopDetected { .. } => {}
            other => panic!("expected loop, got {other:?}"),
        }
    }

    #[test]
    fn test_window_eviction_prevents_stale_loop() {
        let mut detector = LoopDetector::with_config(LoopConfig {
            window_size: 2,
            repeat_threshold: 2,
            min_output_len: 10,
            ..LoopConfig::default()
        });
        let repeated = "Repeated output long enough to count.";
        detector.check(repeated);
        detector.check(repeated);
        detector.check("Completely different output that is long.");
        detector.check("Another unique output that is still long.");
        assert_eq!(detector.check(repeated), LoopStatus::Ok);
    }

    #[test]
    fn test_halt_agent_on_severe_repetition() {
        let mut detector = LoopDetector::with_config(LoopConfig {
            window_size: 20,
            repeat_threshold: 2,
            min_output_len: 10,
            ..LoopConfig::default()
        });
        let repeated = "Severe repetition output for halt testing.";
        detector.check(repeated);
        detector.check(repeated);
        detector.check(repeated);
        detector.check(repeated);
        match detector.check(repeated) {
            LoopStatus::LoopDetected { action, .. } => {
                assert_eq!(action, LoopAction::HaltAgent);
            }
            other => panic!("expected halt, got {other:?}"),
        }
    }
}
