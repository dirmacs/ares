//! Regression tests for recursive `required_services` / `system_prompt`
//! collection through `when` bodies and `together` branches.

use ares_suriv::ast::{Entity, Expr, Harness};
use ares_suriv::codegen::{compile_harness, required_services, system_prompt};
use ares_suriv::lexer::Kind;

fn entity(name: &str, body: Vec<Expr>) -> Expr {
    Expr::Entity(Entity {
        name: name.into(),
        kind: Kind::Agent,
        realm: None,
        body,
    })
}

#[test]
fn require_inside_when_reaches_requires_map() {
    let h = Harness {
        name: "nested-require".into(),
        body: vec![entity(
            "worker",
            vec![Expr::When(
                "cond".into(),
                vec![Expr::Require(vec!["secret-service".into()])],
            )],
        )],
    };
    let c = compile_harness(&h);
    assert_eq!(
        c.requires.get("worker").cloned(),
        Some(vec!["secret-service".into()])
    );
    if let Expr::Entity(e) = &h.body[0] {
        assert_eq!(
            required_services(&e.body),
            vec!["secret-service".to_string()]
        );
    } else {
        panic!("expected entity");
    }
}

#[test]
fn require_inside_together_branch_reaches_requires_map() {
    let h = Harness {
        name: "nested-together".into(),
        body: vec![entity(
            "worker",
            vec![Expr::Together(vec![
                vec![Expr::Require(vec!["alpha".into()])],
                vec![Expr::Require(vec!["beta".into()])],
            ])],
        )],
    };
    let c = compile_harness(&h);
    assert_eq!(
        c.requires.get("worker").cloned(),
        Some(vec!["alpha".into(), "beta".into()])
    );
}

#[test]
fn nested_prose_interleaves_with_outer_describes_in_source_order() {
    // Must be "a b c", not "a c b" (would fail a top-level-only or two-pass join).
    let body = vec![
        Expr::Describe("a".into()),
        Expr::When("c".into(), vec![Expr::Describe("b".into())]),
        Expr::Describe("c".into()),
    ];
    assert_eq!(system_prompt(&body), "a b c");
}

#[test]
fn prose_inside_when_reaches_system_prompt_in_order() {
    let body = vec![
        Expr::Describe("Outer first.".into()),
        Expr::When(
            "cond".into(),
            vec![Expr::Describe("Inner when prose.".into())],
        ),
        Expr::Describe("Outer second.".into()),
    ];
    assert_eq!(
        system_prompt(&body),
        "Outer first. Inner when prose. Outer second."
    );
}

#[test]
fn require_interleaves_across_when_in_source_order() {
    let body = vec![
        Expr::Require(vec!["a".into()]),
        Expr::When("c".into(), vec![Expr::Require(vec!["b".into()])]),
        Expr::Require(vec!["c".into()]),
    ];
    assert_eq!(required_services(&body), vec!["a", "b", "c"]);
}

#[test]
fn duplicate_require_lines_preserve_order_and_duplicates() {
    let body = vec![
        Expr::Require(vec!["a".into()]),
        Expr::Require(vec!["a".into()]),
    ];
    assert_eq!(required_services(&body), vec!["a", "a"]);
}
