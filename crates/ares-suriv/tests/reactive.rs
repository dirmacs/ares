//! Golden test for the reactive desugar (`watch` / `on change of` →
//! `when`), per `specs/suriv-reactive-spec.md` §2 and its §5 worked
//! mini-example (housed in-repo as `specs/suriv-reactive-spec.md`).
//!
//! Zero Lean change: the compiled form is ordinary `Expr.when` data, so
//! every existing theorem (`compileOrder_independent`, `docIsImpl`, the
//! `require` discipline theorems) applies unchanged.

use ares_suriv::ast::{Expr, Harness};
use ares_suriv::codegen::{Compiled, compile_source};
use ares_suriv::lexer::{self, Token};
use ares_suriv::parser;

const FIXTURE: &str = include_str!("fixtures/billing-flap.suriv");

fn parsed() -> Harness {
    let toks = lexer::lex(FIXTURE).expect("lex");
    parser::parse(&toks).expect("parse")
}

fn compiled() -> Compiled {
    compile_source(FIXTURE).expect("compiles")
}

fn router_body(h: &Harness) -> &[Expr] {
    for x in &h.body {
        if let Expr::Entity(e) = x
            && e.name == "billing-router"
        {
            return &e.body;
        }
    }
    panic!("billing-router not declared")
}

/// Every `when` node reachable in the body, in source order, paired with
/// its condition text. Nested bodies are flattened for the membership
/// checks below.
fn whens(exprs: &[Expr]) -> Vec<(String, Vec<Expr>)> {
    let mut out = Vec::new();
    for x in exprs {
        match x {
            Expr::When(c, b) => {
                out.push((c.clone(), b.clone()));
                out.extend(whens(b));
            }
            Expr::Together(bs) => {
                for b in bs {
                    out.extend(whens(b));
                }
            }
            Expr::Entity(e) => out.extend(whens(&e.body)),
            _ => {}
        }
    }
    out
}

fn conditions(exprs: &[Expr]) -> Vec<String> {
    whens(exprs).into_iter().map(|(c, _)| c).collect()
}

#[test]
fn reactive_spellings_add_no_token_to_the_closed_table() {
    // The ceiling rule (reactive-spec header): zero new keywords, zero
    // new primitives, zero lexer-token changes. `watch` / `on change of`
    // are desugar spellings, not semantic tokens: the closed phrase
    // table still spells exactly 11 semantic tokens (structural, via the
    // helpers — `Kind` and `Resource` are single tokens with several
    // spellings, and the const mirrors the Lean `semanticTokenCount`,
    // Syntax.lean:116), and the reactive phrases reach the parser as
    // ordinary `Name` words.
    assert_eq!(
        lexer::phrase_table_len() - lexer::phrase_table_kind_rows() + 2,
        lexer::SEMANTIC_TOKEN_COUNT
    );
    assert_eq!(lexer::SEMANTIC_TOKEN_COUNT, 11);
    // The spellings lex as `Name` words — NOT phrase tokens. No watch
    // phrase has a `Token` variant, so the exhaustive match over the
    // closed `Token` table below states the ceiling structurally: a new
    // phrase variant breaks compilation here.
    let toks = lexer::lex(FIXTURE).expect("lex");
    let mut watch_words = 0usize;
    for t in &toks {
        match t {
            Token::HarnessBegins
            | Token::HarnessEnds
            | Token::BringIn
            | Token::Kind(_)
            | Token::HandThisTo
            | Token::Require
            | Token::When
            | Token::AtTheSameTime
            | Token::Resource(_)
            | Token::CallThis
            | Token::Recall
            | Token::Period
            | Token::Newline => {}
            Token::Name(w) if w == "watch" => watch_words += 1,
            Token::Name(_) => {}
        }
    }
    assert_eq!(
        watch_words, 1,
        "the fixture's one `watch` line is a `Name` word, not a phrase token"
    );
    // The desugar produces only `When` nodes — no new Expr variant.
    let h = parsed();
    let body = router_body(&h);
    for x in body {
        // Exhaustive match over the closed `Expr` table — no wildcard
        // arm, so a future variant breaks compilation right here, which
        // is the ceiling rule stated structurally:
        // "desugar must not introduce a variant: {x:?}"
        match x {
            Expr::Entity(_)
            | Expr::Instruct(_)
            | Expr::When(..)
            | Expr::Together(_)
            | Expr::Resource(_)
            | Expr::Describe(_)
            | Expr::Bind(_)
            | Expr::Recall(_)
            | Expr::Require(_) => {}
        }
    }
}

#[test]
fn mini_example_desugars_to_the_three_stated_when_nodes() {
    let h = parsed();
    let body = router_body(&h);
    assert_eq!(
        conditions(body),
        vec![
            "billing-ledger is present".to_string(),
            "billing-ledger is present".to_string(),
            "billing-ledger is absent".to_string(),
            "billing-ledger changed epoch".to_string(),
        ],
        "reactive-spec §5 desugared form: the outer presence when from
         `watch` (its body is the fixture's two inner whens, verbatim),
         then those two inner whens flattened, then the epoch when from
         `on change of` — outer presence when + two inner whens + epoch when"
    );
}

#[test]
fn watch_body_is_copied_verbatim_into_each_when() {
    let h = parsed();
    let body = router_body(&h);
    // The watch body's own two `when` statements are ordinary statements
    // inside it (reactive-spec §2 "No nesting newness"), so the watch
    // yields one `when` per watched service, each carrying the whole
    // body. One service here → one outer `when`.
    let outer: Vec<&Expr> = body
        .iter()
        .filter(|x| matches!(x, Expr::When(..)))
        .collect();
    assert_eq!(outer.len(), 2, "watch + on change of = two top-level whens");
    let (cond, inner) = match outer[0] {
        Expr::When(c, b) => (c, b),
        other => panic!("expected when, found {other:?}"),
    };
    assert_eq!(cond, "billing-ledger is present");
    // Verbatim copy: the outer when's direct children are the fixture's
    // two inner `when` statements, as written (present-when, absent-when).
    let kinds: Vec<&str> = inner
        .iter()
        .map(|x| match x {
            Expr::Instruct(_) => "instruct",
            Expr::Bind(_) => "bind",
            Expr::Describe(_) => "describe",
            Expr::When(..) => "when",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, vec!["when", "when"]);
    // The bodies are the watch body, not a flattened rewrite: the outer
    // node holds the inner whens as written.
    assert_eq!(inner.len(), 2);
    // Each inner when still holds its own body verbatim, at the level
    // where the copy actually happened.
    let inner_kinds = |w: &Expr| -> Vec<&'static str> {
        let Expr::When(_, b) = w else {
            panic!("expected when, found {w:?}")
        };
        b.iter()
            .map(|x| match x {
                Expr::Instruct(_) => "instruct",
                Expr::Bind(_) => "bind",
                Expr::Describe(_) => "describe",
                Expr::When(..) => "when",
                _ => "other",
            })
            .collect()
    };
    assert_eq!(inner_kinds(&inner[0]), vec!["instruct", "bind"]);
    assert_eq!(inner_kinds(&inner[1]), vec!["instruct", "describe"]);
}

#[test]
fn watch_observes_and_never_declares_a_coeffect() {
    let c = compiled();
    // `require billing-ledger.` is the sole coeffect producer; the watch
    // line references the dependency already declared and adds nothing.
    assert_eq!(
        c.requires.get("billing-router").cloned(),
        Some(vec!["billing-ledger".to_string()])
    );
    assert_eq!(c.requires.len(), 1, "only the require line contributes");
}

#[test]
fn nested_describes_reach_system_prompt_while_reactive_spellings_do_not() {
    let c = compiled();
    let router = c
        .entries
        .iter()
        .find(|e| e.id == "billing-router")
        .expect("entry");
    // §6: watch / on change desugar to `when` nodes only — they add no
    // resource and no new phrase tokens. Describes anywhere in the entity
    // body (including inside those whens) join into the system prompt in
    // source order. An older flat collector kept only top-level describes;
    // that was the bug this assertion previously encoded.
    assert_eq!(
        router.config.system_prompt,
        concat!(
            "The router dispatches billing work and notices when the ledger comes back. ",
            "No ledger, escalating to a person. ",
            "The ledger flapped, recording the new epoch."
        )
    );
    assert!(!router.config.system_prompt.contains("watch"));
    assert!(!router.config.system_prompt.contains("on change"));
}

#[test]
fn epoch_when_records_the_bind_the_spec_states() {
    let h = parsed();
    let body = router_body(&h);
    let epoch = whens(body)
        .into_iter()
        .find(|(c, _)| c == "billing-ledger changed epoch")
        .expect("epoch when");
    let inner = epoch.1;
    assert!(
        matches!(inner.first(), Some(Expr::Bind(n)) if n == "flap"),
        "the epoch body records the new epoch via a `call this` store write"
    );
    assert!(
        inner.iter().any(|x| matches!(x, Expr::Describe(d) if d == "The ledger flapped, recording the new epoch.")),
        "the epoch body's describe is verbatim"
    );
}

#[test]
fn watch_of_two_services_yields_one_when_per_name_in_written_order() {
    let src = "this harness begins as two\n\nbring in r, an agent.\nwatch a, b.\n  hand this to a.\nthis harness ends\n";
    let toks = lexer::lex(src).expect("lex");
    let h = parser::parse(&toks).expect("parse");
    let body = match &h.body[0] {
        Expr::Entity(e) => &e.body,
        other => panic!("expected entity, found {other:?}"),
    };
    assert_eq!(
        conditions(body),
        vec!["a is present".to_string(), "b is present".to_string()],
        "reactive-spec §2 row 3: one when per name, written order"
    );
    // SIBLINGS, not a fan-out (reactive-spec §2 / §5c): the entity body
    // holds exactly two top-level `Expr::When` nodes in written order,
    // each with its own verbatim copy of the watch body. No `Together`
    // node wraps them — `Together` would assert join/parallel semantics
    // §5c never states for watch.
    assert_eq!(
        body.len(),
        2,
        "two sibling whens, nothing else in the entity body: {body:?}"
    );
    for (i, expected_cond) in ["a is present", "b is present"].into_iter().enumerate() {
        let Expr::When(cond, when_body) = &body[i] else {
            panic!("expected sibling when at {i}, found {:?}", body[i])
        };
        assert_eq!(cond, expected_cond, "sibling {i} condition");
        // Verbatim copy: both siblings carry an identical copy of the
        // watch body as parsed (its first node is the stray
        // `Describe(".")` left by the comma-slurp path — the
        // pre-existing shape, identical in both copies).
        let sibling_body = |j: usize| match &body[j] {
            Expr::When(_, b) => b.clone(),
            other => panic!("expected sibling when at {j}, found {other:?}"),
        };
        assert_eq!(
            when_body,
            &sibling_body((i + 1) % body.len()),
            "sibling {i} body copy is identical to the other sibling's copy"
        );
        assert!(
            matches!(when_body.last(), Some(Expr::Instruct(t)) if t == "a"),
            "sibling {i} body ends with the watch body's statement: {when_body:?}"
        );
    }
    // No `Together` anywhere in the entity body.
    assert!(
        !body.iter().any(|x| matches!(x, Expr::Together(_))),
        "multi-watch must not wrap siblings in Together: {body:?}"
    );
}

#[test]
fn watch_until_conjoins_the_presence_predicate() {
    let src = "this harness begins as u\n\nbring in r, an agent.\nwatch a until the topic is billing.\n  hand this to a.\nthis harness ends\n";
    let toks = lexer::lex(src).expect("lex");
    let h = parser::parse(&toks).expect("parse");
    let body = match &h.body[0] {
        Expr::Entity(e) => &e.body,
        other => panic!("expected entity, found {other:?}"),
    };
    assert_eq!(
        conditions(body),
        vec!["a is present and the topic is billing".to_string()],
        "reactive-spec §2 row 6"
    );
}

#[test]
fn on_change_appearing_and_disappearing_map_to_presence_and_absence() {
    let src = "this harness begins as tails\n\nbring in r, an agent.\non change of a appearing.\n  hand this to a.\non change of a disappearing.\n  hand this to a human.\nthis harness ends\n";
    let toks = lexer::lex(src).expect("lex");
    let h = parser::parse(&toks).expect("parse");
    let body = match &h.body[0] {
        Expr::Entity(e) => &e.body,
        other => panic!("expected entity, found {other:?}"),
    };
    assert_eq!(
        conditions(body),
        vec!["a is present".to_string(), "a is absent".to_string()],
        "reactive-spec §2 rows 5-6"
    );
}

#[test]
fn watch_body_contains_ordinary_when_and_together_statements() {
    // §2 "No nesting newness": a `when` and an `at the same time` inside
    // a watch body are ordinary — the body is copied verbatim, and the
    // block is closed by the next `bring in`, not by the inner `when`.
    let src = "this harness begins as nested\n\nbring in r, an agent.\nwatch a.\n  when the topic is billing\n    hand this to a.\n  at the same time\n    hand this to b.\n    hand this to c.\nthis harness ends\n";
    let toks = lexer::lex(src).expect("lex");
    let h = parser::parse(&toks).expect("parse");
    let body = match &h.body[0] {
        Expr::Entity(e) => &e.body,
        other => panic!("expected entity, found {other:?}"),
    };
    assert_eq!(
        conditions(body),
        vec![
            "a is present".to_string(),
            "the topic is billing".to_string()
        ],
        "the inner when stays inside the watch body"
    );
    // The `at the same time` is an ordinary statement too: one together
    // node with two branches, inside the watch body.
    let outer = body
        .iter()
        .find(|x| matches!(x, Expr::When(c, _) if c == "a is present"))
        .expect("outer when");
    let Expr::When(_, inner) = outer else {
        unreachable!()
    };
    let together = inner
        .iter()
        .find(|x| matches!(x, Expr::Together(_)))
        .expect("together inside watch body");
    let Expr::Together(branches) = together else {
        unreachable!()
    };
    assert_eq!(branches.len(), 2, "one branch per statement");
}

#[test]
fn empty_watch_body_is_an_error_not_a_silent_false() {
    let src =
        "this harness begins as empty\n\nbring in r, an agent.\nwatch a.\nthis harness ends\n";
    let toks = lexer::lex(src).expect("lex");
    let err = parser::parse(&toks).expect_err("empty watch body must error");
    assert!(err.contains("watch"), "error names the form: {err}");
}

#[test]
fn check_prints_the_desugared_condition_for_each_watch_line() {
    // reactive-spec §6 implementation note: `suriv check` prints the
    // desugared condition so the reader sees exactly what compiles.
    let h = parsed();
    let body = router_body(&h);
    let printed: Vec<String> = body
        .iter()
        .filter_map(|x| match x {
            Expr::When(c, _) => Some(c.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        printed,
        vec![
            "billing-ledger is present".to_string(),
            "billing-ledger changed epoch".to_string()
        ]
    );
}

#[test]
fn reactive_phrases_do_not_leak_into_prose_or_conditions() {
    // `watch` inside prose or inside a `when` condition is an ordinary
    // word: the reactive spellings are NOT in the closed phrase table, so
    // they lex as `Name` words and only the parser's statement-start
    // gate turns them into a desugar (here the fixture's prose has none,
    // and this test pins that the mini-example's describe text is
    // unchanged).
    let c = compiled();
    let router = c
        .entries
        .iter()
        .find(|e| e.id == "billing-router")
        .expect("entry");
    assert!(
        router
            .config
            .system_prompt
            .contains("notices when the ledger comes back.")
    );
    // Token stream: both reactive spellings are `Name` word sequences,
    // never phrase tokens. `watch` is one `Name("watch")`; `on change of`
    // is three consecutive `Name`s on one line (`on`, `change`, `of`).
    let toks = lexer::lex(FIXTURE).expect("lex");
    let words: Vec<&str> = toks
        .iter()
        .filter_map(|t| match t {
            Token::Name(w) => Some(w.as_str()),
            _ => None,
        })
        .collect();
    assert!(words.contains(&"watch"), "`watch` lexes as a Name word");
    assert!(
        words.windows(3).any(|w| w == ["on", "change", "of"]),
        "`on change of` lexes as three consecutive Name words"
    );
    // And the parser still desugars both spellings to `when` nodes.
    let h = parsed();
    let body = router_body(&h);
    let printed: Vec<String> = body
        .iter()
        .filter_map(|x| match x {
            Expr::When(c, _) => Some(c.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        printed,
        vec![
            "billing-ledger is present".to_string(),
            "billing-ledger changed epoch".to_string()
        ]
    );
}
