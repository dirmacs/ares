use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use thiserror::Error;

use crate::context::Context;
use crate::effect::Disposable;
use crate::error::ValidationError;

#[derive(Debug, Error)]
pub enum CordisError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("fiber error: {0}")]
    Fiber(String),
    #[error("service not found: {0}")]
    ServiceNotFound(String),
    #[error("duplicate provider for '{name}' registered by '{owner}'")]
    DuplicateProvider { name: String, owner: String },
    #[error("invalid config: {0}")]
    InvalidConfig(String),
    #[error("invalid config: {0}")]
    Validation(ValidationError),
    #[error("fiber {fiber} stuck in transition for {waited_ms} ms")]
    TransitionStuck { fiber: u64, waited_ms: u64 },
    #[error("internal kernel error: {0}")]
    Internal(String),
}

impl CordisError {
    /// Lift structured [`ValidationError`] issues into the InvalidConfig
    /// error class.
    ///
    /// Display keeps the established `"invalid config: …"` prefix; the
    /// aggregate renders its issues joined by `"; "` (each issue as
    /// `"- msg (at a.b.c)"`). Structure stays recoverable through
    /// [`Self::validation_error`].
    pub fn validation(issues: Vec<crate::error::ValidationIssue>) -> Self {
        Self::Validation(ValidationError::new(issues))
    }

    /// The structured validation issues carried by this error, if any.
    ///
    /// Returns `Some` only for [`CordisError::Validation`]; every other
    /// variant (including stringly [`CordisError::InvalidConfig`]) has no
    /// machine-readable issue list.
    pub fn validation_error(&self) -> Option<&ValidationError> {
        match self {
            Self::Validation(validation) => Some(validation),
            _ => None,
        }
    }

    /// The Display text of this error as an owned `String`.
    ///
    /// Convenience for callers that only want the rendered message (log
    /// fields, aggregates, string assertions) without going through
    /// `format!("{e}")` at every site.
    pub fn message(&self) -> String {
        self.to_string()
    }
}

pub type ServiceInitFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Box<dyn Disposable>>, CordisError>> + Send + 'a>>;

pub trait Service: Send + Sync + 'static {
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    fn init(&self, _ctx: &Arc<Context>) -> ServiceInitFuture<'_> {
        Box::pin(async move { Ok(None) })
    }

    /// Availability predicate for this service instance.
    ///
    /// The kernel consumes `check()` at every point where a freshly built
    /// instance meets the graph:
    ///
    /// - [`crate::RegistryService::register`] — after the plugin factory
    ///   produces the service and BEFORE it is provided; a `false` verdict
    ///   rests the fiber as inspectable `Failed { error: "availability
    ///   predicate rejected service" }` instead of `Active`. Registration is
    ///   non-throwing: register-before-ready is a supported transient that
    ///   later refreshes converge.
    /// - [`crate::Context::plugin`] — after `init`; a `false` verdict leaves
    ///   the provided value in place but rests the fiber as `Inactive` (the
    ///   historical behavior).
    ///
    /// It is NOT consulted on untyped store reads (`Context::get`): the store
    /// holds type-erased values, so per-read checks would need downcasting
    /// machinery that no consumer has asked for. Services whose availability
    /// can change AFTER registration (circuit breakers, feature gates) must
    /// drive their dependents by re-providing / notifying instead of relying
    /// on spontaneous re-checks.
    fn check(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod error_tests {
    use super::CordisError;

    #[test]
    fn structured_variants_render_through_display_and_message() {
        let cases: Vec<(CordisError, &str)> = vec![
            (
                CordisError::ServiceNotFound("ares_tools::Tools".into()),
                "service not found: ares_tools::Tools",
            ),
            (
                CordisError::DuplicateProvider {
                    name: "cordis::EventsService".into(),
                    owner: "context".into(),
                },
                "duplicate provider for 'cordis::EventsService' registered by 'context'",
            ),
            (
                CordisError::InvalidConfig("missing url".into()),
                "invalid config: missing url",
            ),
            (
                CordisError::TransitionStuck {
                    fiber: 7,
                    waited_ms: 250,
                },
                "fiber 7 stuck in transition for 250 ms",
            ),
            (
                CordisError::Internal("invariant violated".into()),
                "internal kernel error: invariant violated",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.message(), expected);
            // Round-trip: matching the constructed variant recovers its fields.
            match err {
                CordisError::ServiceNotFound(name) => {
                    assert_eq!(name, "ares_tools::Tools");
                }
                CordisError::DuplicateProvider { name, owner } => {
                    assert_eq!(
                        (name.as_str(), owner.as_str()),
                        ("cordis::EventsService", "context")
                    );
                }
                CordisError::InvalidConfig(msg) => assert_eq!(msg, "missing url"),
                CordisError::TransitionStuck { fiber, waited_ms } => {
                    assert_eq!((fiber, waited_ms), (7, 250));
                }
                CordisError::Internal(msg) => assert_eq!(msg, "invariant violated"),
                other => panic!("unexpected variant: {other:?}"),
            }
        }
    }

    #[test]
    fn legacy_catch_all_variants_are_unchanged() {
        let config = CordisError::Configuration("still supported".into());
        let fiber = CordisError::Fiber("still supported".into());
        assert_eq!(config.message(), "configuration error: still supported");
        assert_eq!(fiber.message(), "fiber error: still supported");
    }

    /// The single-source discipline refusal keeps the `duplicate provider`
    /// phrase that tests and docs assert on via `contains`.
    #[test]
    fn duplicate_provider_display_keeps_asserted_phrase() {
        let err = CordisError::DuplicateProvider {
            name: "FooService".into(),
            owner: "root".into(),
        };
        assert!(
            err.to_string().contains("duplicate provider"),
            "Display must keep the asserted phrase, got: {err}"
        );
    }
}
