//! Suriv AST — a Rust mirror of `Expr` (Primitives.lean:157-201), the 9
//! primitives, plus the harness boundary (`Harness`, Primitives.lean:202-205)
//! and the realm compile parameter (§6).

use crate::lexer::{Kind, Resource};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// `bring in <name>, a <kind>[ in <realm>]` — the realm is a compile
    /// parameter, not an AST field (§6, Primitives.lean:357), so it lives
    /// on `Entity` alongside the body it scopes.
    Entity(Entity),
    /// `hand this to <name>.`
    Instruct(String),
    /// `when <condition>` + body.
    When(String, Vec<Expr>),
    /// `at the same time` + branch list.
    Together(Vec<Vec<Expr>>),
    /// `@file` / `@dir` / `@env` / `@inline`.
    Resource(Resource),
    /// Any non-keyword line inside a body.
    Describe(String),
    /// `call this <name>.`
    Bind(String),
    /// `recall <name>.`
    Recall(String),
    /// `require <service>.` / `require <a>, <b>.`
    Require(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity {
    pub name: String,
    pub kind: Kind,
    /// Optional explicit realm from `, in <realm>`; `None` inherits the
    /// harness realm (§6).
    pub realm: Option<String>,
    pub body: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Harness {
    pub name: String,
    pub body: Vec<Expr>,
}
