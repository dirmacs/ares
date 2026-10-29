//! # ares-suriv - the Suriv language lexer, parser and code generator
//!
//! `ares-suriv` compiles Suriv surface text to Cordis `Entry` records plus the
//! separate `requires` coeffect map. It is the production port of the
//! prototype it was grown from: the module split, the closed phrase
//! table, the desugars and the golden fixtures are carried over
//! unchanged. The choices the implementation makes where the language's
//! specification is silent are documented with that specification.
//!
//! ## What this crate implements
//!
//! Implemented:
//!
//! - the **system prompt** definition: the space-join of every `describe`
//!   in the body, including inside `when` and `together`, in source order
//!   ([`codegen::system_prompt`]);
//! - **`lintNoBareKind`**: harness entities with no describe text anywhere
//!   ([`codegen::lint_no_bare_kind`], [`codegen::has_description`]);
//! - the **entry compiler** $cal(C)$: an entity declaration (name, kind,
//!   optional realm, body) to an entry record with id, plugin, config
//!   (name and system prompt), and the realm landing on `isolate`
//!   ([`codegen::compile_entity`]);
//! - the **kind-to-plugin map** $\kappa$ of @tab:kappa
//!   ([`codegen::compile_entity`]);
//! - the **fiber compiler**'s entry half and the coeffect projection:
//!   `requiredServices` collects every `require` node's services in source
//!   order and is emitted as a side map keyed by entity id, never inside
//!   an `Entry` ([`codegen::compile_harness`], [`codegen::required_services`]);
//! - the reactive **`watch` / `on change of` parser-level desugar** into
//!   ordinary `when` nodes ([`parser`]);
//! - [`reconcile`]: the `reconcile` diff plus [`reconcile::dry_run`], mirroring
//!   Eval.lean:521 with the composition order the paper proves.
//!
//! Not implemented (out of scope for this crate):
//!
//! - the **fiber compiler**'s service-environment half
//!   $\cal(F)(n,k,\rho,\overline{e},\sigma)$: applying $\sigma$ to each
//!   required service is the loader's job and lives in ARES, not here. This
//!   crate emits the service *names*.
//! - the **dispatch selector** $\delta$ (kind to Cordis dispatch mode) and
//!   the workflow compilations that turn `when` guards into conditional
//!   listeners, `together` blocks into `Dispatch::Parallel` fan-out and
//!   `instruct` sequences into chained dispatches. Those are Cordis
//!   registration paths; this crate stops at the AST.
//! - resource **resolution at load** (the linker), `gatherInstructs`, and
//!   the metatheory.
//!
//! ## Pipeline
//!
//! ```text
//! source ─▶ lexer ─▶ parser ─▶ codegen ─▶ Compiled { entries, requires }
//! ```
//!
//! [`codegen::compile_source`] runs the whole pipeline end to end.

pub mod ast;
pub mod codegen;
pub mod lexer;
pub mod parser;
pub mod reconcile;
