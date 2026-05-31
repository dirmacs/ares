//! Loop detection for ARES agents.
//!
//! Detects when an agent is producing repetitive outputs and intervenes
//! to break the loop. Uses a sliding window of recent outputs with
//! similarity hashing.

use std::collections::VecDeque;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Configuration for loop detection.
#[derive(Clone, Debug)]
pub struct LoopDetectorConfig {
    /// Maximum number of recent outputs to track
    pub window_size: usize,
    /// Number of identical hashes that trigger loop detection
    pub repeat_threshold: usize,
    /// Minimum output length to consider for loop detection
    pub min_output_len: usize,
}

impl Default for LoopDetectorConfig {
    fn default() -> Self {
        Self {
            window_size: 10,
            repeat_threshold: 3,
            min_output_len: 20,
        }
    }
}

/// Tracks agent outputs and detects repetitive loops.
#[derive(Clone, Debug)]
pub struct LoopDetector {
    config: LoopDetectorConfig,
    /// Sliding window of output hashes
    output_hashes: VecDeque<u64>,
    /// Total outputs processed
    total_outputs: usize,
    /// Number of loops detected
    loops_detected: usize,
}

/// Result of checking an output for loops.
#[derive(Clone, Debug, PartialEq)]
pub enum LoopStatus {
    /// No loop detected, proceed normally
    Ok,
    /// Loop detected — agent is repeating itself
    LoopDetected {
        /// Number of consecutive repeats
        repeats: usize,
        /// Suggested action
        action: LoopAction,
    },
}

/// Actions to take when a loop is detected.
#[derive(Clone, Debug, PartialEq)]
pub enum LoopAction {
    /// Inject a "you are repeating yourself" prompt
    InjectWarning,
    /// Force the agent to try a different approach
    ForceAlternative,
    /// Stop the agent entirely
    HaltAgent,
}

impl LoopDetector {
    /// Create a new loop detector with default config.
    pub fn new() -> Self {
        Self::with_config(LoopDetectorConfig::default())
    }

    /// Create a new loop detector with custom config.
    pub fn with_config(config: LoopDetectorConfig) -> Self {
        Self {
            output_hashes: VecDeque::with_capacity(config.window_size),
            config,
            total_outputs: 0,
            loops_detected: 0,
        }
    }

    /// Hash an output string (first 500 chars, normalized).
    fn hash_output(output: &str) -> u64 {
        let normalized = output
            .chars()
            .take(500)
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_lowercase();
        let mut hasher = DefaultHasher::new();
        normalized.hash(&mut hasher);
        hasher.finish()
    }

    /// Check if the given output indicates a loop.
    pub fn check(&mut self, output: &str) -> LoopStatus {
        self.total_outputs += 1;

        // Skip very short outputs
        if output.len() < self.config.min_output_len {
            return LoopStatus::Ok;
        }

        let hash = Self::hash_output(output);

        // Count how many recent outputs have the same hash
        let repeats = self.output_hashes.iter().filter(|&&h| h == hash).count();

        // Add to window (sliding)
        if self.output_hashes.len() >= self.config.window_size {
            self.output_hashes.pop_front();
        }
        self.output_hashes.push_back(hash);

        if repeats >= self.config.repeat_threshold {
            self.loops_detected += 1;
            let action = if repeats >= self.config.repeat_threshold * 2 {
                LoopAction::HaltAgent
            } else if repeats >= self.config.repeat_threshold + 1 {
                LoopAction::ForceAlternative
            } else {
                LoopAction::InjectWarning
            };
            LoopStatus::LoopDetected { repeats, action }
        } else {
            LoopStatus::Ok
        }
    }

    /// Reset the detector (e.g., on new conversation).
    pub fn reset(&mut self) {
        self.output_hashes.clear();
        self.total_outputs = 0;
    }

    /// Get statistics.
    pub fn stats(&self) -> (usize, usize) {
        (self.total_outputs, self.loops_detected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // 4th repeat should trigger (3 previous matches in window)
        match detector.check(repeated) {
            LoopStatus::LoopDetected { repeats, action } => {
                assert!(repeats >= 3);
                assert_eq!(action, LoopAction::InjectWarning);
            }
            _ => panic!("should detect loop"),
        }
    }

    #[test]
    fn test_short_output_ignored() {
        let mut detector = LoopDetector::new();
        // Short outputs shouldn't trigger loop detection
        assert_eq!(detector.check("ok"), LoopStatus::Ok);
        assert_eq!(detector.check("ok"), LoopStatus::Ok);
        assert_eq!(detector.check("ok"), LoopStatus::Ok);
        assert_eq!(detector.check("ok"), LoopStatus::Ok);
    }

    #[test]
    fn test_escalation() {
        let mut detector = LoopDetector::with_config(LoopDetectorConfig {
            window_size: 20,
            repeat_threshold: 2,
            min_output_len: 10,
        });
        let repeated = "This is a repeated response that keeps coming back.";
        detector.check(repeated); // 1
        detector.check(repeated); // 2
        // 3rd should warn
        match detector.check(repeated) {
            LoopStatus::LoopDetected { action, .. } => assert_eq!(action, LoopAction::InjectWarning),
            _ => panic!("should warn"),
        }
        // 4th should force alternative
        match detector.check(repeated) {
            LoopStatus::LoopDetected { action, .. } => assert_eq!(action, LoopAction::ForceAlternative),
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
        // After reset, same output should not trigger
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
        let mut detector = LoopDetector::with_config(LoopDetectorConfig {
            repeat_threshold: 2,
            ..Default::default()
        });
        // Same content with different whitespace should match
        detector.check("Hello   world,  how are you doing today?");
        detector.check("Hello world, how are you doing today?");
        match detector.check("Hello\n\tworld,\thow are you doing today?") {
            LoopStatus::LoopDetected { .. } => {} // expected
            _ => panic!("whitespace-normalized duplicates should match"),
        }
    }
    #[test]
    fn test_config_default_values() {
        let cfg = LoopDetectorConfig::default();
        assert_eq!(cfg.window_size, 10);
        assert_eq!(cfg.repeat_threshold, 3);
        assert_eq!(cfg.min_output_len, 20);
    }

    #[test]
    fn test_config_custom_values() {
        let mut detector = LoopDetector::with_config(LoopDetectorConfig {
            window_size: 5,
            repeat_threshold: 2,
            min_output_len: 30,
        });
        // Outputs shorter than min_output_len are ignored
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
        let mut detector = LoopDetector::with_config(LoopDetectorConfig {
            repeat_threshold: 2,
            min_output_len: 10,
            ..Default::default()
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
        let mut detector = LoopDetector::with_config(LoopDetectorConfig {
            window_size: 2,
            repeat_threshold: 2,
            min_output_len: 10,
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
        let mut detector = LoopDetector::with_config(LoopDetectorConfig {
            window_size: 20,
            repeat_threshold: 2,
            min_output_len: 10,
        });
        let repeated = "Severe repetition output for halt testing.";
        detector.check(repeated);
        detector.check(repeated);
        detector.check(repeated); // warn
        detector.check(repeated); // force alternative
        match detector.check(repeated) {
            LoopStatus::LoopDetected { action, .. } => {
                assert_eq!(action, LoopAction::HaltAgent);
            }
            other => panic!("expected halt, got {other:?}"),
        }
    }

}
