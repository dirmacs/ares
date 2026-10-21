//! The OpenAPPA embed: the host-side Adapter, the pre/post-call gate at the
//! [`Tools`](crate::Tools) funnel, and the write-contract layer the post-call
//! check rides on.
//!
//! ## Where it sits
//!
//! [`Tools::execute`](crate::Tools::execute) is the single funnel every agent
//! loop dispatches through (the `ToolCoordinator`, the streaming fallback, and
//! the skills engine). This module gates that funnel twice per call:
//!
//! 1. **Pre-call** — a [`HookEvent::ToolCall`] to the OpenAPPA runtime. An
//!    `AllowCall` executes as today; a `DenyCall` returns the engine's feedback
//!    to the model *without entering the tool's execute path*; a `Block` stops
//!    the turn; a `Refuse` (an operational fault: an annotator, sanitizer, or
//!    the engine itself did not answer) stops the turn too — never a default
//!    allow.
//! 2. **Post-call** — a [`HookEvent::ToolResult`]. The admitted value is
//!    delivered to the model; a `Block`/`ReplaceOutput` withholds the raw value
//!    and the conversation sees the runtime's words instead. On an admitted
//!    result the L0-L3 write fence ([`crate::fence`]) judges the write contract
//!    of a write-class tool that already landed: a violated contract withholds
//!    the result and lands in the fence's audit ring.
//!
//! ## The control tool
//!
//! The model takes a remedy by quoting an `offer_id` on the canonical control
//! tool `appa/execute_remedy_plan`, which this host spells for the model as
//! `execute_remedy_plan` (function-calling APIs reject `/` in tool names). A
//! call under either spelling is intercepted *before* tool resolution and routed
//! to [`AppaGate::execute_remedy`] under the engine's mandate envelope.
//!
//! ## Trajectories
//!
//! One OpenAPPA trajectory per tenant isolate label (falling back to
//! `default`): `ares:<tenant>`. A trajectory records its own facts — effects,
//! accepted narrowings, admitted values — so a tenant's runs share one decision
//! history. Per-run trajectories are a deliberate omission recorded in the PR
//! that ships this (ARES carries no run id on the tool context today).
//!
//! ## Arity of the arguments
//!
//! The engine requires the arguments **unparsed**: duplicate keys and other
//! refusable forms must reach it, not a parser's interpretation. ARES parses
//! tool arguments at the LLM wire boundary (`genai` hands the coordinator a
//! `serde_json::Value`), so the bytes that reach this funnel are already the
//! parser's reading. The gate therefore re-serializes the value it is handed and
//! crosses it as a `RawValue`; recovering the original bytes requires a change
//! upstream of this funnel (also recorded as remaining work).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use appa_eventlog::{Backend, LogStore};
use appa_runtime::api::{ExecuteRemedyPlanArgs, OpenError, RemedyOutcome, Runtime};
use appa_runtime::config::{Config, ConfigError, HostDefaults, HostedBattery};
use appa_runtime::hooks;
use appa_runtime_api::{
    ADVERTISED_CONTROL_TOOL, Actor, Adapter, AdapterName, CanonicalTool, CanonicalToolError,
    HookDecision, HookEvent, IdentifiedTool, ParseRefusal, ProposedCall, TrajectoryId,
};

use crate::fence::{Fence, FencePolicy, WriteGuard};

/// The host spelling of the canonical control tool, as the model calls it.
pub const REMEDY_TOOL_SPELLING: &str = ADVERTISED_CONTROL_TOOL;

/// The canonical toolkit namespace every ARES tool maps into.
const HOST_NAMESPACE: &str = "ares";

/// Default read-class tool names the fence observes. A read through a listed
/// tool registers the path (and its version) in the trajectory's fence, which
/// is what a later `ReplaceIfVersion` write chains off.
pub const DEFAULT_FENCE_READ_TOOLS: &[&str] = &[
    "read_file",
    "read_web_page",
    "files_read",
    "get_file_contents",
];

/// Default write-class tool names the fence gates. A write through a listed
/// tool must satisfy a guard contract against the observation the trajectory
/// holds, or its result is withheld.
pub const DEFAULT_FENCE_WRITE_TOOLS: &[&str] = &[
    "write_file",
    "edit_file",
    "edit_file_lines",
    "insert_after",
    "append_file",
];

/// The argument keys a fence-relevant tool names its path under, in the order
/// they are tried.
const PATH_ARG_KEYS: &[&str] = &["path", "file_path", "filename", "file"];

/// Identify one of ARES's raw tool spellings as a canonical tool id.
///
/// The mapping is a bijection over the spellings ARES accepts: the control
/// tool under either spelling, an MCP bridge tool `mcp_<server>_<tool>`, and
/// everything else as `host/ares/<name>`.
fn identify_tool(raw: &str) -> Result<IdentifiedTool, ParseRefusal> {
    let canonical = if appa_runtime_api::is_reserved_tool_name(raw) {
        CanonicalTool::control()
    } else if let Some(rest) = raw.strip_prefix("mcp_") {
        match rest.split_once('_') {
            Some((server, tool)) => CanonicalTool::of("mcp", server, tool),
            None => CanonicalTool::of("mcp", rest, "tool"),
        }
        .map_err(|error: CanonicalToolError| ParseRefusal::Malformed {
            detail: error.to_string(),
        })?
    } else {
        CanonicalTool::of("host", HOST_NAMESPACE, raw).map_err(|error: CanonicalToolError| {
            ParseRefusal::Malformed {
                detail: error.to_string(),
            }
        })?
    };
    Ok(IdentifiedTool {
        canonical,
        spawn: None,
    })
}

/// Spell a canonical id back into the spelling ARES's model dispatches. The
/// inverse of [`identify_tool`]; a canonical id this host cannot produce stays
/// unspelled, which the open reads as a hard failure only for the control tool.
fn spell(canonical: &CanonicalTool) -> Option<String> {
    if canonical.is_control() {
        return Some(REMEDY_TOOL_SPELLING.to_string());
    }
    let mut segments = canonical.as_str().splitn(3, '/');
    match (segments.next(), segments.next(), segments.next()) {
        (Some("mcp"), Some(server), Some(tool)) => Some(format!("mcp_{server}_{tool}")),
        (Some("host"), Some(HOST_NAMESPACE), Some(name)) => Some(name.to_string()),
        _ => None,
    }
}

fn names_children(_: &Actor, _: &ProposedCall) -> Vec<TrajectoryId> {
    Vec::new()
}

/// The host's shape translation: an in-process host that names tools the way
/// its own policy does. ARES spawns no child trajectories, so no call names a
/// child and the wildcard's spawn cover stands.
pub fn adapter() -> Adapter {
    Adapter {
        name: AdapterName::Embedded,
        identify_tool,
        names_children,
        spell,
        wildcard_covers_spawn: true,
        spells_server: |name| name.starts_with("mcp_"),
    }
}

/// Why a gate could not open.
#[derive(Debug, thiserror::Error)]
pub enum AppaError {
    #[error("appa configuration refused: {0}")]
    Config(#[from] ConfigError),
    #[error("appa runtime refused: {0}")]
    Open(#[from] OpenError),
    #[error("appa event log refused: {0}")]
    Log(String),
    #[error("appa arguments could not be carried: {0}")]
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

/// Which fence contract class a tool's call falls into.
#[derive(Clone, Debug)]
pub struct FenceClasses {
    read: Vec<String>,
    write: Vec<String>,
}

impl Default for FenceClasses {
    fn default() -> Self {
        Self {
            read: DEFAULT_FENCE_READ_TOOLS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            write: DEFAULT_FENCE_WRITE_TOOLS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

impl FenceClasses {
    fn class_of(&self, tool: &str) -> Option<bool> {
        if self.write.iter().any(|name| name == tool) {
            Some(true)
        } else if self.read.iter().any(|name| name == tool) {
            Some(false)
        } else {
            None
        }
    }
}

/// The funnel's view of the gate, as a trait object: the concrete runtime
/// holds a deep type tree, and an auto-trait check that has to recurse
/// through it from a spawn boundary overflows the solver on the pinned
/// toolchain. The object stops that recursion; the bound is what the funnel
/// and the tokio spawns actually require.
#[async_trait::async_trait]
pub trait ToolGate: Send + Sync {
    /// The trajectory this host's contexts decide under.
    fn actor_for(&self, tenant: Option<&str>) -> Actor;

    /// Submit the pre-call event and fold the engine's answer.
    async fn check_call(&self, actor: &Actor, tool: &str, args: &Value) -> Result<PreCall, String>;

    /// Submit the post-call event and fold the engine's answer.
    async fn admit_result(
        &self,
        actor: &Actor,
        tool: &str,
        args: &Value,
        result: &Value,
    ) -> Result<Value, Withheld>;

    /// Execute one remedy plan the model quoted on the control tool.
    async fn execute_remedy(&self, actor: &Actor, args: ExecuteRemedyPlanArgs) -> RemedyOutcome;

    /// Report a tool that ran and failed.
    async fn report_failure(
        &self,
        actor: &Actor,
        tool: &str,
        args: &Value,
        failure: appa_runtime_api::ToolOutcome,
    );

    /// Snapshot of one trajectory's fence audit ring.
    fn fence_audit(&self, actor: &Actor) -> Vec<crate::fence::AuditEntry>;
}

/// A shared gate is a gate: the funnel holds `Arc<dyn ToolGate>` and passes
/// it around by value.
#[async_trait::async_trait]
impl<T: ToolGate + Send + Sync + ?Sized> ToolGate for Arc<T> {
    fn actor_for(&self, tenant: Option<&str>) -> Actor {
        (**self).actor_for(tenant)
    }

    async fn check_call(&self, actor: &Actor, tool: &str, args: &Value) -> Result<PreCall, String> {
        (**self).check_call(actor, tool, args).await
    }

    async fn admit_result(
        &self,
        actor: &Actor,
        tool: &str,
        args: &Value,
        result: &Value,
    ) -> Result<Value, Withheld> {
        (**self).admit_result(actor, tool, args, result).await
    }

    async fn execute_remedy(&self, actor: &Actor, args: ExecuteRemedyPlanArgs) -> RemedyOutcome {
        (**self).execute_remedy(actor, args).await
    }

    async fn report_failure(
        &self,
        actor: &Actor,
        tool: &str,
        args: &Value,
        failure: appa_runtime_api::ToolOutcome,
    ) {
        (**self).report_failure(actor, tool, args, failure).await
    }

    fn fence_audit(&self, actor: &Actor) -> Vec<crate::fence::AuditEntry> {
        (**self).fence_audit(actor)
    }
}

#[async_trait::async_trait]
impl ToolGate for AppaGate {
    fn actor_for(&self, tenant: Option<&str>) -> Actor {
        AppaGate::actor_for(self, tenant)
    }

    async fn check_call(&self, actor: &Actor, tool: &str, args: &Value) -> Result<PreCall, String> {
        AppaGate::check_call(self, actor, tool, args).await
    }

    async fn admit_result(
        &self,
        actor: &Actor,
        tool: &str,
        args: &Value,
        result: &Value,
    ) -> Result<Value, Withheld> {
        AppaGate::admit_result(self, actor, tool, args, result).await
    }

    async fn execute_remedy(&self, actor: &Actor, args: ExecuteRemedyPlanArgs) -> RemedyOutcome {
        AppaGate::execute_remedy(self, actor, args).await
    }

    async fn report_failure(
        &self,
        actor: &Actor,
        tool: &str,
        args: &Value,
        failure: appa_runtime_api::ToolOutcome,
    ) {
        AppaGate::report_failure(self, actor, tool, args, failure).await
    }

    fn fence_audit(&self, actor: &Actor) -> Vec<crate::fence::AuditEntry> {
        AppaGate::fence_audit(self, actor)
    }
}

/// The host-side gate: one OpenAPPA runtime, one adapter, one fence policy,
/// with a per-trajectory fence and a record of the trajectories this host has
/// opened.
pub struct AppaGate {
    runtime: Runtime,
    fence_policy: FencePolicy,
    fence_classes: FenceClasses,
    fences: Mutex<HashMap<String, Fence>>,
    sessions: Mutex<HashSet<String>>,
}

impl AppaGate {
    /// Open the runtime over a hosted policy document composed from `root` and
    /// `batteries`, in-memory trajectory log, and the ARES adapter.
    ///
    /// `lookup` answers the runtime variables the document's `token_env`
    /// entries name; a host that grants none answers `None`.
    pub fn open(
        root: &str,
        batteries: &[HostedBattery<'_>],
        defaults: HostDefaults,
        lookup: impl Fn(&str) -> Option<String>,
        fence_policy: FencePolicy,
    ) -> Result<Self, AppaError> {
        Self::open_with_classes(
            root,
            batteries,
            defaults,
            lookup,
            fence_policy,
            FenceClasses::default(),
        )
    }

    /// [`AppaGate::open`] with an explicit fence tool-class table.
    pub fn open_with_classes(
        root: &str,
        batteries: &[HostedBattery<'_>],
        defaults: HostDefaults,
        lookup: impl Fn(&str) -> Option<String>,
        fence_policy: FencePolicy,
        fence_classes: FenceClasses,
    ) -> Result<Self, AppaError> {
        let config = Config::hosted_composed(root, batteries, defaults, lookup)?;
        let store = Arc::new(
            LogStore::open(Backend::Memory).map_err(|error| AppaError::Log(error.to_string()))?,
        );
        let runtime = Runtime::open_with_store_as(config, store, None, adapter())?;
        Ok(Self {
            runtime,
            fence_policy,
            fence_classes,
            fences: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashSet::new()),
        })
    }

    /// The trajectory this host's contexts decide under.
    pub fn actor_for(&self, tenant: Option<&str>) -> Actor {
        let root = match tenant {
            Some(tenant) if !tenant.is_empty() => format!("ares:{tenant}"),
            _ => "ares:default".to_string(),
        };
        Actor {
            root: TrajectoryId::new(root),
            child: None,
        }
    }

    /// The runtime this gate decides under, for diagnostics and tests.
    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Submit the pre-call event and fold the engine's answer.
    ///
    /// An error return is an operational failure the caller must treat as a
    /// stop; the string is the runtime's own words.
    pub async fn check_call(
        &self,
        actor: &Actor,
        tool: &str,
        args: &Value,
    ) -> Result<PreCall, String> {
        self.ensure_session(actor).await?;
        // An embedding host names tools the way its own policy does: identify
        // the raw spelling through this host's adapter and cross the canonical
        // identity, exactly as the embedded reference does.
        let identified = identify_tool(tool).map_err(|refusal| match refusal {
            ParseRefusal::Unreadable { detail } => detail,
            ParseRefusal::Malformed { detail } => detail,
        })?;
        let arguments = self.carry(args)?;
        let decision = hooks::handle(
            &self.runtime,
            HookEvent::ToolCall {
                actor: actor.clone(),
                call: ProposedCall {
                    tool: identified.canonical.into_string(),
                    arguments,
                    cwd: None,
                },
                call_id: None,
                spawn: None,
                prompt: None,
                ruling: None,
            },
        )
        .await;
        Ok(fold_call(decision))
    }

    /// Submit the post-call event and fold the engine's answer, then run the
    /// write-contract check over an admitted result.
    ///
    /// `Ok(value)` is the value that reaches the model; `Err` is a withheld
    /// result — the raw value never re-enters the conversation.
    pub async fn admit_result(
        &self,
        actor: &Actor,
        tool: &str,
        args: &Value,
        result: &Value,
    ) -> Result<Value, Withheld> {
        self.ensure_session(actor)
            .await
            .map_err(|detail| Withheld::Refused { detail })?;
        let arguments = self
            .carry(args)
            .map_err(|detail| Withheld::Refused { detail })?;
        let body = serde_json::to_string(result).map_err(|error| Withheld::Refused {
            detail: format!("the result could not be carried: {error}"),
        })?;
        let identified = identify_tool(tool).map_err(|refusal| Withheld::Refused {
            detail: match refusal {
                ParseRefusal::Unreadable { detail } => detail,
                ParseRefusal::Malformed { detail } => detail,
            },
        })?;
        let decision = hooks::handle(
            &self.runtime,
            HookEvent::ToolResult {
                actor: actor.clone(),
                call: ProposedCall {
                    tool: identified.canonical.into_string(),
                    arguments,
                    cwd: None,
                },
                call_id: None,
                outcome: appa_runtime_api::ToolOutcome::Success {
                    body: appa_runtime_api::OutcomeBody::Available(body),
                },
            },
        )
        .await;

        let admitted = match decision {
            HookDecision::Ack | HookDecision::Context { .. } => Some(result.clone()),
            // The admitted value crosses as it crossed: these bytes, nothing
            // rewritten in them.
            HookDecision::DeliverValue { value } => Some(Value::String(value)),
            // The runtime's words replace the value; the value never crosses.
            HookDecision::ReplaceOutput { output } => {
                return Err(Withheld::Blocked { reason: output });
            }
            HookDecision::Block { reason } => return Err(Withheld::Blocked { reason }),
            HookDecision::Refuse { detail } => return Err(Withheld::Refused { detail }),
            // No call decision is a legal answer to a result event; fail
            // closed rather than guess.
            other => {
                return Err(Withheld::Refused {
                    detail: format!("appa answered a tool result with {other:?}"),
                });
            }
        };

        // The admitted result now rides the write-contract layer: a
        // write-class tool's landed write is judged against the fence, and a
        // violated contract withholds the result.
        let value = admitted.ok_or_else(|| Withheld::Refused {
            detail: "appa answered a tool result with nothing".to_string(),
        })?;
        if let Some(reason) = self.write_contract(actor, tool, args) {
            return Err(Withheld::Blocked { reason });
        }
        Ok(value)
    }

    /// Execute one remedy plan the model quoted on the control tool, inside
    /// the engine's mandate envelope.
    pub async fn execute_remedy(
        &self,
        actor: &Actor,
        args: ExecuteRemedyPlanArgs,
    ) -> RemedyOutcome {
        self.runtime.execute_embedded_remedy(actor, args).await
    }

    /// Report a tool that ran and failed: the engine closes the dispatch as a
    /// failure and no value is admitted. Without this the trajectory would
    /// hold an open dispatch awaiting a result that never crossed.
    pub async fn report_failure(
        &self,
        actor: &Actor,
        tool: &str,
        args: &Value,
        failure: appa_runtime_api::ToolOutcome,
    ) {
        let arguments = match self.carry(args) {
            Ok(arguments) => arguments,
            Err(error) => {
                tracing::warn!(%error, tool, "the failed call's arguments could not be carried");
                return;
            }
        };
        let Ok(identified) = identify_tool(tool) else {
            return;
        };
        let decision = hooks::handle(
            &self.runtime,
            HookEvent::ToolResult {
                actor: actor.clone(),
                call: ProposedCall {
                    tool: identified.canonical.into_string(),
                    arguments,
                    cwd: None,
                },
                call_id: None,
                outcome: failure,
            },
        )
        .await;
        if let HookDecision::Refuse { detail } = decision {
            tracing::debug!(%detail, tool, "the runtime refused a failed call's report");
        }
    }

    /// The trajectory's fence, creating it on first use.
    fn fence_for(&self, actor: &Actor) -> std::sync::MutexGuard<'_, HashMap<String, Fence>> {
        let mut fences = self.fences.lock().expect("the fence table is not poisoned");
        fences
            .entry(actor.root.as_str().to_string())
            .or_insert_with(|| Fence::new(self.fence_policy.clone()));
        fences
    }

    /// The post-call write contract for one call, when the fence classifies
    /// the tool.
    ///
    /// Returns `Some(reason)` when the call violated its contract. A
    /// write-class tool's landed write is judged with
    /// [`WriteGuard::Unconditional`]: the observation the trajectory holds is
    /// the contract a post-call check can still prove, because the write that
    /// just landed has already moved the version a `ReplaceIfVersion` guard
    /// would compare against. The version equality contract belongs to the
    /// pre-write path, where the bytes route through [`Fence::fence_write`].
    /// A read-class call records its observation so a later write can prove
    /// read-before-edit, and a read the fence refuses is itself withheld: the
    /// model must not see out-of-fence bytes. Either violation lands in the
    /// fence's audit ring.
    fn write_contract(&self, actor: &Actor, tool: &str, args: &Value) -> Option<String> {
        let write = self.fence_classes.class_of(tool)?;
        let path = match path_argument(args) {
            Some(path) => path,
            // A fence-relevant call that names no path cannot satisfy a
            // contract; refuse it rather than wave it through.
            None => {
                let class = if write { "write" } else { "read" };
                return Some(format!(
                    "fence FS_NOT_OBSERVED: a {class}-class call named no path"
                ));
            }
        };
        let mut fences = self.fence_for(actor);
        let fence = fences
            .get_mut(actor.root.as_str())
            .expect("fence_for just inserted the trajectory's fence");
        let outcome = if write {
            fence
                .verify_write(&path, WriteGuard::Unconditional)
                .map(|_| ())
        } else {
            fence.fence_read(&path).map(|_| ())
        };
        match outcome {
            Ok(()) => None,
            Err(error) => Some(format!("fence {}: {}", error.code, error.message)),
        }
    }

    /// Carry the arguments as unparsed bytes. ARES hands the funnel a parsed
    /// value, so these bytes are the parser's reading re-serialized; see the
    /// module docs.
    fn carry(&self, args: &Value) -> Result<Box<serde_json::value::RawValue>, String> {
        let text = serde_json::to_string(args).map_err(|error| error.to_string())?;
        serde_json::value::RawValue::from_string(text).map_err(|error| error.to_string())
    }

    /// Open the trajectory on first use. A refused open refuses the call.
    async fn ensure_session(&self, actor: &Actor) -> Result<(), String> {
        {
            let sessions = self
                .sessions
                .lock()
                .expect("the session set is not poisoned");
            if sessions.contains(actor.root.as_str()) {
                return Ok(());
            }
        }
        let decision = hooks::handle(
            &self.runtime,
            HookEvent::SessionStart {
                root: actor.root.clone(),
                principal: None,
                address: None,
                title: None,
                start: None,
                launch: None,
            },
        )
        .await;
        match decision {
            HookDecision::Ack | HookDecision::Context { .. } => {
                let mut sessions = self
                    .sessions
                    .lock()
                    .expect("the session set is not poisoned");
                sessions.insert(actor.root.as_str().to_string());
                Ok(())
            }
            HookDecision::Refuse { detail } => Err(detail),
            other => Err(format!("appa answered a session start with {other:?}")),
        }
    }

    /// Snapshot of one trajectory's fence audit ring, for diagnostics.
    pub fn fence_audit(&self, actor: &Actor) -> Vec<crate::fence::AuditEntry> {
        self.fence_for(actor)
            .get(actor.root.as_str())
            .map(|fence| fence.audit_log())
            .unwrap_or_default()
    }
}

/// Fold a call decision into the gate's answer. An answer that names no legal
/// call decision fails closed as a refusal.
fn fold_call(decision: HookDecision) -> PreCall {
    match decision {
        HookDecision::AllowCall { .. } => PreCall::Allow,
        HookDecision::DenyCall {
            feedback, offers, ..
        } => PreCall::Deny {
            feedback,
            offers: offers.into_iter().map(|offer| offer.id).collect(),
        },
        HookDecision::Block { reason } => PreCall::Block { reason },
        HookDecision::Refuse { detail } => PreCall::Refuse { detail },
        HookDecision::PassControl => PreCall::Control,
        other => PreCall::Refuse {
            detail: format!("appa answered a tool call with {other:?}"),
        },
    }
}

/// The path argument a fence-relevant call names, if any.
fn path_argument(args: &Value) -> Option<PathBuf> {
    let object = args.as_object()?;
    PATH_ARG_KEYS
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str).map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fence::FenceMode;
    use crate::registry::Tool;
    use crate::tool_service::Tools;
    use appa_runtime_api::CONTROL_TOOL;
    use async_trait::async_trait;
    use cordis::{Context, EventsService};
    use serde_json::json;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// The test posture of the estate policy: everything the funnel is judged
    /// against in these tests. The annotator URL is a placeholder so a closed
    /// port can stand in for a battery outage.
    const TEST_ROOT_POLICY: &str = r#"
[policy]
version = 2
trust_chain = ["suspicious", "trusted"]

[[policy.tool]]
name = "host/ares/read_ticket"
description = "Reads a knowledge-store record (eruka-classified content)."
tags = ["knowledge"]
# Audience-only here: the test's sanitizer route carries the audience back to
# public, so the withheld result can cross. The trust-floor rule is covered by
# the replay suite's trust-floor trace.
delta = { audience = ["internal"] }
effects = ["knowledge.read"]

[[policy.tool]]
name = "host/ares/publish_update"
description = "Publishes data to the world: requires an unrestricted audience."
requires = { audience = { contains = ["public"] } }
delta = {}

[[policy.tool]]
name = "host/ares/notes"
description = "Annotated tool: its contract comes from the classification battery."
annotator = "classify_notes"

[[policy.tool]]
name = "host/ares/echo"
delta = {}

[[policy.tool]]
name = "host/ares/read_file"
delta = {}

[[policy.tool]]
name = "host/ares/write_file"
delta = {}

[[policy.annotator]]
name = "classify_notes"
ranks = ["suspicious", "trusted"]
audiences = ["internal"]
marks = []
effects = []
hint = "Classify ARES notes calls."

[policy.deployment]
confined_results = ["host/ares/read_ticket"]

[[policy.sanitizer]]
name = "redact_private"
on = ["tool_output"]
tags = ["knowledge"]
hint = "Remove private knowledge-store content from a ticket."

[policy.sanitizer.permits]
audience = { from = ["internal"], to = ["public"] }

[externals.annotators.classify_notes]
url = "{annotator_url}"

[externals.sanitizers.redact_private]
builtin = "redact-secrets"
"#;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock runs")
                .as_nanos();
            let dir = std::env::temp_dir()
                .join(format!("ares-appa-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            TempDir(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A tool whose execution is observable: the ran flag is the invocation
    /// count the invariant tests read, and the canary file is the side effect
    /// a denied call must not produce.
    struct CanaryTool {
        name: String,
        ran: Arc<AtomicBool>,
        body: Value,
        canary_file: Option<PathBuf>,
    }

    #[async_trait]
    impl Tool for CanaryTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "canary"
        }

        fn parameters_schema(&self) -> Value {
            json!({ "type": "object", "properties": {} })
        }

        async fn execute(&self, _args: Value) -> ares_types::types::Result<Value> {
            self.ran.store(true, Ordering::SeqCst);
            if let Some(path) = &self.canary_file {
                std::fs::write(path, b"the side effect landed")
                    .expect("the canary file is writable");
            }
            Ok(self.body.clone())
        }
    }

    /// A file tool that reads what its `path` argument names, seeding the file
    /// when the session has not seen it yet.
    struct FileReader {
        ran: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Tool for FileReader {
        fn name(&self) -> &str {
            "read_file"
        }

        fn description(&self) -> &str {
            "reads a file"
        }

        fn parameters_schema(&self) -> Value {
            json!({ "type": "object", "properties": { "path": { "type": "string" } } })
        }

        async fn execute(&self, args: Value) -> ares_types::types::Result<Value> {
            self.ran.store(true, Ordering::SeqCst);
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .expect("the read names a path");
            if !path.exists() {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("the workspace is writable");
                }
                std::fs::write(&path, b"the file a session observed").expect("seed the read file");
            }
            Ok(json!({ "content": "the file a session observed" }))
        }
    }

    /// A file tool that writes what its `path` argument names.
    struct FileWriter {
        ran: Arc<AtomicBool>,
    }

    #[async_trait]
    impl Tool for FileWriter {
        fn name(&self) -> &str {
            "write_file"
        }

        fn description(&self) -> &str {
            "writes a file"
        }

        fn parameters_schema(&self) -> Value {
            json!({ "type": "object", "properties": { "path": { "type": "string" } } })
        }

        async fn execute(&self, args: Value) -> ares_types::types::Result<Value> {
            self.ran.store(true, Ordering::SeqCst);
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .expect("the write names a path");
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the workspace is writable");
            }
            std::fs::write(&path, b"the tool's output").expect("the write lands");
            Ok(json!({ "written": path }))
        }
    }

    fn policy_with_annotator(url: &str) -> String {
        TEST_ROOT_POLICY.replace("{annotator_url}", url)
    }

    /// A port nothing answers on: the battery-outage stand-in.
    fn closed_port_url() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a free local port");
        let port = listener.local_addr().expect("a bound address").port();
        drop(listener);
        format!("http://127.0.0.1:{port}/")
    }

    fn gate(root: &str, workspace: &Path) -> AppaGate {
        let fence_policy = FencePolicy::new(
            FenceMode::WorkspaceWrite,
            workspace,
            vec![".env".to_string(), "id_rsa".to_string()],
        );
        AppaGate::open(
            root,
            &[],
            HostDefaults::new(Duration::from_millis(2000), 65_536),
            |_| None,
            fence_policy,
        )
        .expect("the test policy opens under the ARES adapter")
    }

    fn canary(name: &str, body: Value, ran: &Arc<AtomicBool>) -> Arc<dyn Tool> {
        Arc::new(CanaryTool {
            name: name.to_string(),
            ran: Arc::clone(ran),
            body,
            canary_file: None,
        })
    }

    fn events_ctx() -> Arc<Context> {
        let ctx = Context::new_root();
        ctx.provide(EventsService::new());
        ctx
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_adapter_spells_the_control_tool_and_round_trips_ares_names() {
        // The control tool is spelled for the model, or the open fails closed.
        assert_eq!(
            spell(&CanonicalTool::control()).as_deref(),
            Some(REMEDY_TOOL_SPELLING)
        );
        let unspelled = Adapter {
            spell: |canonical| {
                (!canonical.is_control())
                    .then(|| spell(canonical))
                    .flatten()
            },
            ..adapter()
        };
        let config = Config::hosted_composed(
            &policy_with_annotator("http://127.0.0.1:1/"),
            &[],
            HostDefaults::new(Duration::from_millis(2000), 65_536),
            |_| None,
        )
        .expect("the hosted document composes");
        let store = Arc::new(LogStore::open(Backend::Memory).expect("an in-memory log opens"));
        let refused = Runtime::open_with_store_as(config, store, None, unspelled);
        assert!(
            matches!(refused, Err(OpenError::UnspelledControlTool)),
            "an adapter that spells no control tool is refused at open"
        );

        // The mapping is a bijection over the spellings ARES accepts: each
        // host spelling identifies once and spells back to itself.
        for raw in [
            "echo",
            "read_ticket",
            "publish_update",
            "mcp_github_get_file_contents",
            REMEDY_TOOL_SPELLING,
        ] {
            let identified = identify_tool(raw).expect("the spelling is in the domain");
            let spelled = spell(&identified.canonical).expect("and it spells back");
            assert_eq!(spelled, raw, "{raw} round-trips through the adapter");
        }
        // The canonical control id is what the engine keys on; both spellings
        // of it identify to the one control tool.
        for raw in [REMEDY_TOOL_SPELLING, CONTROL_TOOL] {
            let identified = identify_tool(raw).expect("the control spelling is in the domain");
            assert!(
                identified.canonical.is_control(),
                "{raw} identifies the control tool"
            );
        }
    }

    /// Invariant 1: a denied call never executes. The tool's execute path
    /// records zero invocations and the canary side effect is absent; the
    /// reverse control proves the allow path still runs.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_denied_call_never_executes_and_the_allow_path_still_runs() {
        let ws = TempDir::new("deny");
        let publish_ran = Arc::new(AtomicBool::new(false));
        let publish_canary = ws.path().join("published.txt");
        let read_ran = Arc::new(AtomicBool::new(false));

        let tools = Tools::with_appa(
            vec![
                Arc::new(CanaryTool {
                    name: "read_ticket".to_string(),
                    ran: Arc::clone(&read_ran),
                    body: json!({ "ticket": "acme-1" }),
                    canary_file: None,
                }) as Arc<dyn Tool>,
                Arc::new(CanaryTool {
                    name: "publish_update".to_string(),
                    ran: Arc::clone(&publish_ran),
                    body: json!({ "published": true }),
                    canary_file: Some(publish_canary.clone()),
                }) as Arc<dyn Tool>,
            ],
            gate(&policy_with_annotator(&closed_port_url()), ws.path()),
        );
        let ctx = events_ctx();

        // Control: on a fresh trajectory the same call is allowed — a gate
        // that blocked everything would pass this test vacuously.
        let fresh = tools
            .execute(&ctx, "publish_update", json!({}))
            .await
            .expect("a fresh trajectory's audience is public");
        assert_eq!(fresh, json!({ "published": true }));
        assert!(
            publish_ran.load(Ordering::SeqCst),
            "the control call executed"
        );

        // The control arm above wrote the canary; the deny arm asserts the
        // absence of ITS side effect, so clear the control's first.
        let _ = std::fs::remove_file(&publish_canary);
        publish_ran.store(false, Ordering::SeqCst);
        let blocked = tools
            .execute(&ctx, "read_ticket", json!({}))
            .await
            .expect("a narrowing is offered, not applied silently");
        let feedback = blocked
            .get("denied")
            .and_then(Value::as_str)
            .expect("the block names its offers")
            .to_string();
        let offers = blocked
            .get("offers")
            .and_then(Value::as_array)
            .expect("the block carries its offers");
        let accepted = offers
            .iter()
            .filter_map(Value::as_str)
            .find(|offer| feedback.contains(offer))
            .expect("the feedback names a standing offer")
            .to_string();
        let took = tools
            .execute(&ctx, REMEDY_TOOL_SPELLING, json!({ "offer_id": accepted }))
            .await
            .expect("the control call is answered");
        assert_eq!(
            took.get("remedy").and_then(Value::as_str),
            Some("authorized"),
            "the accepted narrowing executes: {took}"
        );

        // The same read now runs.
        let read = tools
            .execute(&ctx, "read_ticket", json!({}))
            .await
            .expect("the accepted read is allowed");
        assert_eq!(read, json!({ "ticket": "acme-1" }));
        assert!(read_ran.load(Ordering::SeqCst));

        // The egress to the world is now denied: the tool never runs and the
        // feedback reaches the model as the call's result.
        let denied = tools
            .execute(&ctx, "publish_update", json!({}))
            .await
            .expect("a denial is a result the model reads, not an error");
        assert!(
            denied.get("denied").and_then(Value::as_str).is_some(),
            "the denial carries the engine's feedback: {denied}"
        );
        assert!(
            !publish_ran.load(Ordering::SeqCst),
            "the denied tool's execute path records zero invocations"
        );
        assert!(
            !publish_canary.exists(),
            "the denied call's side effect is absent"
        );
    }

    /// Invariant 2: a blocked result never reaches the model. The workflow is
    /// the engine's own: a restricted result from a confined tool offers a
    /// withhold plan, the model takes it on the control tool, and the admitted
    /// value is the sanitizer's derivation — never the raw bytes.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_withheld_result_never_reaches_the_model() {
        let ws = TempDir::new("withhold");
        let read_ran = Arc::new(AtomicBool::new(false));
        let body = json!({ "ticket": "acme-7", "api_key": "AKIAIOSFODNN7EXAMPLE" });

        let tools = Tools::with_appa(
            vec![Arc::new(CanaryTool {
                name: "read_ticket".to_string(),
                ran: Arc::clone(&read_ran),
                body,
                canary_file: None,
            }) as Arc<dyn Tool>],
            gate(&policy_with_annotator(&closed_port_url()), ws.path()),
        );
        let ctx = events_ctx();

        // The first call is blocked with the withhold offer; the tool has not
        // run yet.
        let first = tools
            .execute(&ctx, "read_ticket", json!({}))
            .await
            .expect("a block is a result the model reads");
        let feedback = first
            .get("denied")
            .and_then(Value::as_str)
            .expect("the block names the withhold plan")
            .to_string();
        assert!(
            feedback.contains(REMEDY_TOOL_SPELLING),
            "the model is told to take the remedy through the control tool: {feedback}"
        );
        let offers = first
            .get("offers")
            .and_then(Value::as_array)
            .expect("the block carries its offers");
        // The offer that routes the result through the redaction battery is
        // the one the sanitizer's feedback line names; the accept-narrowing
        // offer would admit the raw value instead.
        // The feedback spells each route as a label line followed by the
        // control-tool line carrying its offer id, so the id for the
        // sanitizer route is the first offer id after that label.
        let route_at = feedback
            .find("Use sanitizer redact_private")
            .expect("the block offers the sanitizer route");
        let route_text = &feedback[route_at..];
        let mut best: Option<(usize, &str)> = None;
        for offer in offers.iter().filter_map(Value::as_str) {
            if let Some(at) = route_text.find(offer)
                && best.is_none_or(|(best_at, _)| at < best_at)
            {
                best = Some((at, offer));
            }
        }
        let offer = best
            .expect("the sanitizer route names its offer")
            .1
            .to_string();
        assert!(
            !read_ran.load(Ordering::SeqCst),
            "the blocked call has not executed"
        );

        // The model takes the offer on the control tool; the engine executes
        // the plan inside its mandate envelope.
        let taken = tools
            .execute(&ctx, REMEDY_TOOL_SPELLING, json!({ "offer_id": offer }))
            .await
            .expect("the control call is answered");
        assert_eq!(
            taken.get("remedy").and_then(Value::as_str),
            Some("authorized"),
            "the accepted remedy executes: {taken}"
        );

        // The same call now runs; the admitted value is the sanitizer's
        // derivation. The raw secret never re-enters the conversation.
        let admitted = tools
            .execute(&ctx, "read_ticket", json!({}))
            .await
            .expect("the accepted call runs");
        let admitted_text = admitted.to_string();
        assert!(read_ran.load(Ordering::SeqCst), "the tool executed once");
        assert!(
            !admitted_text.contains("AKIAIOSFODNN7EXAMPLE"),
            "the raw secret never reaches the model: {admitted_text}"
        );
        assert!(
            admitted_text.contains("[redacted-secret]"),
            "the admitted value is the sanitizer's derivation: {admitted_text}"
        );
    }

    /// Invariant 3: a battery outage stops the flow with a refusal — never a
    /// default allow — and the healthy-battery reverse control proves the same
    /// call proceeds.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_battery_outage_refuses_the_turn_and_a_healthy_battery_proceeds() {
        let ws = TempDir::new("outage");
        let ran = Arc::new(AtomicBool::new(false));
        let ctx = events_ctx();
        let answer = json!({ "note": "written" });

        // Outage arm: the classification battery's port answers nothing.
        let dark = Tools::with_appa(
            canary_registry("notes", answer.clone(), &ran),
            gate(&policy_with_annotator(&closed_port_url()), ws.path()),
        );
        let refused = dark.execute(&ctx, "notes", json!({})).await;
        let error = refused.expect_err("an unanswered battery stops the flow; it never allows");
        assert!(
            error.to_string().contains("appa"),
            "the refusal is the runtime's own words: {error}"
        );
        assert!(
            !ran.load(Ordering::SeqCst),
            "the refused call never executes"
        );

        // Healthy arm: the same call against a live battery proceeds.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"version":1,"answer":{"delta":{},"requires":{"history":[],"attention":[]},"emits":[]}}"#,
            ))
            .mount(&server)
            .await;
        let live = Tools::with_appa(
            canary_registry("notes", answer, &ran),
            gate(&policy_with_annotator(&server.uri()), ws.path()),
        );
        let allowed = live
            .execute(&ctx, "notes", json!({}))
            .await
            .expect("the healthy battery answers and the call proceeds");
        assert_eq!(allowed, json!({ "note": "written" }));
        assert!(
            ran.load(Ordering::SeqCst),
            "the same call proceeds against a healthy battery"
        );
    }

    fn canary_registry(name: &str, body: Value, ran: &Arc<AtomicBool>) -> Vec<Arc<dyn Tool>> {
        vec![canary(name, body, ran)]
    }

    /// The fence is the write-contract layer the post-call check rides on: a
    /// write whose path the trajectory observed is admitted, and a write that
    /// never read its path is withheld with the violation in the audit ring.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_outside_the_write_contract_is_withheld() {
        let ws = TempDir::new("fence");
        let ran = Arc::new(AtomicBool::new(false));
        let read_path = ws.path().join("reads").join("notes.txt");
        let blind_path = ws.path().join("writes").join("blind.txt");

        let tools = Tools::with_appa(
            vec![
                Arc::new(FileReader {
                    ran: Arc::clone(&ran),
                }) as Arc<dyn Tool>,
                Arc::new(FileWriter {
                    ran: Arc::clone(&ran),
                }) as Arc<dyn Tool>,
            ],
            gate(&policy_with_annotator(&closed_port_url()), ws.path()),
        );
        let ctx = events_ctx();
        let actor = tools
            .appa_gate()
            .expect("the gate is attached")
            .actor_for(None);

        // Read-before-edit: the observation is registered by the read.
        let read = tools
            .execute(&ctx, "read_file", json!({ "path": read_path }))
            .await
            .expect("the read is admitted");
        assert!(read.get("content").is_some());

        // The guarded write to the observed path satisfies its contract.
        let written = tools
            .execute(&ctx, "write_file", json!({ "path": read_path }))
            .await
            .expect("a write to an observed path is admitted");
        assert!(written.get("written").is_some());

        // A blind write to an unobserved path is withheld: the tool ran (the
        // check is post-call by design), but its result never reaches the
        // model and the violation lands in the audit ring.
        let withheld = tools
            .execute(&ctx, "write_file", json!({ "path": blind_path }))
            .await
            .expect_err("a write with no observation is a contract violation");
        let text = withheld.to_string();
        assert!(
            text.contains("fence FS_NOT_OBSERVED"),
            "the withheld write names the violated contract: {text}"
        );
        assert!(
            !text.contains("the tool's output"),
            "the withheld write's result never re-enters the conversation"
        );
        let audit = tools
            .appa_gate()
            .expect("the gate is attached")
            .fence_audit(&actor);
        assert!(
            audit
                .iter()
                .any(|entry| entry.outcome == crate::fence::FsError::FS_NOT_OBSERVED),
            "the violation lands in the fence's audit ring"
        );
    }

    /// The hook path is never silently unguarded: the tools.execute event
    /// waterfall the policy's audit listeners ride on is bypassed without
    /// EventsService, so a configured gate without it fails loudly instead of
    /// running unguarded.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_hook_path_without_events_service_fails_loudly() {
        let ws = TempDir::new("no-events");
        let ran = Arc::new(AtomicBool::new(false));
        let tools = Tools::with_appa(
            canary_registry("echo", json!({ "echo": true }), &ran),
            gate(&policy_with_annotator(&closed_port_url()), ws.path()),
        );
        let ctx = Context::new_root();
        let error = tools
            .execute(&ctx, "echo", json!({}))
            .await
            .expect_err("a configured gate without EventsService is a configuration fault");
        assert!(
            error.to_string().contains("EventsService"),
            "the fault names the missing registration: {error}"
        );
        assert!(!ran.load(Ordering::SeqCst), "the tool never runs unguarded");
    }

    /// The engine's own default-deny: a tool no rule names is refused before
    /// it runs, which is an operational stop rather than a policy judgment.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_undeclared_tool_is_refused_before_it_runs() {
        let ws = TempDir::new("undeclared");
        let ran = Arc::new(AtomicBool::new(false));
        let tools = Tools::with_appa(
            canary_registry("unknown_tool", json!({ "ran": true }), &ran),
            gate(&policy_with_annotator(&closed_port_url()), ws.path()),
        );
        let ctx = events_ctx();
        let error = tools
            .execute(&ctx, "unknown_tool", json!({}))
            .await
            .expect_err("an undeclared tool is refused");
        assert!(error.to_string().contains("appa"), "{error}");
        assert!(!ran.load(Ordering::SeqCst), "the refused tool never runs");
    }
}
