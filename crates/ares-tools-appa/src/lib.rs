//! The OpenAPPA pre-call tool gate for ARES.
//!
//! Split out of `ares-tools` because the upstream crates
//! (`archestra-ai/OpenAPPA`) are git-only and their workspace sets
//! `publish = false`. Cargo refuses to package any manifest carrying a git
//! dependency — optional or not — so keeping this code in `ares-tools` would
//! make that crate unpublishable, and every crate above it with it.
//!
//! `ares-tools` owns the [`ares_tools::gate::ToolGate`] contract and names no
//! engine type. This crate implements that contract against OpenAPPA, so the
//! deployed server keeps its gate while the published crates stay clean.

pub mod appa;
pub mod builder;

pub use appa::{AppaGate, REMEDY_TOOL_SPELLING};
pub use builder::install;
