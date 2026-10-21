//! Codegen: AST -> `Entry` records (Primitives.lean:339-373) plus the
//! separate require/coeffect projection (`requiredServices`,
//! Eval.lean:116-119).
//!
//! Per the compileEntity ruling, `require` NEVER lands in an `Entry`: the
//! `Entry` shape is id / plugin / config / disabled / isolate only, and a
//! require line contributes no describe text and no resource. The
//! services are emitted as a side map keyed by entity id, mirroring
//! `compileFiber`'s `CompiledFiber.coeffect` (Eval.lean:105, 121-136).

use crate::ast::{Entity, Expr, Harness};
use crate::lexer::Kind;
use std::collections::BTreeMap;

/// A Cordis `Entry` — the atomic unit of a compiled program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub plugin: String,
    pub config: Config,
    pub disabled: bool,
    pub isolate: Option<String>,
}

/// `Val.record [("name", .text ..), ("system_prompt", .text ..)]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub name: String,
    pub system_prompt: String,
}

/// The compile output: entries plus the coeffect map.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Compiled {
    pub entries: Vec<Entry>,
    /// entity id -> required services, in source order. Mirrors
    /// `requiredServices`; absent key means the entity has no coeffect.
    pub requires: BTreeMap<String, Vec<String>>,
}

/// `pluginFromKind` (Eval.lean:182).
fn plugin_from_kind(kind: Kind) -> &'static str {
    match kind {
        Kind::Agent => "AgentResolver",
        Kind::Tool => "Tool",
        Kind::Knowledge => "RagPipeline",
        Kind::Connector => "Store",
        Kind::Human => "EscalationQueue",
    }
}

/// `compileEntity (name) (kind) (realm : Option Realm) (body)`
/// (Primitives.lean:357-373). The prompt is the `describe` join; the
/// realm phrase lands on `Entry.isolate`.
pub fn compile_entity(e: &Entity) -> Entry {
    let system_prompt = system_prompt(&e.body);
    Entry {
        id: e.name.clone(),
        plugin: plugin_from_kind(e.kind).to_string(),
        config: Config {
            name: e.name.clone(),
            system_prompt,
        },
        disabled: false,
        isolate: e.realm.clone(),
    }
}

/// `systemPrompt` (Eval.lean:256-260): describe join with single spaces.
pub fn system_prompt(body: &[Expr]) -> String {
    let parts: Vec<&str> = body
        .iter()
        .filter_map(|x| match x {
            Expr::Describe(s) => Some(s.as_str()),
            _ => None,
        })
        .collect();
    parts.join(" ")
}

/// `requiredServices` (Eval.lean:116-119): every `require` node in the
/// body contributes its services, in source order. `require` contributes
/// no prose and no resource.
pub fn required_services(body: &[Expr]) -> Vec<String> {
    let mut out = Vec::new();
    for x in body {
        if let Expr::Require(services) = x {
            out.extend(services.iter().cloned());
        }
    }
    out
}

/// Compile a harness. Top-level non-entity nodes compile to nothing at
/// the `Entry` level (§7), and top-level `require` is not an entity
/// declaration, so it is dropped here too (Primitives.lean:381-385).
pub fn compile_harness(h: &Harness) -> Compiled {
    let mut entries = Vec::new();
    let mut requires: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for x in &h.body {
        if let Expr::Entity(e) = x {
            let services = required_services(&e.body);
            if !services.is_empty() {
                requires.insert(e.name.clone(), services);
            }
            entries.push(compile_entity(e));
        }
    }
    Compiled { entries, requires }
}

/// End to end: lex -> parse -> codegen.
pub fn compile_source(src: &str) -> Result<Compiled, String> {
    let tokens = crate::lexer::lex(src)?;
    let harness = crate::parser::parse(&tokens)?;
    Ok(compile_harness(&harness))
}
