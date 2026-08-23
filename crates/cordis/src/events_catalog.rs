//! Declared event contracts for the ARES event bus.
//!
//! Cordis events in upstream are typed via declaration merging with an
//! `@mode` contract checked at dispatch sites. Rust has no declaration
//! merging; the equivalent is a single declared catalog plus debug-only
//! validation inside [`EventsService`] entry points. Release builds pay
//! nothing: `debug_assert!` compiles out.
//!
//! Rules encoded here:
//! - unknown event names are undeclared (typo protection);
//! - `around == true` events are waterfall-only and must be dispatched with
//!   [`Dispatch::Waterfall`], registered via `on_waterfall`;
//! - non-around events forbid Waterfall;
//! - [`Dispatch::Serial`] and [`Dispatch::Bail`] share one runtime path, so
//!   Serial is accepted wherever Bail is declared.

use crate::events::Dispatch;

/// One declared event: canonical name, dispatch mode, middleware shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventContract {
    pub name: &'static str,
    pub mode: Dispatch,
    /// True for around-middleware waterfalls (`waterfall_around` / `on_waterfall`).
    pub around: bool,
}

/// Canonical event names. Production code references these instead of raw
/// string literals so renames are compile errors.
pub mod ev {
    pub const AGENT_ADMIT: &str = "agent.admit";
    pub const AGENT_STARTED: &str = "agent.started";
    pub const AGENT_USAGE: &str = "agent.usage";
    pub const AGENT_COMPLETED: &str = "agent.completed";
    pub const AGENT_FAILED: &str = "agent.failed";
    pub const AGENT_RUN: &str = "agent.run";
    pub const LLM_COMPLETE: &str = "llm.complete";
    pub const LLM_GET_CLIENT: &str = "llm.get_client";
    pub const LLM_GENERATE: &str = "llm.generate";
    pub const LLM_GENERATE_TOOLS: &str = "llm.generate_tools";
    pub const TOOLS_EXECUTE: &str = "tools.execute";
    pub const TOOLS_LIST: &str = "tools.list";
    pub const TOOLS_RESOLVE: &str = "tools.resolve";
    pub const SCHEDULER_BEFORE_RUN: &str = "scheduler.before_run";
    pub const SCHEDULER_ADMIT: &str = "scheduler.admit";
    pub const SERVICE_CHANGED: &str = "service.changed";
}

/// The 16 production events, verified by inventory across all crates.
pub const CONTRACTS: &[EventContract] = &[
    EventContract {
        name: ev::AGENT_ADMIT,
        mode: Dispatch::Bail,
        around: false,
    },
    EventContract {
        name: ev::AGENT_STARTED,
        mode: Dispatch::Parallel,
        around: false,
    },
    EventContract {
        name: ev::AGENT_USAGE,
        mode: Dispatch::Emit,
        around: false,
    },
    EventContract {
        name: ev::AGENT_COMPLETED,
        mode: Dispatch::Emit,
        around: false,
    },
    EventContract {
        name: ev::AGENT_FAILED,
        mode: Dispatch::Emit,
        around: false,
    },
    EventContract {
        name: ev::AGENT_RUN,
        mode: Dispatch::Waterfall,
        around: true,
    },
    EventContract {
        name: ev::LLM_COMPLETE,
        mode: Dispatch::Waterfall,
        around: true,
    },
    EventContract {
        name: ev::LLM_GET_CLIENT,
        mode: Dispatch::Waterfall,
        around: true,
    },
    EventContract {
        name: ev::LLM_GENERATE,
        mode: Dispatch::Waterfall,
        around: true,
    },
    EventContract {
        name: ev::LLM_GENERATE_TOOLS,
        mode: Dispatch::Waterfall,
        around: true,
    },
    EventContract {
        name: ev::TOOLS_EXECUTE,
        mode: Dispatch::Waterfall,
        around: true,
    },
    EventContract {
        name: ev::TOOLS_LIST,
        mode: Dispatch::Waterfall,
        around: true,
    },
    EventContract {
        name: ev::TOOLS_RESOLVE,
        mode: Dispatch::Waterfall,
        around: true,
    },
    EventContract {
        name: ev::SCHEDULER_BEFORE_RUN,
        mode: Dispatch::Waterfall,
        around: true,
    },
    EventContract {
        name: ev::SCHEDULER_ADMIT,
        mode: Dispatch::Bail,
        around: false,
    },
    EventContract {
        name: ev::SERVICE_CHANGED,
        mode: Dispatch::Emit,
        around: false,
    },
];

/// Look up the declared contract for `name`.
pub fn contract_for(name: &str) -> Option<&'static EventContract> {
    CONTRACTS.iter().find(|c| c.name == name)
}

/// Validate a dispatch of `name` with `mode`.
///
/// Serial is an alias of Bail at runtime, so it passes wherever Bail is
/// declared. Around events require Waterfall; flat events forbid it.
pub fn validate_dispatch(name: &str, mode: Dispatch) -> Result<(), String> {
    let Some(contract) = contract_for(name) else {
        return Err(format!("undeclared event '{name}'"));
    };
    match (contract.around, mode) {
        (true, Dispatch::Waterfall) => Ok(()),
        (true, other) => Err(format!(
            "event '{name}' is around-middleware; dispatch must be Waterfall, got {other:?}"
        )),
        (false, Dispatch::Waterfall) => Err(format!(
            "event '{name}' is not around-middleware; Waterfall dispatch is invalid"
        )),
        (false, _) => {
            // Bail-declared flat events accept Serial as its documented alias.
            if matches!(mode, Dispatch::Serial) && contract.mode != Dispatch::Bail {
                return Err(format!(
                    "event '{name}' declares {:?}; Serial is only an alias of Bail",
                    contract.mode
                ));
            }
            Ok(())
        }
    }
}

/// Validate a listener registration: `waterfall_registration` selects which
/// registry (`on_waterfall` vs `on`) the caller used.
pub fn validate_listener(name: &str, waterfall_registration: bool) -> Result<(), String> {
    let Some(contract) = contract_for(name) else {
        return Err(format!("undeclared event '{name}'"));
    };
    if contract.around && !waterfall_registration {
        return Err(format!(
            "event '{name}' is around-middleware; register with on_waterfall"
        ));
    }
    if !contract.around && waterfall_registration {
        return Err(format!(
            "event '{name}' is not around-middleware; register with on, not on_waterfall"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_names_are_unique() {
        let mut names: Vec<_> = CONTRACTS.iter().map(|c| c.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate contract names");
        assert_eq!(total, 16);
    }

    #[test]
    fn validate_dispatch_rejects_wrong_mode() {
        assert!(validate_dispatch(ev::AGENT_RUN, Dispatch::Parallel).is_err());
        assert!(validate_dispatch(ev::AGENT_RUN, Dispatch::Waterfall).is_ok());
        assert!(validate_dispatch(ev::TOOLS_EXECUTE, Dispatch::Emit).is_err());
        assert!(validate_dispatch(ev::AGENT_ADMIT, Dispatch::Waterfall).is_err());
    }

    #[test]
    fn serial_is_accepted_alias_of_bail_only() {
        assert!(validate_dispatch(ev::AGENT_ADMIT, Dispatch::Serial).is_ok());
        assert!(validate_dispatch(ev::AGENT_ADMIT, Dispatch::Bail).is_ok());
        assert!(validate_dispatch(ev::AGENT_STARTED, Dispatch::Serial).is_err());
    }

    #[test]
    fn validate_listener_catches_registry_split() {
        assert!(validate_listener(ev::TOOLS_EXECUTE, false).is_err());
        assert!(validate_listener(ev::TOOLS_EXECUTE, true).is_ok());
        assert!(validate_listener(ev::AGENT_ADMIT, true).is_err());
        assert!(validate_listener(ev::AGENT_ADMIT, false).is_ok());
    }

    #[test]
    fn unknown_names_are_undeclared() {
        assert!(contract_for("agent.runn").is_none());
        assert!(validate_dispatch("not-an-event", Dispatch::Emit)
            .unwrap_err()
            .contains("undeclared"));
        assert!(validate_listener("also-not-an-event", true).is_err());
    }
}
