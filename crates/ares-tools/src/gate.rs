//! The pre-call tool gate contract, with no dependency on any gate engine.
//!
//! This module exists because the OpenAPPA implementation cannot be published:
//! `archestra-ai/OpenAPPA` sets `publish = false` on its workspace, and cargo
//! refuses to package any manifest carrying a `git` dependency — optional or
//! not. Keeping the contract here and the engine in `ares-tools-appa` is what
//! lets `ares-tools` publish while the deployed server still runs the gate.
//!
//! Every type here is a plain local shape. Nothing in this file names a type
//! from the gate engine, so a consumer can implement [`ToolGate`] without
//! taking that dependency at all.

use serde_json::Value;
use std::sync::Arc;

/// Which trajectory a call is judged under.
///
/// Mirrors the engine's `Actor` without importing it: the engine identifies a
/// trajectory by a root id and an optional child id, and that is all a host
/// needs to say which conversation a call belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActorRef {
    /// The root trajectory — one per session or tenant.
    pub root: String,
    /// The forked child currently running, when there is one.
    pub child: Option<String>,
}

impl ActorRef {
    pub fn new(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            child: None,
        }
    }
}

/// Why a gate could not open.
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error("gate configuration refused: {0}")]
    Config(String),
    #[error("gate runtime refused: {0}")]
    Open(String),
    #[error("gate event log refused: {0}")]
    Log(String),
    #[error("gate arguments could not be carried: {0}")]
    Arguments(String),
}

/// The gate's answer to one proposed call.
#[derive(Debug, Clone, PartialEq)]
pub enum PreCall {
    /// The engine allows the call; execute as today.
    Allow,
    /// The policy blocks the call and names why, with the ids of the remedies
    /// it offers. The tool's execute path is not entered; the feedback and the
    /// offer ids reach the model as the call's result, so it can take a remedy
    /// on the control tool.
    Deny {
        feedback: String,
        offers: Vec<String>,
    },
    /// A hard stop: the turn ends. No remedy is offered.
    Block { reason: String },
    /// An operational fault: nothing was judged. The turn ends; the detail is
    /// the runtime's words, never policy feedback.
    Refuse { detail: String },
    /// The model quoted an offer on the control tool; route to the remedy path.
    Control,
}

/// Why a result did not reach the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Withheld {
    /// The engine (or the fence) blocked the value; the conversation sees the
    /// reason, never the value.
    Blocked { reason: String },
    /// An operational fault at result time; the value is withheld and the
    /// detail is the runtime's words.
    Refused { detail: String },
}

/// How a tool call ended, as the engine records it.
///
/// Mirrors the engine's own variants so a host can report what happened
/// without importing the engine: a success may carry a body the runtime
/// declined to carry, and a call whose outcome is unknown must say so rather
/// than claim failure.
#[derive(Debug, Clone)]
pub enum ToolOutcome {
    /// The tool returned. `body` is None when the runtime refused to carry it.
    Success { body: Option<String> },
    /// The tool failed; the message is what the engine records.
    Failure { message: String },
    /// The call's fate is unknown — neither success nor failure can be claimed.
    Indeterminate,
}

/// The engine's answer to a quoted remedy plan.
#[derive(Debug, Clone)]
pub enum RemedyAnswer {
    /// The plan was authorized and the engine ran the call.
    Authorized { call: Value },
    /// The plan crossed a declared return; the value is here.
    Returned { value: Value },
    /// The engine declined the plan.
    Declined { presentation: String },
    /// The engine was asked and did not answer.
    NoAnswer { feedback: String },
    /// The engine refused to consider the plan.
    Refused { reason: String },
}

/// One proposed call, as the engine needs to see it.
#[derive(Debug, Clone)]
pub struct RemedyPlan {
    /// The plan arguments the model quoted, opaque to the host.
    pub args: Value,
}

/// The funnel's view of a gate.
///
/// Deliberately object-safe (the concrete runtime holds a deep type tree, and
/// a monomorphised auto-trait check has to recurse through it from a spawn
/// boundary, which overflows the solver on the pinned toolchain).
#[async_trait::async_trait]
pub trait ToolGate: Send + Sync {
    /// The trajectory this host's contexts decide under.
    fn actor_for(&self, tenant: Option<&str>) -> ActorRef;

    /// Submit the pre-call event and fold the engine's answer.
    async fn check_call(
        &self,
        actor: &ActorRef,
        tool: &str,
        args: &Value,
    ) -> Result<PreCall, String>;

    /// Submit the post-call event and fold the engine's answer.
    async fn admit_result(
        &self,
        actor: &ActorRef,
        tool: &str,
        args: &Value,
        result: &Value,
    ) -> Result<Value, Withheld>;

    /// Execute one remedy plan the model quoted on the control tool.
    async fn execute_remedy(&self, actor: &ActorRef, plan: RemedyPlan) -> RemedyAnswer;

    /// Report a tool that ran and failed.
    async fn report_failure(
        &self,
        actor: &ActorRef,
        tool: &str,
        args: &Value,
        failure: ToolOutcome,
    );

    /// Snapshot of one trajectory's fence audit ring.
    fn fence_audit(&self, actor: &ActorRef) -> Vec<crate::fence::AuditEntry>;

    /// Is this name the engine's reserved control tool rather than a host tool?
    ///
    /// The engine owns the answer; the host must not hardcode it, or a control
    /// tool rename in the engine would silently start routing to not-found.
    fn is_reserved_tool_name(&self, name: &str) -> bool;

    /// The host's spelling of the control tool, for routing a quote.
    fn control_tool_spelling(&self) -> &'static str;
}

/// A shared gate is a gate: the funnel holds `Arc<dyn ToolGate>` and passes
/// it around by value.
#[async_trait::async_trait]
impl<T: ToolGate + Send + Sync + ?Sized> ToolGate for Arc<T> {
    fn actor_for(&self, tenant: Option<&str>) -> ActorRef {
        (**self).actor_for(tenant)
    }

    async fn check_call(
        &self,
        actor: &ActorRef,
        tool: &str,
        args: &Value,
    ) -> Result<PreCall, String> {
        (**self).check_call(actor, tool, args).await
    }

    async fn admit_result(
        &self,
        actor: &ActorRef,
        tool: &str,
        args: &Value,
        result: &Value,
    ) -> Result<Value, Withheld> {
        (**self).admit_result(actor, tool, args, result).await
    }

    async fn execute_remedy(&self, actor: &ActorRef, args: RemedyPlan) -> RemedyAnswer {
        (**self).execute_remedy(actor, args).await
    }

    async fn report_failure(
        &self,
        actor: &ActorRef,
        tool: &str,
        args: &Value,
        failure: ToolOutcome,
    ) {
        (**self).report_failure(actor, tool, args, failure).await
    }

    fn fence_audit(&self, actor: &ActorRef) -> Vec<crate::fence::AuditEntry> {
        (**self).fence_audit(actor)
    }

    fn is_reserved_tool_name(&self, name: &str) -> bool {
        (**self).is_reserved_tool_name(name)
    }

    fn control_tool_spelling(&self) -> &'static str {
        (**self).control_tool_spelling()
    }
}

/// A constructor installed by the engine crate that owns the gate.
///
/// `ares-tools` cannot name `AppaGate` — that would put the git dependency back
/// in the published manifest and make this crate unpublishable again. Instead
/// the engine crate registers a builder here, and the tools plugin calls it.
pub type GateBuilder =
    fn(&serde_json::Value) -> Result<Option<Arc<dyn ToolGate>>, cordis::CordisError>;

static GATE_BUILDER: std::sync::OnceLock<GateBuilder> = std::sync::OnceLock::new();

/// Install the engine's gate builder. First call wins; later calls are no-ops.
pub fn install_gate_builder(builder: GateBuilder) {
    let _ = GATE_BUILDER.set(builder);
}

/// Build the configured gate, if an engine crate installed a builder.
///
/// No builder installed means no gate, which is an explicitly unguarded build:
/// a config that ASKS for a gate is refused rather than silently accepted.
pub fn installed_from_config(
    config: &serde_json::Value,
) -> Result<Option<Arc<dyn ToolGate>>, cordis::CordisError> {
    match GATE_BUILDER.get() {
        Some(builder) => builder(config),
        None => {
            if config.get("appa").is_some() || std::env::var_os("ARES_APPA_CONFIG").is_some() {
                return Err(cordis::CordisError::Configuration(
                    "a pre-call gate is configured but no gate engine is linked into this \
                     build; add the ares-tools-appa dependency rather than running unguarded"
                        .to_string(),
                ));
            }
            Ok(None)
        }
    }
}
