//! Codegen: AST -> `Entry` records (`compileEntity`, Primitives.lean) plus
//! the separate require/coeffect projection (`requiredServices`,
//! Primitives.lean).
//!
//! Per the compileEntity ruling, `require` NEVER lands in an `Entry`: the
//! `Entry` shape is id / plugin / config / disabled / isolate only, and a
//! require line contributes no describe text and no resource. The
//! services are emitted as a side map keyed by entity id, mirroring
//! `compileFiber`'s `CompiledFiber.coeffect` (`Coeffect`, `compileFiber`,
//! Eval.lean).

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

/// `pluginFromKind`, Eval.lean.
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

/// `systemPrompt`: recursive describe join with single spaces (see
/// `requiredServices` for the shared collection shape).
pub fn system_prompt(body: &[Expr]) -> String {
    let mut parts = Vec::new();
    collect_prose(body, &mut parts);
    parts.join(" ")
}

/// Lean `hasDescription`: true when `describeParts` is non-empty. Uses the
/// same recursive collector as [`system_prompt`], not a separate walk.
pub fn has_description(body: &[Expr]) -> bool {
    let mut parts = Vec::new();
    collect_prose(body, &mut parts);
    !parts.is_empty()
}

/// Lean `lintNoBareKind`: harness-level entities whose body carries no
/// `.describe` anywhere (including inside `when` / `together`). Non-entity
/// top-level nodes are skipped; names are returned in source order.
pub fn lint_no_bare_kind(h: &Harness) -> Vec<String> {
    let mut out = Vec::new();
    for x in &h.body {
        if let Expr::Entity(e) = x
            && !has_description(&e.body)
        {
            out.push(e.name.clone());
        }
    }
    out
}

fn collect_prose<'a>(body: &'a [Expr], parts: &mut Vec<&'a str>) {
    // Single pass in source order: each sibling is handled before the next,
    // and `when`/`together` recurse inline (never a separate top-level pass).
    for x in body {
        match x {
            Expr::Describe(s) => parts.push(s.as_str()),
            Expr::When(_, inner) => collect_prose(inner, parts),
            Expr::Together(branches) => {
                for branch in branches {
                    collect_prose(branch, parts);
                }
            }
            Expr::Entity(_)
            | Expr::Instruct(_)
            | Expr::Resource(_)
            | Expr::Bind(_)
            | Expr::Recall(_)
            | Expr::Require(_) => {}
        }
    }
}

/// `requiredServices`: every `require` in the body, including inside
/// `when` bodies and `together` branches, in source order. `require`
/// contributes no prose and no resource.
pub fn required_services(body: &[Expr]) -> Vec<String> {
    let mut out = Vec::new();
    collect_services(body, &mut out);
    out
}

fn collect_services(body: &[Expr], out: &mut Vec<String>) {
    for x in body {
        match x {
            Expr::Require(services) => out.extend(services.iter().cloned()),
            Expr::When(_, inner) => collect_services(inner, out),
            Expr::Together(branches) => {
                for branch in branches {
                    collect_services(branch, out);
                }
            }
            Expr::Entity(_)
            | Expr::Instruct(_)
            | Expr::Resource(_)
            | Expr::Bind(_)
            | Expr::Recall(_)
            | Expr::Describe(_) => {}
        }
    }
}

/// Compile a harness. Top-level non-entity nodes compile to nothing at
/// the `Entry` level (§7), and top-level `require` is not an entity
/// declaration, so it is dropped here too (`compileHarness`, Primitives.lean).
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
