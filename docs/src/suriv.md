# Suriv

Suriv is the surface language for agent harnesses. You write the entities that take part in a workflow and how control passes between them; the compiler turns that text into the same Cordis `Entry` records the rest of ARES loads, plus a separate map of the services each entity requires.

This page is the onboarding guide: the shape of the language, how to compile a harness, and what the compiler deliberately leaves to the loader.

## When to read this

- You are writing or changing a `.suriv` harness.
- You are adding a construct to the language and need to know which file owns it.
- You want to know why a `require` line does not appear in the compiled output.
- A harness compiles but does not behave the way you expected at run time.

## Where it lives

| File | Owns |
|---|---|
| `crates/ares-suriv/src/lexer.rs` | the closed phrase table: surface text to tokens |
| `crates/ares-suriv/src/parser.rs` | tokens to the AST, including the reactive desugars |
| `crates/ares-suriv/src/ast.rs` | the AST, mirroring the language's `Expr` |
| `crates/ares-suriv/src/codegen.rs` | the AST to `Entry` records and the `requires` map |

`ares-suriv` is the only crate in the workspace that knows about Suriv, and nothing else in the workspace references it. The server does not read `.suriv` files: the compiler is a library, and a harness reaches the server only after it has been compiled and loaded.

The language's specification, and the choices this implementation makes where that specification is silent, live with the specification rather than in this repository.

## The shape of a harness

```suriv
this harness begins as customer-support

bring in router, an agent, in support.
Router reads customer messages and decides which specialist handles them.
It considers topic, urgency, and history.

require billing-specialist, product-docs.

when a customer inquiry arrives
  hand this to router.

  when the topic is billing
    hand this to billing-specialist.

this harness ends
```

| Form | Meaning |
|---|---|
| `this harness begins as <name>` / `this harness ends` | the harness boundary |
| `bring in <name>, <kind>[, in <realm>]` | an entity declaration |
| `require <service>...` | the services the entity needs: a coeffect, not an entry field |
| `when <condition>` | a conditional |
| `at the same time` | a parallel block |
| `hand this to <target>` | a hand-off |
| `call this <name>` / `recall <name>` | a binding, and its later use |
| `@file("...")`, `@inline("...")`, `@dir("...")`, `@env("...")` | resources, resolved at load rather than at compile time |

There is one spelling per kind, and each kind compiles to one plugin:

| Kind | Plugin |
|---|---|
| `an agent` | `AgentResolver` |
| `a tool` | `Tool` |
| `a knowledge source` | `RagPipeline` |
| `a connector` | `Store` |
| `a human` | `EscalationQueue` |

`a knowledge source` has no alias. The kind spellings are matched as whole phrases with a separator after them, so `a knowledge` on its own is not a kind.

## Compiling a harness

There is no `ares compile` command. The compiler is a library call:

```rust
use ares_suriv::codegen::compile_source;

let compiled = compile_source(source_text)?;   // Compiled { entries, requires }
```

`compile_source` runs the whole pipeline, lex to parse to codegen, and returns the entries in source order plus the `requires` map keyed by entity id. The suite that covers it:

```bash
cargo test -p ares-suriv
```

## What you get back

Compiling `crates/ares-suriv/tests/fixtures/customer-support.suriv` produces five entries and one coeffect:

```text
ENTRY id=router plugin=AgentResolver isolate=Some("support")
   system_prompt="Router reads customer messages and decides which specialist handles them. It considers topic, urgency, and history. If unsure, it asks rather than guesses. It uses claude-3.5-sonnet. It can classify and escalate. It understands its role through @file(\"prompts/router.md\")."
ENTRY id=billing-specialist plugin=AgentResolver isolate=None
   system_prompt="The billing specialist handles refunds, account lookups, and payment disputes. It uses gpt-4o. It can look-up-account and process-refund. It answers timing questions from @inline(\"Refunds take 5-7 business days.\")."
ENTRY id=product-docs plugin=RagPipeline isolate=None
   system_prompt="Our product documentation lives in @dir(\"docs/products/\")."
ENTRY id=customer-db plugin=Store isolate=Some("billing")
   system_prompt="The customer database speaks postgres at @env(\"DATABASE_URL\"). It can look-up-account."
ENTRY id=tech-agent plugin=AgentResolver isolate=None
   system_prompt="The technical agent resolves faults using documentation and account history. It uses gpt-4o. It can diagnose and respond."
REQUIRES router -> ["billing-specialist", "product-docs"]
```

An `Entry` is `id`, `plugin`, `config` (the name and the system prompt), `disabled`, and `isolate`. The realm you declare lands on `isolate`.

## What the compiler does not do

The compiler stops at the AST and the entry records. The rest is the loader's and the kernel's work:

- applying the service environment to each required service, and resolving resources at load;
- the dispatch selector, which maps a kind to its dispatch mode;
- the workflow compilations that turn `when` guards into conditional listeners, `at the same time` blocks into parallel fan-out, and `instruct` sequences into chained dispatches;
- the static lints and the metatheory.

A harness that compiles is therefore not a harness that runs. The pieces above are what turn entries into behaviour.

## Pitfalls

- **`require` is a coeffect.** It never lands in an `Entry`: it contributes no prose and no resource, and it appears only in the `requires` map, keyed by entity id.
- **Resources stay literal.** `@file("prompts/router.md")` is text in the compiled system prompt. The loader resolves it, so a missing file is a load-time failure rather than a compile-time one.
- **The phrase table is closed, and its size is asserted.** The lexer holds one row per phrase spelling, and the golden suite checks that the semantic token count stays at 11. Adding a row breaks that test. The reactive spellings `watch` and `on change of` are deliberately not rows: they lex as ordinary words and the parser matches them at statement start.
- **A keyword needs a trailing separator.** The matcher is longest-match over the table with a required separator after the phrase, so `bring inx` is not a keyword.
- **Prose compiles only inside an entity body.** A non-keyword line inside an entity body is a describe node, and the describe nodes are joined with single spaces into the system prompt. Prose at the top level of a harness compiles to nothing.
- **A top-level `require` is dropped.** It is not an entity declaration, so the harness compiler discards it.
- **A `watch` block desugars.** `watch X` with `when X is present` and `when X is absent` becomes ordinary `when` nodes, so a reactive harness compiles to the same kind of entries as a plain one.

## See also

- [Command Line Interface](cli.md): the server's commands, and why there is no Suriv command among them.
- `crates/ares-suriv/README.md`: the crate's own scope note, and the tests that cover it.
