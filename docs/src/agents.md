# Agents

This chapter describes the agent configuration model in ARES v0.10.0.
Every field here is read from the source files named in each section.

Agent behavior comes from TOML configuration. The root file `ares.toml` carries an `[agents]` table. You can also keep agents in TOON files under `config/agents/`. The `config.agents_dir` key in `ares.toml` sets that directory (`crates/ares-http/src/overlay.rs`, `DynamicConfigPaths`).

## Agent fields

Each agent entry deserializes into `AgentConfig` (`crates/ares-agent/src/config.rs`):

| Field | Type | Default | Meaning |
|---|---|---|---|
| `model` | string | required | Model name defined under `[models]`. |
| `system_prompt` | string | none | Personality and instructions for the agent. |
| `tools` | list of strings | empty | Tool names this agent uses. |
| `allowed_tools` | list of strings | all tools | Whitelist of permitted tool names. Absent means all tools are allowed. |
| `max_tool_iterations` | integer | 10 | Maximum tool-calling rounds before the agent stops. |
| `parallel_tools` | boolean | false | Run independent tool calls in parallel when possible. |
| `compaction_enabled` | boolean | false | Turn on per-session history compaction through the LLM `Compactor`. Long conversations stay a bounded working set instead of a last-5 history slice. |
| *(extra keys)* | table | empty | Unknown keys pass through unchanged via `#[serde(flatten)]`. |

## Agent resolution order

The resolver (`crates/ares-agent/src/resolver.rs`) picks one agent definition from three tiers:

1. Tenant database row (`tenant_db tenant_agents`).
2. Community public agent.
3. System agent from static TOML/TOON configuration.

The first tier that holds the name wins. The scope comes from the request isolate label, then a tenant context intercept, then a fallback id.

## Skills configuration

The `[skills]` group deserializes into `SkillsTomlConfig` (`crates/ares-agent/src/workflows_config.rs`):

- `project_dir` — project skills directory, for example `./.claude/skills/`.
- `personal_dir` — personal skills directory, for example `~/.claude/skills/`.
- `plugin_dirs` — extra directories to scan for `SKILL.md` files.

All three keys are optional.

## Workflows

A workflow defines how agents work together. Each entry under `[workflows.<name>]` deserializes into `WorkflowConfig` (`crates/ares-agent/src/workflows_config.rs`):

| Field | Type | Default | Meaning |
|---|---|---|---|
| `entry_agent` | string | required | Agent that receives the initial request. |
| `fallback_agent` | string | none | Agent used when routing fails or no match exists. |
| `max_depth` | integer | 3 | Maximum depth for nested workflows. |
| `max_iterations` | integer | 5 | Maximum iterations for iterative workflows. |
| `parallel_subagents` | boolean | false | Execute sub-agent calls in parallel. |

The engine (`crates/ares-agent/src/workflows/engine.rs`) runs steps through the unified executor and records a `WorkflowStep` per hop: `agent_name`, `input`, `output`, `timestamp`, `duration_ms`. The result is a `WorkflowOutput` with `final_response`, `steps_executed`, `agents_used`, and `reasoning_path`. Workflows live on the context as a `WorkflowSet`; the HTTP overlay copies `[workflows]` from TOML into it.

## Delegation flags

Skill delegation arguments parse into `DelegationArgs` (`crates/ares-agent/src/skills/mod.rs`). Three flags exist:

- `--parallel` — latches split-per-token mode. After the flag, every plain token starts its own task and `|` separators are ignored.
- `--model <value>` — consumes exactly one token as the model override. An error occurs when no value follows.
- `--tools` — enables the inner tool loop for delegated tasks.

Before `--parallel`, bare `|` separates sequential tasks. Double-quoted segments parse as single tokens and may contain spaces and `|`. Model resolution precedence: explicit flag > profile default > global default.

## Review gate for delegated results

`SkillsPluginConfig.review_delegated_results` (`crates/ares-agent/src/skills/mod.rs`) opts nested skill results into a review micro-step before integration:

- Off by default. A missing or failing reviewer passes the original result through.
- On acceptance, the original result integrates unchanged.
- On rejection, the parent receives a structured rejection with review notes; the original stays intact in metadata for re-dispatch.

## Self-check rounds

`SkillEngine::with_self_check_rounds(n)` (`crates/ares-agent/src/skills/engine.rs`) adds up to `n` critique rounds over each nested skill result before integration. One LLM call runs per round over a fixed template. A verbatim reply ends the loop early. An LLM failure keeps the last good answer silently. Off by default.

Delegated sub-workflows accept only allowlisted step kinds. Nested tool rounds hard-cap at three. Slash-command chatter lines are stripped from delegated text before it enters the parent context.

## Ambient enrichment

`AmbientEnrichmentConfig.enabled` (`crates/ares-agent/src/skills/engine.rs`) is off by default. When enabled, each LLM step completion fires two parallel micro calls after the answer: intent classification and keyword tagging. Outcomes attach as session metadata on the skill-step record under the `ambient_enrichment` key. Enrichment input truncates at 4000 characters. Failures log at debug level and never delay or fail the completion.

## Emergency stop

`EmergencyStop` (`crates/ares-agent/src/emergency_stop.rs`) is a Cordis service with an atomic flag. `set_active(true)` turns the kill switch on. Skill execution checks the switch at step boundaries together with per-subtask cancel tokens. An aborted subtask integrates nothing into the parent context.

## Worked example

Every key below appears in the structures cited above:

```toml
[server]
host = "127.0.0.1"
port = 3000
log_level = "info"

[providers.local]
type = "ollama"
base_url = "http://localhost:11434"
default_model = "llama3.1:8b"

[models.default]
provider = "local"
model = "llama3.1:8b"
temperature = 0.7
max_tokens = 512

[agents.helper]
model = "default"
system_prompt = "You answer questions about internal documents."
tools = ["calculator"]
allowed_tools = ["calculator"]
max_tool_iterations = 10
parallel_tools = false
compaction_enabled = true

[workflows.support]
entry_agent = "helper"
fallback_agent = "helper"
max_depth = 3
max_iterations = 5
parallel_subagents = false

[skills]
project_dir = "./.claude/skills/"
```
