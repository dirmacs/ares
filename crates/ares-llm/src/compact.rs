//! History compaction behind any [`LLMClient`](crate::client::LLMClient).
//!
//! The doctrine is deliberate: **small context, frequent micro-calls**. Long
//! conversations are not fed back wholesale; instead a [`Compactor`] keeps a
//! bounded working set and spends tiny single-purpose calls to maintain it:
//!
//! 1. **Score** every recorded turn once, 1–5, with a fixed rubric prompt
//!    ("score low when unsure").
//! 2. **Audit** periodically: re-score anything unscored or weak in ONE
//!    batched call, hoist 5-tier turns into a VERBATIM critical-facts list,
//!    rebuild a short rolling memory from mid-tier facts, and evict
//!    low-value turns — never the newest [`CompactConfig::grace_turns`].
//! 3. **Build context**: callers get `[base, critical, memory, recent]`
//!    message pairs ready for `generate_with_history`.
//!
//! Every prompt is a `const` fixed template so provider-side prompt caches
//! see stable prefixes. Every LLM failure path degrades silently: state is
//! kept as-is and a [`CompactEvent::Skipped`] is returned instead of an
//! error — recording a turn must never fail because a scoring call did.

use std::sync::Arc;

use parking_lot::Mutex;

use serde_json::Value;

use crate::client::LLMClient;
use crate::micro::salvage_json;

/// Fixed rubric for the single-turn score call. Cache-stable by contract.
const SCORE_SYSTEM: &str = "You rate one conversation turn for long-term value. \
Reply with ONLY a JSON object {\"score\":N} where N is 1-5: \
5 = critical fact or decision worth keeping verbatim, \
4 = useful detail, \
3 = mild context value, \
2 = mostly filler, \
1 = worthless. \
Score low when unsure.";

/// Fixed rubric for the batched audit re-score call. Cache-stable by contract.
const AUDIT_SYSTEM: &str = "You re-rate conversation turns for long-term value. \
Each turn is prefixed with its sequence number [seq]. \
Reply with ONLY a JSON array [{\"seq\":N,\"score\":N}] covering EVERY listed seq, \
scores 1-5: \
5 = critical fact or decision worth keeping verbatim, \
4 = useful detail, \
3 = mild context value, \
2 = mostly filler, \
1 = worthless. \
Score low when unsure.";

/// Fixed instruction for the memory rebuild call. Output is plain text.
const MEMORY_SYSTEM: &str = "You compress raw conversation notes into a dense rolling summary. \
Keep only durable facts, decisions and preferences; drop filler. \
Preserve concrete names, numbers and dates. \
Output ONLY the summary text, nothing else.";

/// Score at or above this tier is hoisted verbatim into the critical list.
const S_TIER_SCORE: u8 = 5;
/// Lowest mid-tier score feeding the memory rebuild.
const MID_TIER_MIN: u8 = 3;
/// Highest mid-tier score feeding the memory rebuild.
const MID_TIER_MAX: u8 = 4;
/// Scores at or below this mark a turn as an eviction candidate.
const LOW_EVICT_SCORE: u8 = 2;

/// Tuning knobs for a [`Compactor`].
///
/// Defaults suit interactive chat; tighten `trigger_turns` for bursty
/// workloads, widen `grace_turns` when the newest exchanges must never be
/// judged prematurely.
#[derive(Debug, Clone)]
pub struct CompactConfig {
    /// Run an audit once this many NEW turns arrived since the last audit.
    pub trigger_turns: usize,
    /// Target size of the live (in-context) history; an audit also fires
    /// when live turns exceed this capacity.
    pub history_turns: usize,
    /// The newest this many turns are NEVER evicted, regardless of score.
    pub grace_turns: usize,
    /// Hard character ceiling for the rebuilt memory string.
    pub memory_max_chars: usize,
    /// Maximum number of verbatim critical facts kept; overflow drops the
    /// oldest admitted item (all admitted items scored 5, so age breaks ties).
    pub critical_max_items: usize,
    /// Turns scoring at or below this are re-scored during audits and count
    /// as eviction candidates when they land at or below 2.
    pub critical_reaudit: usize,
}

impl Default for CompactConfig {
    fn default() -> Self {
        Self {
            trigger_turns: 6,
            history_turns: 6,
            grace_turns: 3,
            memory_max_chars: 500,
            critical_max_items: 8,
            critical_reaudit: 6,
        }
    }
}

/// One recorded conversation turn with its audited importance score.
#[derive(Debug, Clone)]
pub struct TurnEntry {
    /// Monotonic sequence number, starting at 1.
    pub seq: u64,
    /// The user side of the turn.
    pub user: String,
    /// The assistant side of the turn.
    pub assistant: String,
    /// Rubric score 1–5; `None` while unscored or when scoring failed.
    pub score: Option<u8>,
}

/// Internal compaction state guarded by the [`Compactor`] mutex.
#[derive(Debug, Default)]
pub struct CompactionState {
    entries: Vec<TurnEntry>,
    critical: Vec<String>,
    memory: String,
    last_audit_seq: u64,
}

impl CompactionState {
    /// Next sequence number (last entry + 1, or 1 when empty).
    fn next_seq(&self) -> u64 {
        self.entries.last().map(|e| e.seq + 1).unwrap_or(1)
    }

    /// Number of NEW turns since the last audit finished.
    fn turns_since_audit(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.seq > self.last_audit_seq)
            .count()
    }

    fn apply_score(&mut self, seq: u64, score: u8) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.seq == seq) {
            entry.score = Some(score);
        }
    }

    fn apply_scores(&mut self, updates: &[(u64, u8)]) {
        for (seq, score) in updates {
            self.apply_score(*seq, *score);
        }
    }
}

/// Counted state for tests and admin surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionSnapshot {
    /// Live turns currently held in history.
    pub turn_count: usize,
    /// Live turns carrying a score.
    pub scored_count: usize,
    /// Verbatim critical facts kept.
    pub critical_count: usize,
    /// Character length of the rolling memory.
    pub memory_chars: usize,
    /// Sequence number the last completed audit covered up to.
    pub last_audit_seq: u64,
}

/// Outcome of one compactor operation. Never an error: LLM trouble
/// degrades to [`CompactEvent::Skipped`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactEvent {
    /// A recorded turn was scored.
    Scored {
        /// Sequence number of the scored turn.
        seq: u64,
        /// Clamped rubric score, 1–5.
        score: u8,
    },
    /// An audit pass completed.
    Audited {
        /// Size of the critical list after the audit.
        critical_kept: usize,
        /// Character length of the memory string after the audit.
        memory_chars: usize,
        /// Sequence numbers evicted from live history.
        dropped_seqs: Vec<u64>,
    },
    /// The operation degraded silently; prior state is untouched.
    Skipped {
        /// Machine-readable reason (`score-parse`, `score-call`,
        /// `audit-call`, `audit-parse`, `not-due`).
        reason: &'static str,
    },
}

/// History compaction service running small frequent micro-calls over one
/// shared client. See the [module docs](self) for the pipeline.
pub struct Compactor {
    config: CompactConfig,
    client: Arc<dyn LLMClient>,
    state: Mutex<CompactionState>,
}

impl Compactor {
    /// Creates a compactor over `client` with `config`.
    pub fn new(config: CompactConfig, client: Arc<dyn LLMClient>) -> Self {
        Self {
            config,
            client,
            state: Mutex::new(CompactionState::default()),
        }
    }

    /// Creates a compactor over `client` with default [`CompactConfig`].
    pub fn with_client(client: Arc<dyn LLMClient>) -> Self {
        Self::new(CompactConfig::default(), client)
    }

    /// Records one user/assistant turn and scores THE PAIR with a single
    /// micro-call.
    ///
    /// A transport failure yields [`CompactEvent::Skipped`] with reason
    /// `score-call`; an unparseable reply yields `score-parse`. Either way
    /// the turn is stored unscored and the caller never sees an error.
    pub async fn record_turn(&self, user: String, assistant: String) -> CompactEvent {
        let seq = {
            let mut state = self.lock();
            let seq = state.next_seq();
            state.entries.push(TurnEntry {
                seq,
                user: user.clone(),
                assistant: assistant.clone(),
                score: None,
            });
            seq
        };

        let input = format!("user: {}\nassistant: {}", user, assistant);
        let Ok(text) = self.client.generate_with_system(SCORE_SYSTEM, &input).await else {
            return CompactEvent::Skipped {
                reason: "score-call",
            };
        };
        match parse_score(&text) {
            Some(score) => {
                self.lock().apply_score(seq, score);
                CompactEvent::Scored { seq, score }
            }
            None => CompactEvent::Skipped {
                reason: "score-parse",
            },
        }
    }

    /// Runs an audit pass when due: enough new turns arrived
    /// ([`CompactConfig::trigger_turns`]) or live history exceeds
    /// [`CompactConfig::history_turns`].
    ///
    /// One batched call re-scores every unscored or weak turn; then S-tier
    /// (5) turns are hoisted VERBATIM into the deduplicated critical list,
    /// the rolling memory is rebuilt from mid-tier facts in one more call
    /// (skipped when there is nothing new to summarize), and low-value turns
    /// outside the grace window are evicted. Returns a single-element
    /// vector: [`CompactEvent::Audited`] on success or
    /// [`CompactEvent::Skipped`] (`not-due`, `audit-call`, `audit-parse`).
    pub async fn audit_if_due(&self) -> Vec<CompactEvent> {
        let (due, candidates) = {
            let state = self.lock();
            let due = state.turns_since_audit() >= self.config.trigger_turns
                || state.entries.len() > self.config.history_turns;
            let candidates: Vec<TurnEntry> = state
                .entries
                .iter()
                .filter(|e| {
                    e.score.is_none() || e.score <= Some(self.config.critical_reaudit as u8)
                })
                .cloned()
                .collect();
            (due, candidates)
        };
        if !due {
            return vec![CompactEvent::Skipped { reason: "not-due" }];
        }

        // One batched re-score call over all unscored/low turns.
        let listing = candidates
            .iter()
            .map(|e| format!("[{}] user: {}\nassistant: {}", e.seq, e.user, e.assistant))
            .collect::<Vec<_>>()
            .join("\n---\n");
        let Ok(text) = self
            .client
            .generate_with_system(AUDIT_SYSTEM, &listing)
            .await
        else {
            return vec![CompactEvent::Skipped {
                reason: "audit-call",
            }];
        };
        let updates = parse_audit_scores(&text);
        if updates.is_empty() {
            return vec![CompactEvent::Skipped {
                reason: "audit-parse",
            }];
        }

        // Structural pass under one short-lived lock.
        let (dropped_seqs, mid_facts, previous_memory) = {
            let mut state = self.lock();
            state.apply_scores(&updates);

            // Newest grace_turns entries are untouchable, whatever they
            // scored; older low-scorers leave live history.
            let keep_from = state.entries.len().saturating_sub(self.config.grace_turns);
            let mut dropped_seqs = Vec::new();
            let mut kept: Vec<TurnEntry> = Vec::with_capacity(state.entries.len());
            for (position, entry) in state.entries.drain(..).enumerate() {
                let low = matches!(entry.score, Some(score) if score <= LOW_EVICT_SCORE);
                let protected = position >= keep_from;
                if protected || !low {
                    kept.push(entry);
                } else {
                    dropped_seqs.push(entry.seq);
                }
            }
            state.entries = kept;

            // Hoist S-tier turns VERBATIM into the critical list, dedup by
            // content. Scanning AFTER eviction is safe: a 5-scored entry is
            // never evicted (only scores <= 2 leave), so nothing is missed.
            let s_tier_items: Vec<String> = state
                .entries
                .iter()
                .filter(|entry| entry.score == Some(S_TIER_SCORE))
                .map(critical_item_text)
                .collect();
            for item in s_tier_items {
                if !state.critical.contains(&item) {
                    state.critical.push(item);
                }
            }
            while state.critical.len() > self.config.critical_max_items {
                state.critical.remove(0);
            }

            // Mid-tier facts inside the pre-grace window feed the memory
            // rebuild; the previous summary rides along as context.
            let window_end = state.entries.len().saturating_sub(self.config.grace_turns);
            let mid_facts: Vec<String> = state
                .entries
                .iter()
                .take(window_end)
                .filter(|e| {
                    matches!(e.score, Some(score) if (MID_TIER_MIN..=MID_TIER_MAX).contains(&score))
                })
                .map(mid_fact_text)
                .collect();
            let previous_memory = state.memory.clone();

            state.last_audit_seq = state
                .entries
                .last()
                .map(|e| e.seq)
                .unwrap_or(state.last_audit_seq);
            (dropped_seqs, mid_facts, previous_memory)
        };

        // Memory rebuild runs OUTSIDE the state lock; on failure or an empty
        // reply the previous memory simply stays.
        if !mid_facts.is_empty() {
            let input = format!(
                "Previous summary:\n{}\n\nNew notes:\n{}",
                previous_memory,
                mid_facts.join("\n")
            );
            if let Ok(summary) = self
                .client
                .generate_with_system(MEMORY_SYSTEM, &input)
                .await
            {
                let trimmed = summary.trim();
                if !trimmed.is_empty() {
                    self.lock().memory = truncate_chars(trimmed, self.config.memory_max_chars);
                }
            }
        }

        let event = {
            let state = self.lock();
            CompactEvent::Audited {
                critical_kept: state.critical.len(),
                memory_chars: state.memory.chars().count(),
                dropped_seqs,
            }
        };
        vec![event]
    }

    /// Builds message pairs ready for `generate_with_history` callers:
    /// `[base, critical, memory, recent turns]`, in that order. The critical
    /// and memory slots appear only when non-empty; `recent_window` bounds
    /// how many newest turns ride along.
    pub fn build_context(&self, base: &str, recent_window: usize) -> Vec<(String, String)> {
        let state = self.lock();
        let mut messages = vec![("system".to_string(), base.to_string())];
        if !state.critical.is_empty() {
            messages.push((
                "system".to_string(),
                format!(
                    "Critical facts to preserve verbatim:\n{}",
                    state
                        .critical
                        .iter()
                        .map(|item| format!("- {}", item))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            ));
        }
        if !state.memory.is_empty() {
            messages.push((
                "system".to_string(),
                format!("Conversation memory summary:\n{}", state.memory),
            ));
        }
        let start = state.entries.len().saturating_sub(recent_window);
        for entry in &state.entries[start..] {
            messages.push(("user".to_string(), entry.user.clone()));
            messages.push(("assistant".to_string(), entry.assistant.clone()));
        }
        messages
    }

    /// Counted view of the current state for tests and admin surfaces.
    pub fn state_snapshot(&self) -> CompactionSnapshot {
        let state = self.lock();
        CompactionSnapshot {
            turn_count: state.entries.len(),
            scored_count: state.entries.iter().filter(|e| e.score.is_some()).count(),
            critical_count: state.critical.len(),
            memory_chars: state.memory.chars().count(),
            last_audit_seq: state.last_audit_seq,
        }
    }

    /// Locks the state. `parking_lot` guards cannot poison, so a panic in
    /// some other holder can never wedge turn recording.
    fn lock(&self) -> parking_lot::MutexGuard<'_, CompactionState> {
        self.state.lock()
    }
}

/// Verbatim critical item text for a 5-tier turn.
fn critical_item_text(entry: &TurnEntry) -> String {
    format!("user: {}\nassistant: {}", entry.user, entry.assistant)
}

/// Compressed fact text feeding the memory rebuild for a mid-tier turn.
fn mid_fact_text(entry: &TurnEntry) -> String {
    format!(
        "[{}] user: {}; assistant: {}",
        entry.seq, entry.user, entry.assistant
    )
}

/// Truncates to at most `max` chars without splitting a character.
fn truncate_chars(text: &str, max: usize) -> String {
    text.char_indices()
        .nth(max)
        .map_or_else(|| text.to_string(), |(idx, _)| text[..idx].to_string())
}

/// Parses a tolerant 1–5 score out of model output (any stage of salvage).
fn parse_score(text: &str) -> Option<u8> {
    let value = salvage_json(text)?;
    let raw = value.get("score").and_then(|field| {
        field.as_i64().or_else(|| {
            field
                .as_str()
                .and_then(|string| string.trim().parse::<i64>().ok())
        })
    })?;
    Some(raw.clamp(1, 5) as u8)
}

/// Parses `[{seq,score}]` audit replies; malformed rows are skipped, valid
/// scores are clamped to 1–5. Empty result means the whole reply was unusable.
fn parse_audit_scores(text: &str) -> Vec<(u64, u8)> {
    let Some(value) = salvage_json(text) else {
        return Vec::new();
    };
    let rows: Vec<Value> = match value {
        Value::Array(rows) => rows,
        Value::Object(_) => vec![value],
        _ => return Vec::new(),
    };
    rows.iter()
        .filter_map(|row| {
            let seq = row.get("seq")?.as_u64()?;
            let score = row.get("score")?.as_i64()?;
            Some((seq, score.clamp(1, 5) as u8))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::types::{AppError, Result};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Step = std::result::Result<String, AppError>;

    /// Client scripting `generate_with_system` replies by call index and
    /// counting calls; every other trait method fails as unused.
    struct ScriptedClient {
        replies: Box<dyn Fn(usize) -> Step + Send + Sync>,
        calls: AtomicUsize,
    }

    impl ScriptedClient {
        fn new<F>(replies: F) -> Self
        where
            F: Fn(usize) -> Step + Send + Sync + 'static,
        {
            Self {
                replies: Box::new(replies),
                calls: AtomicUsize::new(0),
            }
        }

        fn call_index(&self) -> usize {
            self.calls.fetch_add(1, Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LLMClient for ScriptedClient {
        async fn generate(&self, _prompt: &str) -> Result<String> {
            Err(AppError::Internal("unused".into()))
        }

        async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
            (self.replies)(self.call_index())
        }

        async fn generate_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<crate::client::LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn generate_with_tools(
            &self,
            _prompt: &str,
            _tools: &[ares_types::types::ToolDefinition],
        ) -> Result<crate::client::LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn generate_with_tools_and_history(
            &self,
            _messages: &[crate::coordinator::ConversationMessage],
            _tools: &[ares_types::types::ToolDefinition],
        ) -> Result<crate::client::LLMResponse> {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream(
            &self,
            _prompt: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("unused".into()))
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<Box<dyn futures::Stream<Item = Result<String>> + Send + Unpin>> {
            Err(AppError::Internal("unused".into()))
        }

        fn model_name(&self) -> &str {
            "compact-scripted-mock"
        }
    }

    fn config() -> CompactConfig {
        CompactConfig {
            trigger_turns: 1,
            history_turns: 16,
            grace_turns: 1,
            memory_max_chars: 500,
            critical_max_items: 8,
            critical_reaudit: 6,
        }
    }

    #[tokio::test]
    async fn record_turn_scores_the_pair() {
        let client = Arc::new(ScriptedClient::new(|_| Ok("{\"score\": 4}".into())));
        let compactor = Compactor::with_client(client);

        let event = compactor
            .record_turn("what is ares?".to_string(), "an api gateway".to_string())
            .await;

        assert_eq!(event, CompactEvent::Scored { seq: 1, score: 4 });
        let snapshot = compactor.state_snapshot();
        assert_eq!(snapshot.turn_count, 1);
        assert_eq!(snapshot.scored_count, 1);
        assert_eq!(snapshot.last_audit_seq, 0, "scoring alone is not an audit");
    }

    #[tokio::test]
    async fn record_turn_parse_failure_is_skipped_not_err() {
        let client = Arc::new(ScriptedClient::new(|_| Ok("no json at all".into())));
        let compactor = Compactor::with_client(client);

        let event = compactor
            .record_turn("u".to_string(), "a".to_string())
            .await;

        assert_eq!(
            event,
            CompactEvent::Skipped {
                reason: "score-parse"
            }
        );
        let snapshot = compactor.state_snapshot();
        assert_eq!(snapshot.turn_count, 1, "turn is kept unscored");
        assert_eq!(snapshot.scored_count, 0);
    }

    #[tokio::test]
    async fn record_turn_transport_failure_is_skipped_not_err() {
        let client = Arc::new(ScriptedClient::new(|_| {
            Err(AppError::External("down".into()))
        }));
        let compactor = Compactor::with_client(client);

        let event = compactor
            .record_turn("u".to_string(), "a".to_string())
            .await;

        assert_eq!(
            event,
            CompactEvent::Skipped {
                reason: "score-call"
            }
        );
        assert_eq!(compactor.state_snapshot().turn_count, 1);
    }

    #[tokio::test]
    async fn audit_hoists_s_tier_verbatim_and_evicts_low_outside_grace() {
        // Calls 0..3 score four turns; call 4 is the batched audit re-score;
        // call 5 is the memory rebuild (one mid-tier survivor exists).
        let client = Arc::new(ScriptedClient::new(|call| {
            match call {
            0..=3 => Ok(format!("{{\"score\":{}}}", [5, 1, 4, 1][call])),
            4 => Ok("[{\"seq\":1,\"score\":5},{\"seq\":2,\"score\":1},{\"seq\":3,\"score\":4},{\"seq\":4,\"score\":1}]".into()),
            5 => Ok("likes rust; dislikes yaml".into()),
            _ => Err(AppError::Internal("unexpected call".into())),
        }
        }));
        let compactor = Compactor::new(config(), client);

        for index in 0..4u64 {
            let event = compactor
                .record_turn(format!("u{}", index), format!("a{}", index))
                .await;
            assert!(
                matches!(event, CompactEvent::Scored { .. }),
                "seed turn {} should score",
                index
            );
        }

        let events = compactor.audit_if_due().await;
        assert_eq!(events.len(), 1);
        // Seq 2 (score 1) is evicted. Seq 4 ALSO scored 1 but sits inside the
        // newest-entry grace window, so it survives despite its score.
        assert_eq!(
            events[0],
            CompactEvent::Audited {
                critical_kept: 1,
                memory_chars: "likes rust; dislikes yaml".chars().count(),
                dropped_seqs: vec![2],
            }
        );

        let snapshot = compactor.state_snapshot();
        assert_eq!(snapshot.turn_count, 3, "seq 2 evicted; 1, 3, 4 stay");
        assert_eq!(snapshot.critical_count, 1);
        assert_eq!(snapshot.last_audit_seq, 4);

        // Grace window, isolated: a single low-scored turn is the newest
        // entry, so an audit must NOT evict it.
        let client = Arc::new(ScriptedClient::new(|call| match call {
            0 => Ok("{\"score\":1}".into()),
            1 => Ok("[{\"seq\":5,\"score\":1}]".into()),
            _ => Err(AppError::Internal("unexpected call".into())),
        }));
        let compactor = Compactor::new(config(), client);
        compactor
            .record_turn("fresh".to_string(), "low value".to_string())
            .await;
        let events = compactor.audit_if_due().await;
        assert_eq!(
            events[0],
            CompactEvent::Audited {
                critical_kept: 0,
                memory_chars: 0,
                dropped_seqs: vec![],
            },
            "newest grace_turns entry survives despite score 1"
        );
    }

    #[tokio::test]
    async fn audit_memory_failure_keeps_previous_memory() {
        // Two seeded turns put a mid-tier fact inside the pre-grace window so
        // the first audit rebuilds memory; the SECOND audit's memory call
        // fails, and the previous summary must survive untouched.
        let client = Arc::new(ScriptedClient::new(|call| match call {
            0..=1 => Ok("{\"score\":3}".into()),
            2 => Ok("[{\"seq\":1,\"score\":3},{\"seq\":2,\"score\":3}]".into()),
            3 => Ok("first summary".into()),
            4 => Ok("{\"score\":3}".into()),
            5 => Ok("[{\"seq\":3,\"score\":3}]".into()),
            _ => Err(AppError::External("memory call down".into())),
        }));
        let compactor = Compactor::new(config(), client);

        compactor.record_turn("u1".into(), "a1".into()).await;
        compactor.record_turn("u2".into(), "a2".into()).await;
        let first = compactor.audit_if_due().await;
        assert_eq!(
            first[0],
            CompactEvent::Audited {
                critical_kept: 0,
                memory_chars: "first summary".chars().count(),
                dropped_seqs: vec![],
            }
        );

        compactor.record_turn("u3".into(), "a3".into()).await;
        let second = compactor.audit_if_due().await;
        assert!(matches!(second[0], CompactEvent::Audited { .. }));
        let snapshot = compactor.state_snapshot();
        assert_eq!(
            snapshot.memory_chars,
            "first summary".chars().count(),
            "failed rebuild falls back to previous memory"
        );
    }

    #[tokio::test]
    async fn audit_call_failures_degrade_to_skipped() {
        // Audit re-score transport failure...
        let failing = Arc::new(ScriptedClient::new(|call| match call {
            0 => Ok("{\"score\":1}".into()),
            _ => Err(AppError::External("audit down".into())),
        }));
        let compactor = Compactor::new(config(), failing);
        compactor.record_turn("u".into(), "a".into()).await;
        assert_eq!(
            compactor.audit_if_due().await,
            vec![CompactEvent::Skipped {
                reason: "audit-call"
            }]
        );

        // ...and unparseable audit reply.
        let garbage = Arc::new(ScriptedClient::new(|call| match call {
            0 => Ok("{\"score\":1}".into()),
            1 => Ok("total gibberish".into()),
            _ => Err(AppError::Internal("unexpected call".into())),
        }));
        let compactor = Compactor::new(config(), garbage);
        compactor.record_turn("u".into(), "a".into()).await;
        assert_eq!(
            compactor.audit_if_due().await,
            vec![CompactEvent::Skipped {
                reason: "audit-parse"
            }]
        );
        let snapshot = compactor.state_snapshot();
        assert_eq!(snapshot.turn_count, 1, "skipped audits keep state intact");
        assert_eq!(snapshot.last_audit_seq, 0);
    }

    #[tokio::test]
    async fn audit_skips_when_not_due() {
        let client = Arc::new(ScriptedClient::new(|_| {
            Err(AppError::Internal("no calls expected".into()))
        }));
        let quiet = CompactConfig {
            trigger_turns: 100,
            history_turns: 100,
            ..config()
        };
        let compactor = Compactor::new(quiet, client);

        assert_eq!(
            compactor.audit_if_due().await,
            vec![CompactEvent::Skipped { reason: "not-due" }]
        );
    }

    #[tokio::test]
    async fn build_context_orders_base_critical_memory_recent() {
        // Three turns: seq1 mid-tier (feeds memory), seq2 S-tier (verbatim
        // critical), seq3 low but inside the newest-entry grace window.
        let client = Arc::new(ScriptedClient::new(|call| match call {
            0..=2 => Ok(format!("{{\"score\":{}}}", [4, 5, 2][call])),
            3 => Ok(
                "[{\"seq\":1,\"score\":4},{\"seq\":2,\"score\":5},{\"seq\":3,\"score\":2}]".into(),
            ),
            4 => Ok("she prefers dark mode".into()),
            _ => Err(AppError::Internal("unexpected call".into())),
        }));
        let compactor = Compactor::new(config(), client);
        compactor
            .record_turn("theme?".into(), "dark mode".into())
            .await;
        compactor.record_turn("stack?".into(), "rust".into()).await;
        compactor.record_turn("tabs?".into(), "spaces".into()).await;
        compactor.audit_if_due().await;

        let messages = compactor.build_context("You are helpful.", 8);

        assert_eq!(messages.len(), 9, "base + critical + memory + 3 turns x2");
        assert_eq!(
            messages[0],
            ("system".to_string(), "You are helpful.".to_string())
        );
        assert_eq!(messages[1].0, "system");
        assert!(
            messages[1].1.contains("Critical facts") && messages[1].1.contains("user: stack?"),
            "critical slot comes right after base and holds the S-tier pair"
        );
        assert_eq!(messages[2].0, "system");
        assert!(
            messages[2].1.contains("Conversation memory summary")
                && messages[2].1.contains("she prefers dark mode"),
            "memory slot follows critical"
        );
        assert_eq!(messages[3], ("user".to_string(), "theme?".to_string()));
        assert_eq!(
            messages[4],
            ("assistant".to_string(), "dark mode".to_string())
        );
        assert_eq!(messages[5], ("user".to_string(), "stack?".to_string()));
        assert_eq!(messages[6], ("assistant".to_string(), "rust".to_string()));
        assert_eq!(messages[7], ("user".to_string(), "tabs?".to_string()));
        assert_eq!(messages[8], ("assistant".to_string(), "spaces".to_string()));

        // A tight recent window trims oldest turns but keeps the prefix.
        let trimmed = compactor.build_context("base", 1);
        assert_eq!(trimmed.len(), 5, "base + critical + memory + 1 turn x2");
        assert_eq!(trimmed[3], ("user".to_string(), "tabs?".to_string()));
        assert_eq!(trimmed[4], ("assistant".to_string(), "spaces".to_string()));

        // Empty-state compactor emits just the base message.
        let bare = Compactor::with_client(Arc::new(ScriptedClient::new(|_| {
            Err(AppError::Internal("unused".into()))
        })));
        assert_eq!(
            bare.build_context("only", 4),
            vec![("system".to_string(), "only".to_string())]
        );
    }

    #[test]
    fn parse_score_clamps_and_tolerates_strings() {
        assert_eq!(parse_score("{\"score\":4}"), Some(4));
        assert_eq!(parse_score("Sure! {\"score\":\"9\"}"), Some(5));
        assert_eq!(parse_score("{\"score\":0}"), Some(1));
        assert_eq!(parse_score("garbage"), None);
    }

    #[test]
    fn truncate_chars_respects_boundaries() {
        assert_eq!(truncate_chars("hello", 50), "hello");
        assert_eq!(truncate_chars("héllo", 2), "hé");
    }
}
