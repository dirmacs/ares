# ares-suriv — the Suriv language's lexer, parser and code generator

Production port of the Suriv prototype: surface text to Cordis `Entry`
records plus the separate `requires` coeffect map. The port is byte-for-byte
behaviour-preserving — same AST, same `Entry` records, same `requires` map,
same desugars, same golden fixture bytes.

Ground truth: Lean (`Expr` 9 variants, `Entry` shape, `compileEntity` realm
param, `requiredServices` / `compileFiber` coeffect projection).

Layout: `lexer.rs` (closed phrase table → tokens, prose fallback) →
`parser.rs` (tokens → `ast.rs` mirroring `Expr`) → `codegen.rs` (AST →
`Entry` records + separate `requires` coeffect map). Golden test over
`tests/fixtures/customer-support.suriv`, byte-verbatim; reactive desugar
tests over `tests/fixtures/billing-flap.suriv`.

## Scope

Implements: the system-prompt definition, the entry compiler $cal(C)$, the
kind-to-plugin map $\kappa$, and the coeffect projection `requiredServices`
emitted as a side map keyed by entity id.

Does not implement: the fiber compiler's service-environment half
($\sigma$ applied to each required service — that is the loader's job, in
ARES), the dispatch selector $\delta$ (kind to dispatch mode), the workflow
compilations that register `when` guards as conditional listeners and
`together` blocks as `Dispatch::Parallel` fan-out, resource resolution at
load, the static lints, and the metatheory. The crate's `lib.rs` docstring
states the same split.

The language's specification, and the choices this implementation makes
where that specification is silent, are documented with the specification
rather than here.

## Adaptations from the prototype to this repository

- **Crate identity and metadata.** The prototype's package name
  `suriv-proto` and its edition 2021 become `ares-suriv` with the
  workspace's edition 2024, version, license, repository and authors
  inherited from `[workspace.package]`. No source semantics changed.
- **Doc comments.** Comments that referenced the prototype's own name
  (`suriv-proto`) now name this crate; all spec and Lean citations are
  unchanged. `Token` doc comments and the prose descriptions were not
  touched beyond that.
- **Test imports.** `use suriv_proto::...` becomes `use ares_suriv::...`.
  Assertions, fixture bytes and expectations are unchanged.
- **Edition 2024 `impl<'a> Parser<'a>`** still compiles as written; the
  explicit lifetime parameter is kept because `Parser` holds a borrowed
  slice, not because the syntax requires it.

## Tests

- `tests/golden.rs` — 6 tests over `fixtures/customer-support.suriv`: the
  closed token vocabulary, prose-to-describe, kind/resource spellings, the
  golden entries, `require` as a coeffect rather than an entry field, and
  the five entry plugins end to end.
- `tests/reactive.rs` — 13 tests over the `watch` / `on change of` desugar,
  including `fixtures/billing-flap.suriv`.

`cargo test -p ares-suriv` runs both files.
