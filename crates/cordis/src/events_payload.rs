//! Typed event payloads bound to the declared catalog via [`TypedEvent`].
//!
//! Upstream Cordis binds payload types to events through TypeScript
//! declaration merging with an `@mode` contract. Rust has no declaration
//! merging; the equivalent here is one zero-sized marker type per catalog
//! event implementing [`TypedEvent`] with an associated payload struct plus
//! `NAME`/`MODE`/`AROUND` constants. A unit test enforces that every binding
//! matches [`crate::events_catalog::CONTRACTS`], keeping the catalog the
//! single source of truth.
//!
//! Dispatch sites construct the payload struct and call
//! [`crate::EventsService::dispatch_typed`]; listeners register through
//! [`crate::EventsService::on_typed`] / [`crate::EventsService::on_typed_waterfall`],
//! which deserialize into the payload type and skip malformed payloads
//! (warn + passthrough) instead of failing the chain. The raw
//! `serde_json::Value` API stays authoritative for kernel mechanics, dynamic
//! cases, and mid-chain JSON rewriting.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::Dispatch;

/// Compile-time binding between a catalog event and its payload type.
///
/// `NAME`/`MODE`/`AROUND` must mirror the event's
/// [`crate::events_catalog::EventContract`]; the
/// `typed_events_match_catalog_contracts` test fails on drift.
pub trait TypedEvent {
    /// Wire shape of the event payload.
    type Payload: Serialize + DeserializeOwned + Clone + Send + Sync + 'static;
    /// Canonical event name (an `events_catalog::ev::*` constant).
    const NAME: &'static str;
    /// Declared dispatch mode.
    const MODE: Dispatch;
    /// True for around-middleware waterfalls (`on_waterfall` registry).
    const AROUND: bool;
}

// ---------------------------------------------------------------------------
// agent.admit — Dispatch::Bail
// ---------------------------------------------------------------------------

/// Payload for [`AgentAdmitEvent`]. Built by the shared admission gate
/// (`ares-agent::admit`) and the MCP server quota gate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentAdmitPayload {
    pub tenant_id: String,
    #[serde(default)]
    pub monthly: u64,
    #[serde(default)]
    pub daily: u64,
    #[serde(default)]
    pub requests_per_month: Option<u64>,
    #[serde(default)]
    pub requests_per_day: Option<u64>,
    pub tier: String,
}

/// `agent.admit` — quota admission policy (`Dispatch::Bail`). A handler
/// denying the request returns a payload with a `deny`/`error` marker.
#[derive(Debug, Clone, Copy)]
pub struct AgentAdmitEvent;
impl TypedEvent for AgentAdmitEvent {
    type Payload = AgentAdmitPayload;
    const NAME: &'static str = crate::events_catalog::ev::AGENT_ADMIT;
    const MODE: Dispatch = Dispatch::Bail;
    const AROUND: bool = false;
}

// ---------------------------------------------------------------------------
// agent.started — Dispatch::Parallel
// ---------------------------------------------------------------------------

/// Payload for [`AgentStartedEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStartedPayload {
    pub agent_name: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub tenant: String,
    #[serde(default)]
    pub event: String,
}

/// `agent.started` — joined fan-out at run start (`Dispatch::Parallel`).
#[derive(Debug, Clone, Copy)]
pub struct AgentStartedEvent;
impl TypedEvent for AgentStartedEvent {
    type Payload = AgentStartedPayload;
    const NAME: &'static str = crate::events_catalog::ev::AGENT_STARTED;
    const MODE: Dispatch = Dispatch::Parallel;
    const AROUND: bool = false;
}

// ---------------------------------------------------------------------------
// agent.usage — Dispatch::Emit
// ---------------------------------------------------------------------------

/// Payload for [`AgentUsageEvent`]. `tenant` is `None` when no tenant scope
/// was resolved for the run (serialized as JSON `null`, matching the raw API).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentUsagePayload {
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub prompt: i64,
    #[serde(default)]
    pub completion: i64,
    #[serde(default)]
    pub total: i64,
}

/// `agent.usage` — fire-and-forget token accounting (`Dispatch::Emit`).
#[derive(Debug, Clone, Copy)]
pub struct AgentUsageEvent;
impl TypedEvent for AgentUsageEvent {
    type Payload = AgentUsagePayload;
    const NAME: &'static str = crate::events_catalog::ev::AGENT_USAGE;
    const MODE: Dispatch = Dispatch::Emit;
    const AROUND: bool = false;
}

// ---------------------------------------------------------------------------
// agent.completed / agent.failed — Dispatch::Emit
// ---------------------------------------------------------------------------

/// Payload for [`AgentCompletedEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCompletedPayload {
    pub agent_name: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub event: String,
}

fn default_status() -> String {
    "unknown".to_string()
}

/// `agent.completed` — terminal status for every run (`Dispatch::Emit`).
#[derive(Debug, Clone, Copy)]
pub struct AgentCompletedEvent;
impl TypedEvent for AgentCompletedEvent {
    type Payload = AgentCompletedPayload;
    const NAME: &'static str = crate::events_catalog::ev::AGENT_COMPLETED;
    const MODE: Dispatch = Dispatch::Emit;
    const AROUND: bool = false;
}

/// Payload for [`AgentFailedEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentFailedPayload {
    pub agent_name: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub tenant: String,
    #[serde(default)]
    pub event: String,
}

/// `agent.failed` — run failure signal feeding scheduler failure control
/// (`Dispatch::Emit`).
#[derive(Debug, Clone, Copy)]
pub struct AgentFailedEvent;
impl TypedEvent for AgentFailedEvent {
    type Payload = AgentFailedPayload;
    const NAME: &'static str = crate::events_catalog::ev::AGENT_FAILED;
    const MODE: Dispatch = Dispatch::Emit;
    const AROUND: bool = false;
}

// ---------------------------------------------------------------------------
// agent.run — Dispatch::Waterfall (around)
// ---------------------------------------------------------------------------

/// Initial payload for [`AgentRunEvent`]: the requested agent and message.
/// Waterfall handlers may rewrite either before the core runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRunRequest {
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub message: String,
}

/// Result payload produced by the [`AgentRunEvent`] waterfall core. Handlers
/// may set `deny` (with optional `reason`) to short-circuit the run, or read
/// `content`/`usage` from downstream results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRunResult {
    #[serde(default)]
    pub content: Value,
    #[serde(default)]
    pub source: Value,
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub usage: Option<Value>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

/// `agent.run` — around-middleware waterfall wrapping whole agent runs
/// (`Dispatch::Waterfall`).
#[derive(Debug, Clone, Copy)]
pub struct AgentRunEvent;
impl TypedEvent for AgentRunEvent {
    type Payload = AgentRunRequest;
    const NAME: &'static str = crate::events_catalog::ev::AGENT_RUN;
    const MODE: Dispatch = Dispatch::Waterfall;
    const AROUND: bool = true;
}

// ---------------------------------------------------------------------------
// llm.complete — Dispatch::Waterfall (around)
// ---------------------------------------------------------------------------

/// Initial payload for [`LlmCompleteEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmCompleteRequest {
    #[serde(default)]
    pub prompt: String,
}

/// Result payload of the [`LlmCompleteEvent`] waterfall core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmCompleteResult {
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub content: Value,
}

/// `llm.complete` — around-middleware waterfall wrapping single completions
/// (`Dispatch::Waterfall`). Handlers may rewrite `prompt` or short-circuit by
/// returning their own `content`.
#[derive(Debug, Clone, Copy)]
pub struct LlmCompleteEvent;
impl TypedEvent for LlmCompleteEvent {
    type Payload = LlmCompleteRequest;
    const NAME: &'static str = crate::events_catalog::ev::LLM_COMPLETE;
    const MODE: Dispatch = Dispatch::Waterfall;
    const AROUND: bool = true;
}

// ---------------------------------------------------------------------------
// llm.get_client — Dispatch::Waterfall (around)
// ---------------------------------------------------------------------------

/// Payload for [`LlmGetClientEvent`]. The core is identity; handlers may set
/// `deny` to refuse client resolution or pin `model`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmGetClientPayload {
    pub capability: String,
    #[serde(default)]
    pub deny: Option<bool>,
    #[serde(default)]
    pub model: Option<String>,
}

/// `llm.get_client` — around-middleware waterfall over client resolution
/// (`Dispatch::Waterfall`).
#[derive(Debug, Clone, Copy)]
pub struct LlmGetClientEvent;
impl TypedEvent for LlmGetClientEvent {
    type Payload = LlmGetClientPayload;
    const NAME: &'static str = crate::events_catalog::ev::LLM_GET_CLIENT;
    const MODE: Dispatch = Dispatch::Waterfall;
    const AROUND: bool = true;
}

// ---------------------------------------------------------------------------
// llm.generate / llm.generate_tools — Dispatch::Waterfall (around)
// ---------------------------------------------------------------------------

/// One conversation message in [`LlmGeneratePayload`]. Free-form shape: the
/// waterfall core re-parses messages into provider-native types, so `content`
/// stays a raw JSON value.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: String,
    #[serde(default)]
    pub content: Value,
    /// Multimodal parts as raw JSON so this leaf crate stays independent of
    /// `ares-types::ContentPart`. Empty means use `content` only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Value>,
}

/// Initial payload for [`LlmGenerateEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmGeneratePayload {
    #[serde(default)]
    pub messages: Vec<LlmMessage>,
}

/// `llm.generate` — around-middleware waterfall wrapping history generation
/// (`Dispatch::Waterfall`). Handlers may rewrite `messages` mid-chain.
#[derive(Debug, Clone, Copy)]
pub struct LlmGenerateEvent;
impl TypedEvent for LlmGenerateEvent {
    type Payload = LlmGeneratePayload;
    const NAME: &'static str = crate::events_catalog::ev::LLM_GENERATE;
    const MODE: Dispatch = Dispatch::Waterfall;
    const AROUND: bool = true;
}

/// Initial payload for [`LlmGenerateToolsEvent`]. Messages are serialized
/// conversation messages; tools are serialized tool definitions (kept as raw
/// values because this leaf crate cannot depend on the crates defining them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmGenerateToolsPayload {
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default)]
    pub tools: Vec<Value>,
}

/// `llm.generate_tools` — around-middleware waterfall wrapping tool-calling
/// generation (`Dispatch::Waterfall`).
#[derive(Debug, Clone, Copy)]
pub struct LlmGenerateToolsEvent;
impl TypedEvent for LlmGenerateToolsEvent {
    type Payload = LlmGenerateToolsPayload;
    const NAME: &'static str = crate::events_catalog::ev::LLM_GENERATE_TOOLS;
    const MODE: Dispatch = Dispatch::Waterfall;
    const AROUND: bool = true;
}

// ---------------------------------------------------------------------------
// llm.embed — Dispatch::Waterfall (around)
// ---------------------------------------------------------------------------

/// Initial payload for [`LlmEmbedEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmEmbedRequest {
    #[serde(default)]
    pub inputs: Vec<String>,
}

/// Result payload of the [`LlmEmbedEvent`] waterfall core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmEmbedResponse {
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub embeddings: Vec<Vec<f32>>,
}

/// `llm.embed` — around-middleware waterfall wrapping embedding batches
/// (`Dispatch::Waterfall`). Handlers may rewrite `inputs` or short-circuit by
/// returning their own `embeddings`. Core calls `LLMClient::embed`.
#[derive(Debug, Clone, Copy)]
pub struct LlmEmbedEvent;
impl TypedEvent for LlmEmbedEvent {
    type Payload = LlmEmbedRequest;
    const NAME: &'static str = crate::events_catalog::ev::LLM_EMBED;
    const MODE: Dispatch = Dispatch::Waterfall;
    const AROUND: bool = true;
}

// ---------------------------------------------------------------------------
// tools.execute / tools.list / tools.resolve — Dispatch::Waterfall (around)
// ---------------------------------------------------------------------------

/// Initial payload for [`ToolsExecuteEvent`]. The core injects `result`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolsExecutePayload {
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

/// `tools.execute` — around-middleware waterfall wrapping tool execution
/// (`Dispatch::Waterfall`).
#[derive(Debug, Clone, Copy)]
pub struct ToolsExecuteEvent;
impl TypedEvent for ToolsExecuteEvent {
    type Payload = ToolsExecutePayload;
    const NAME: &'static str = crate::events_catalog::ev::TOOLS_EXECUTE;
    const MODE: Dispatch = Dispatch::Waterfall;
    const AROUND: bool = true;
}

/// Initial payload for [`ToolsListEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolsListRequest {
    #[serde(default)]
    pub tenant: Option<String>,
}

/// Result payload of the [`ToolsListEvent`] waterfall core: serialized tool
/// definitions visible to the tenant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolsListResult {
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub tools: Vec<Value>,
}

/// `tools.list` — around-middleware waterfall wrapping tool listing
/// (`Dispatch::Waterfall`).
#[derive(Debug, Clone, Copy)]
pub struct ToolsListEvent;
impl TypedEvent for ToolsListEvent {
    type Payload = ToolsListRequest;
    const NAME: &'static str = crate::events_catalog::ev::TOOLS_LIST;
    const MODE: Dispatch = Dispatch::Waterfall;
    const AROUND: bool = true;
}

/// Initial payload for [`ToolsResolveEvent`]. The core adds `found`; handlers
/// may set `deny` to block resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolsResolveRequest {
    pub name: String,
    #[serde(default)]
    pub tenant: Option<String>,
}

/// `tools.resolve` — around-middleware waterfall wrapping tool resolution
/// (`Dispatch::Waterfall`).
#[derive(Debug, Clone, Copy)]
pub struct ToolsResolveEvent;
impl TypedEvent for ToolsResolveEvent {
    type Payload = ToolsResolveRequest;
    const NAME: &'static str = crate::events_catalog::ev::TOOLS_RESOLVE;
    const MODE: Dispatch = Dispatch::Waterfall;
    const AROUND: bool = true;
}

// ---------------------------------------------------------------------------
// scheduler.before_run / scheduler.admit
// ---------------------------------------------------------------------------

/// Payload for [`SchedulerBeforeRunEvent`]: a scheduled run about to execute.
/// Waterfall handlers may enrich fields before the executor reads them back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchedulerBeforeRunPayload {
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub tenant: Option<String>,
}

/// `scheduler.before_run` — around-middleware waterfall ahead of scheduled
/// execution (`Dispatch::Waterfall`).
#[derive(Debug, Clone, Copy)]
pub struct SchedulerBeforeRunEvent;
impl TypedEvent for SchedulerBeforeRunEvent {
    type Payload = SchedulerBeforeRunPayload;
    const NAME: &'static str = crate::events_catalog::ev::SCHEDULER_BEFORE_RUN;
    const MODE: Dispatch = Dispatch::Waterfall;
    const AROUND: bool = true;
}

/// Payload for [`SchedulerAdmitEvent`]. A handler denying the run returns a
/// payload with `deny: true`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchedulerAdmitPayload {
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub deny: Option<bool>,
}

/// `scheduler.admit` — admission policy for scheduled runs (`Dispatch::Bail`).
#[derive(Debug, Clone, Copy)]
pub struct SchedulerAdmitEvent;
impl TypedEvent for SchedulerAdmitEvent {
    type Payload = SchedulerAdmitPayload;
    const NAME: &'static str = crate::events_catalog::ev::SCHEDULER_ADMIT;
    const MODE: Dispatch = Dispatch::Bail;
    const AROUND: bool = false;
}

// ---------------------------------------------------------------------------
// service.changed — Dispatch::Emit
// ---------------------------------------------------------------------------

/// Payload for [`ServiceChangedEvent`]. `type_id` is the `Debug` rendering of
/// the changed service's `TypeId` (matches the raw API's `format!("{tid:?}")`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceChangedPayload {
    pub type_id: String,
    #[serde(default)]
    pub event: String,
}

// ---------------------------------------------------------------------------
// Engine boundary events — Dispatch::Emit (fire-and-forget observability)
// ---------------------------------------------------------------------------

/// Payload for [`SchedulerTickEvent`]: counts from one due-schedule pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SchedulerTickPayload {
    #[serde(default)]
    pub due_count: u64,
    #[serde(default)]
    pub catchup_count: u64,
}

/// `scheduler.tick` — emitted once per completed scheduler execution pass
/// (`Dispatch::Emit`).
#[derive(Debug, Clone, Copy)]
pub struct SchedulerTickEvent;
impl TypedEvent for SchedulerTickEvent {
    type Payload = SchedulerTickPayload;
    const NAME: &'static str = crate::events_catalog::ev::SCHEDULER_TICK;
    const MODE: Dispatch = Dispatch::Emit;
    const AROUND: bool = false;
}

/// Payload for [`ScheduleDispatchedEvent`]: outcome of one scheduled run.
/// `denied: true` means admission policy skipped the run; a denial is not a
/// failure and still advances the schedule's next_run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScheduleDispatchedPayload {
    pub schedule_id: String,
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub is_catchup: bool,
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub denied: bool,
    #[serde(default)]
    pub error: Option<String>,
}

/// `scheduler.schedule.dispatched` — emitted after each scheduled-run attempt
/// (`Dispatch::Emit`).
#[derive(Debug, Clone, Copy)]
pub struct ScheduleDispatchedEvent;
impl TypedEvent for ScheduleDispatchedEvent {
    type Payload = ScheduleDispatchedPayload;
    const NAME: &'static str = crate::events_catalog::ev::SCHEDULER_SCHEDULE_DISPATCHED;
    const MODE: Dispatch = Dispatch::Emit;
    const AROUND: bool = false;
}

/// Payload for [`PipelineStepStartedEvent`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStepStartedPayload {
    pub pipeline_id: String,
    pub target_agent: String,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub run_id: String,
}

/// `pipeline.step.started` — emitted just before each pipeline target runs
/// (`Dispatch::Emit`).
#[derive(Debug, Clone, Copy)]
pub struct PipelineStepStartedEvent;
impl TypedEvent for PipelineStepStartedEvent {
    type Payload = PipelineStepStartedPayload;
    const NAME: &'static str = crate::events_catalog::ev::PIPELINE_STEP_STARTED;
    const MODE: Dispatch = Dispatch::Emit;
    const AROUND: bool = false;
}

/// Payload for [`PipelineStepFinishedEvent`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStepFinishedPayload {
    pub pipeline_id: String,
    pub target_agent: String,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub error: Option<String>,
}

/// `pipeline.step.finished` — emitted after each pipeline target's status is
/// known (`Dispatch::Emit`).
#[derive(Debug, Clone, Copy)]
pub struct PipelineStepFinishedEvent;
impl TypedEvent for PipelineStepFinishedEvent {
    type Payload = PipelineStepFinishedPayload;
    const NAME: &'static str = crate::events_catalog::ev::PIPELINE_STEP_FINISHED;
    const MODE: Dispatch = Dispatch::Emit;
    const AROUND: bool = false;
}

/// Payload for [`PipelineFanoutCompletedEvent`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PipelineFanoutCompletedPayload {
    #[serde(default)]
    pub source_agent: String,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub triggered: Vec<String>,
}

/// `pipeline.fanout.completed` — emitted at the end of a pipeline fan-out
/// (`Dispatch::Emit`).
#[derive(Debug, Clone, Copy)]
pub struct PipelineFanoutCompletedEvent;
impl TypedEvent for PipelineFanoutCompletedEvent {
    type Payload = PipelineFanoutCompletedPayload;
    const NAME: &'static str = crate::events_catalog::ev::PIPELINE_FANOUT_COMPLETED;
    const MODE: Dispatch = Dispatch::Emit;
    const AROUND: bool = false;
}

/// Payload for [`TriggerFiredEvent`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerFiredPayload {
    pub trigger_id: String,
    #[serde(default)]
    pub event_type: String,
    #[serde(default)]
    pub target_agent: String,
    #[serde(default)]
    pub tenant_id: String,
}

/// `trigger.fired` — emitted when a trigger executes its target agent
/// successfully (`Dispatch::Emit`).
#[derive(Debug, Clone, Copy)]
pub struct TriggerFiredEvent;
impl TypedEvent for TriggerFiredEvent {
    type Payload = TriggerFiredPayload;
    const NAME: &'static str = crate::events_catalog::ev::TRIGGER_FIRED;
    const MODE: Dispatch = Dispatch::Emit;
    const AROUND: bool = false;
}

/// `service.changed` — hot-reload notification emitted by ReflectService
/// (`Dispatch::Emit`).
#[derive(Debug, Clone, Copy)]
pub struct ServiceChangedEvent;
impl TypedEvent for ServiceChangedEvent {
    type Payload = ServiceChangedPayload;
    const NAME: &'static str = crate::events_catalog::ev::SERVICE_CHANGED;
    const MODE: Dispatch = Dispatch::Emit;
    const AROUND: bool = false;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events_catalog::{contract_for, CONTRACTS};

    /// Every typed binding must exist in the catalog and agree on mode and
    /// middleware shape, and every catalog event must have a typed binding.
    #[test]
    fn typed_events_match_catalog_contracts() {
        let bindings: &[(&'static str, Dispatch, bool)] = &[
            (
                AgentAdmitEvent::NAME,
                AgentAdmitEvent::MODE,
                AgentAdmitEvent::AROUND,
            ),
            (
                AgentCompletedEvent::NAME,
                AgentCompletedEvent::MODE,
                AgentCompletedEvent::AROUND,
            ),
            (
                AgentFailedEvent::NAME,
                AgentFailedEvent::MODE,
                AgentFailedEvent::AROUND,
            ),
            (
                AgentRunEvent::NAME,
                AgentRunEvent::MODE,
                AgentRunEvent::AROUND,
            ),
            (
                AgentStartedEvent::NAME,
                AgentStartedEvent::MODE,
                AgentStartedEvent::AROUND,
            ),
            (
                AgentUsageEvent::NAME,
                AgentUsageEvent::MODE,
                AgentUsageEvent::AROUND,
            ),
            (
                LlmCompleteEvent::NAME,
                LlmCompleteEvent::MODE,
                LlmCompleteEvent::AROUND,
            ),
            (
                LlmGetClientEvent::NAME,
                LlmGetClientEvent::MODE,
                LlmGetClientEvent::AROUND,
            ),
            (
                LlmGenerateEvent::NAME,
                LlmGenerateEvent::MODE,
                LlmGenerateEvent::AROUND,
            ),
            (
                LlmGenerateToolsEvent::NAME,
                LlmGenerateToolsEvent::MODE,
                LlmGenerateToolsEvent::AROUND,
            ),
            (
                LlmEmbedEvent::NAME,
                LlmEmbedEvent::MODE,
                LlmEmbedEvent::AROUND,
            ),
            (
                SchedulerAdmitEvent::NAME,
                SchedulerAdmitEvent::MODE,
                SchedulerAdmitEvent::AROUND,
            ),
            (
                SchedulerBeforeRunEvent::NAME,
                SchedulerBeforeRunEvent::MODE,
                SchedulerBeforeRunEvent::AROUND,
            ),
            (
                ServiceChangedEvent::NAME,
                ServiceChangedEvent::MODE,
                ServiceChangedEvent::AROUND,
            ),
            (
                ToolsExecuteEvent::NAME,
                ToolsExecuteEvent::MODE,
                ToolsExecuteEvent::AROUND,
            ),
            (
                ToolsListEvent::NAME,
                ToolsListEvent::MODE,
                ToolsListEvent::AROUND,
            ),
            (
                ToolsResolveEvent::NAME,
                ToolsResolveEvent::MODE,
                ToolsResolveEvent::AROUND,
            ),
            (
                SchedulerTickEvent::NAME,
                SchedulerTickEvent::MODE,
                SchedulerTickEvent::AROUND,
            ),
            (
                ScheduleDispatchedEvent::NAME,
                ScheduleDispatchedEvent::MODE,
                ScheduleDispatchedEvent::AROUND,
            ),
            (
                PipelineStepStartedEvent::NAME,
                PipelineStepStartedEvent::MODE,
                PipelineStepStartedEvent::AROUND,
            ),
            (
                PipelineStepFinishedEvent::NAME,
                PipelineStepFinishedEvent::MODE,
                PipelineStepFinishedEvent::AROUND,
            ),
            (
                PipelineFanoutCompletedEvent::NAME,
                PipelineFanoutCompletedEvent::MODE,
                PipelineFanoutCompletedEvent::AROUND,
            ),
            (
                TriggerFiredEvent::NAME,
                TriggerFiredEvent::MODE,
                TriggerFiredEvent::AROUND,
            ),
        ];
        for (name, mode, around) in bindings {
            let contract = contract_for(name)
                .unwrap_or_else(|| panic!("typed binding {name} missing from catalog"));
            assert_eq!(&contract.mode, mode, "mode drift for {name}");
            assert_eq!(&contract.around, around, "around drift for {name}");
        }
        assert_eq!(
            bindings.len(),
            CONTRACTS.len(),
            "every catalog event must have exactly one typed binding"
        );
    }

    /// Serialization keeps field names/types stable across a round trip, and
    /// missing optional keys fall back to documented defaults (lenient
    /// listener behavior preserved).
    #[test]
    fn payload_round_trip_and_defaults() {
        let started = AgentStartedPayload {
            agent_name: "a".into(),
            run_id: "r".into(),
            tenant: "t".into(),
            event: crate::events_catalog::ev::AGENT_STARTED.into(),
        };
        let v = serde_json::to_value(&started).unwrap();
        assert_eq!(v.get("agent_name").and_then(Value::as_str), Some("a"));
        let back: AgentStartedPayload = serde_json::from_value(v).unwrap();
        assert_eq!(back, started);

        let minimal: AgentCompletedPayload =
            serde_json::from_value(serde_json::json!({ "agent_name": "x" })).unwrap();
        assert_eq!(minimal.status, "unknown");
        assert_eq!(minimal.run_id, "");

        let usage = AgentUsagePayload {
            tenant: None,
            prompt: 3,
            completion: 4,
            total: 7,
        };
        let v = serde_json::to_value(&usage).unwrap();
        assert!(v.get("tenant").map(Value::is_null).unwrap_or(false));
        let back: AgentUsagePayload = serde_json::from_value(v).unwrap();
        assert_eq!(back.prompt, 3);
        assert!(back.tenant.is_none());
    }
}
