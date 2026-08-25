//! Per-provider dispatch admission control (`max_in_flight`).
//!
//! Two different throttles are deliberately kept apart:
//!
//! - **WHO may call** — tenant-level admission (quota, tier policy). That is
//!   an authorization question and lives with request context, not here.
//! - **HOW MUCH a backend absorbs** — per-provider in-flight permits. A
//!   single misbehaving upstream should not collect unbounded concurrent
//!   requests; excess callers queue here instead of piling onto the wire.
//!
//! [`ProviderGovernor`] is the HOW-MUCH half: one semaphore per provider,
//! sized by the optional `max_in_flight` setting. When the setting is absent
//! (the default) admission is unlimited and governed wrappers are never
//! installed, preserving current behavior bit-for-bit.
//!
//! A permit is acquired immediately before dispatch begins and released only
//! after the terminal outcome: for unary calls when the response (or error)
//! returns, and for streaming calls when the stream terminates — the permit
//! rides on the stream itself, so it spans the full body, not just the
//! handshake. Because the permit lives inside the client wrapper rather than
//! a checkout guard, callers that move the client elsewhere stay correctly
//! governed.
//!
//! Configuration surfaces on the pool config:
//!
//! ```ignore
//! [Llm.config.pool.governor]
//! max_in_flight = 4
//! ```

use crate::client::{LLMClient, LLMResponse};
use ares_types::types::{AppError, Result, ToolDefinition};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Backing semaphore size for unlimited governors. Real limits are far
/// smaller; `None` simply means "never contended".
const MAX_PERMITS: usize = usize::MAX >> 4;

/// Tunables for one provider's in-flight governor.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GovernorConfig {
    /// Maximum simultaneous dispatches admitted to this provider.
    /// `None` (default) disables governing entirely.
    pub max_in_flight: Option<usize>,
    /// How long a dispatch may wait for an in-flight slot before failing
    /// closed (default: 30 seconds).
    #[serde(default = "default_acquire_timeout")]
    pub acquire_timeout: Duration,
}

fn default_acquire_timeout() -> Duration {
    Duration::from_secs(30)
}

impl Default for GovernorConfig {
    fn default() -> Self {
        Self {
            max_in_flight: None,
            acquire_timeout: default_acquire_timeout(),
        }
    }
}

impl GovernorConfig {
    /// Enable a hard cap of `max` concurrent dispatches.
    pub fn with_max_in_flight(mut self, max: usize) -> Self {
        self.max_in_flight = Some(max);
        self
    }
}

/// One admission outcome for a single dispatch.
#[derive(Debug)]
enum Admission {
    /// A held in-flight slot; drops when the dispatch reaches its terminal
    /// outcome. The permit is never read — holding it IS the admission.
    #[allow(dead_code)]
    Held(OwnedSemaphorePermit),
    /// Governing disabled — nothing to hold or release.
    Unlimited,
}

/// Semaphore-per-provider admission control.
///
/// Shared by every client checkout for one provider; cheap to clone via
/// `Arc`.
pub struct ProviderGovernor {
    max_in_flight: Option<usize>,
    acquire_timeout: Duration,
    semaphore: Arc<Semaphore>,
}

impl std::fmt::Debug for ProviderGovernor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderGovernor")
            .field("max_in_flight", &self.max_in_flight)
            .field("acquire_timeout", &self.acquire_timeout)
            .finish()
    }
}

impl ProviderGovernor {
    /// Build a governor from its configuration.
    pub fn new(config: GovernorConfig) -> Self {
        let GovernorConfig {
            max_in_flight,
            acquire_timeout,
        } = config;
        let permits = max_in_flight.unwrap_or(MAX_PERMITS);
        Self {
            max_in_flight,
            acquire_timeout,
            semaphore: Arc::new(Semaphore::new(permits)),
        }
    }

    /// Configured cap, if any.
    pub fn max_in_flight(&self) -> Option<usize> {
        self.max_in_flight
    }

    /// Permits currently available (test/introspection aid).
    #[cfg(test)]
    pub(crate) fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// Admit one dispatch. Fails closed: when the cap is saturated the
    /// caller waits up to `acquire_timeout`, then receives an error instead
    /// of adding load to the provider.
    async fn admit(&self) -> Result<Admission> {
        let Some(_) = self.max_in_flight else {
            return Ok(Admission::Unlimited);
        };
        let wait = self.semaphore.clone().acquire_owned();
        match tokio::time::timeout(self.acquire_timeout, wait).await {
            Ok(Ok(permit)) => Ok(Admission::Held(permit)),
            Ok(Err(_closed)) => Err(AppError::LLM(
                "provider governor closed; no in-flight slots can be issued".into(),
            )),
            Err(_elapsed) => Err(AppError::LLM(format!(
                "provider busy: waited over {}ms for an in-flight slot",
                self.acquire_timeout.as_millis()
            ))),
        }
    }

    /// Install a governed wrapper around `client` when a cap is configured;
    /// otherwise return the client untouched so unlimited deployments keep
    /// the exact prior behavior and allocation profile.
    pub(crate) fn wrap_if_limited(&self, client: Box<dyn LLMClient>) -> Box<dyn LLMClient> {
        if self.max_in_flight.is_none() {
            return client;
        }
        Box::new(GovernedClient {
            inner: Arc::from(client),
            governor: Arc::new(Self {
                max_in_flight: self.max_in_flight,
                acquire_timeout: self.acquire_timeout,
                semaphore: Arc::clone(&self.semaphore),
            }),
        })
    }
}

/// Client wrapper holding one in-flight slot per dispatch.
///
/// The slot is taken at the start of each call — before any provider I/O —
/// and released when the call resolves. Streams keep the slot until the
/// stream terminates (final item, error item, or drop).
pub(crate) struct GovernedClient {
    inner: Arc<dyn LLMClient>,
    governor: Arc<ProviderGovernor>,
}

#[async_trait]
impl LLMClient for GovernedClient {
    async fn generate(&self, prompt: &str) -> Result<String> {
        let _slot = self.governor.admit().await?;
        self.inner.generate(prompt).await
    }

    async fn generate_with_system(&self, system: &str, prompt: &str) -> Result<String> {
        let _slot = self.governor.admit().await?;
        self.inner.generate_with_system(system, prompt).await
    }

    async fn generate_with_history(&self, messages: &[(String, String)]) -> Result<LLMResponse> {
        let _slot = self.governor.admit().await?;
        self.inner.generate_with_history(messages).await
    }

    async fn generate_with_tools(
        &self,
        prompt: &str,
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        let _slot = self.governor.admit().await?;
        self.inner.generate_with_tools(prompt, tools).await
    }

    async fn generate_with_tools_and_history(
        &self,
        messages: &[crate::coordinator::ConversationMessage],
        tools: &[ToolDefinition],
    ) -> Result<LLMResponse> {
        let _slot = self.governor.admit().await?;
        self.inner
            .generate_with_tools_and_history(messages, tools)
            .await
    }

    async fn stream(
        &self,
        prompt: &str,
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let slot = self.governor.admit().await?;
        match self.inner.stream(prompt).await {
            Ok(stream) => Ok(Box::new(GovernedStream {
                inner: Some(stream),
                _slot: Some(slot),
            })),
            Err(err) => Err(err), // slot drops here: failed setup never holds capacity
        }
    }

    async fn stream_with_system(
        &self,
        system: &str,
        prompt: &str,
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let slot = self.governor.admit().await?;
        match self.inner.stream_with_system(system, prompt).await {
            Ok(stream) => Ok(Box::new(GovernedStream {
                inner: Some(stream),
                _slot: Some(slot),
            })),
            Err(err) => Err(err),
        }
    }

    async fn stream_with_history(
        &self,
        messages: &[(String, String)],
    ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
        let slot = self.governor.admit().await?;
        match self.inner.stream_with_history(messages).await {
            Ok(stream) => Ok(Box::new(GovernedStream {
                inner: Some(stream),
                _slot: Some(slot),
            })),
            Err(err) => Err(err),
        }
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }

    fn supports_hints(&self) -> bool {
        self.inner.supports_hints()
    }

    fn set_hints(&self, hints: crate::client::GenerationHints) {
        self.inner.set_hints(hints);
    }
}

/// Stream wrapper carrying the dispatch slot until termination.
///
/// On the terminal item — success or error — the slot is dropped eagerly so
/// capacity returns before the consumer necessarily drops the stream value.
struct GovernedStream {
    inner: Option<Box<dyn Stream<Item = Result<String>> + Send + Unpin>>,
    _slot: Option<Admission>,
}

impl Stream for GovernedStream {
    type Item = Result<String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        let Some(inner) = this.inner.as_mut() else {
            return Poll::Ready(None);
        };
        match Pin::new(inner).poll_next(cx) {
            Poll::Ready(Some(item @ Ok(_))) => Poll::Ready(Some(item)),
            Poll::Ready(Some(item @ Err(_))) => {
                // Error item is terminal: release the slot now.
                this.inner = None;
                this._slot = None;
                Poll::Ready(Some(item))
            }
            Poll::Ready(None) => {
                // Terminal success: release the slot now.
                this.inner = None;
                this._slot = None;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    /// Mock whose stream yields the configured successes and then one
    /// optional failure. Items are rebuilt per call (AppError is not
    /// Clone); the returned stream is `futures::stream::iter`, which is
    /// Send + Unpin by construction.
    struct StreamingMock {
        ok_texts: Vec<&'static str>,
        fail_with: Option<&'static str>,
    }

    impl StreamingMock {
        fn ok(texts: &[&'static str]) -> Self {
            Self {
                ok_texts: texts.to_vec(),
                fail_with: None,
            }
        }

        fn failing_after(texts: &[&'static str], err: &'static str) -> Self {
            Self {
                ok_texts: texts.to_vec(),
                fail_with: Some(err),
            }
        }
    }

    #[async_trait]
    impl LLMClient for StreamingMock {
        async fn generate(&self, _prompt: &str) -> Result<String> {
            Ok("generated".into())
        }

        async fn generate_with_system(&self, _system: &str, _prompt: &str) -> Result<String> {
            Ok("generated".into())
        }

        async fn generate_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: "generated".into(),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }

        async fn generate_with_tools(
            &self,
            _prompt: &str,
            _tools: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: "generated".into(),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }

        async fn generate_with_tools_and_history(
            &self,
            _messages: &[crate::coordinator::ConversationMessage],
            _tools: &[ToolDefinition],
        ) -> Result<LLMResponse> {
            Ok(LLMResponse {
                content: "generated".into(),
                tool_calls: vec![],
                finish_reason: "stop".into(),
                usage: None,
            })
        }

        async fn stream(
            &self,
            _prompt: &str,
        ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
            let mut items: std::collections::VecDeque<Result<String>> = self
                .ok_texts
                .iter()
                .map(|text| Ok((*text).to_string()))
                .collect();
            if let Some(err) = self.fail_with {
                items.push_back(Err(AppError::LLM(err.to_string())));
            }
            Ok(Box::new(futures::stream::iter(items)))
        }

        async fn stream_with_system(
            &self,
            _system: &str,
            _prompt: &str,
        ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
            self.stream("").await
        }

        async fn stream_with_history(
            &self,
            _messages: &[(String, String)],
        ) -> Result<Box<dyn Stream<Item = Result<String>> + Send + Unpin>> {
            self.stream("").await
        }

        fn model_name(&self) -> &str {
            "streaming-mock"
        }
    }

    fn capped(timeout_ms: u64) -> Arc<ProviderGovernor> {
        Arc::new(ProviderGovernor::new(GovernorConfig {
            max_in_flight: Some(1),
            acquire_timeout: Duration::from_millis(timeout_ms),
        }))
    }

    #[tokio::test]
    async fn unlimited_admission_never_blocks() {
        let governor = ProviderGovernor::new(GovernorConfig::default());
        assert_eq!(governor.max_in_flight(), None);
        for _ in 0..16 {
            assert!(matches!(governor.admit().await, Ok(Admission::Unlimited)));
        }
    }

    #[tokio::test]
    async fn governor_config_deserializes_without_fields() {
        let parsed: GovernorConfig = serde_json::from_str("{}").expect("empty object");
        assert_eq!(parsed, GovernorConfig::default());

        let configured = GovernorConfig::default().with_max_in_flight(7);
        let json = serde_json::to_string(&configured).unwrap();
        assert!(json.contains("\"max_in_flight\":7"));
        let decoded: GovernorConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, configured);
    }

    #[tokio::test]
    async fn saturated_governor_fails_closed_after_timeout() {
        let governor = capped(50);
        let _held = governor.admit().await.expect("first slot");

        let started = std::time::Instant::now();
        let err = governor.admit().await.expect_err("cap is saturated");
        assert!(
            started.elapsed() >= Duration::from_millis(50),
            "waiter should respect the configured timeout"
        );
        match err {
            AppError::LLM(msg) => assert!(msg.contains("provider busy")),
            other => panic!("expected LLM busy error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn governed_stream_holds_slot_until_terminal_item() {
        let governor = capped(60);
        let client = governor.wrap_if_limited(Box::new(StreamingMock::ok(&["a", "b", "c"])));

        let mut stream = client.stream("p").await.expect("stream opens");
        assert!(
            governor.admit().await.is_err(),
            "slot must stay held while the stream body is in flight"
        );

        let mut seen = 0;
        while let Some(item) = stream.next().await {
            assert!(item.is_ok());
            seen += 1;
        }
        assert_eq!(seen, 3);

        assert!(
            governor.admit().await.is_ok(),
            "slot returns once the stream reaches its terminal item"
        );
    }

    #[tokio::test]
    async fn governed_stream_error_item_releases_slot() {
        let governor = capped(60);
        let client = governor.wrap_if_limited(Box::new(StreamingMock::failing_after(
            &["partial"],
            "upstream broke mid-body",
        )));

        let mut stream = client.stream("p").await.expect("stream opens");
        assert!(stream.next().await.unwrap().is_ok());
        let err = stream
            .next()
            .await
            .expect("error item yielded")
            .unwrap_err();
        match err {
            AppError::LLM(msg) => assert!(msg.contains("mid-body")),
            other => panic!("expected upstream error, got {other:?}"),
        }
        // Terminal error item already released the slot.
        assert_eq!(governor.available_permits(), 1);
        assert!(governor.admit().await.is_ok());
    }

    #[tokio::test]
    async fn governed_generate_forwards_hints_to_inner() {
        // Regression guard: the wrapper must forward hint methods, mirroring
        // every other Arc-forwarding adapter in this crate.
        let governor = capped(30);
        let client = governor.wrap_if_limited(Box::new(StreamingMock::ok(&[])));
        assert!(!client.supports_hints(), "inner mock declares no hints");
        client.set_hints(crate::client::GenerationHints {
            json_mode: true,
            ..Default::default()
        });
        assert_eq!(client.model_name(), "streaming-mock");
    }
}
