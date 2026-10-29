//! Lean `lintNoBareKind` parity (`Properties.lean`).

use ares_suriv::ast::{Entity, Expr, Harness};
use ares_suriv::codegen::lint_no_bare_kind;
use ares_suriv::lexer::{Kind, Resource};

const CUSTOMER_SUPPORT: &str = include_str!("fixtures/customer-support.suriv");
const BILLING_FLAP: &str = include_str!("fixtures/billing-flap.suriv");

fn parse_harness(src: &str) -> Harness {
    let tokens = ares_suriv::lexer::lex(src).expect("lex");
    ares_suriv::parser::parse(&tokens).expect("parse")
}

fn entity(name: &str, body: Vec<Expr>) -> Expr {
    Expr::Entity(Entity {
        name: name.into(),
        kind: Kind::Agent,
        realm: None,
        body,
    })
}

#[test]
fn customer_support_fixture_matches_lean() {
    let h = parse_harness(CUSTOMER_SUPPORT);
    assert!(lint_no_bare_kind(&h).is_empty());
}

#[test]
fn billing_flap_fixture_reports_nothing() {
    let h = parse_harness(BILLING_FLAP);
    assert!(lint_no_bare_kind(&h).is_empty());
}

#[test]
fn prose_only_inside_when_is_not_bare() {
    let h = Harness {
        name: "nested-prose".into(),
        body: vec![entity(
            "worker",
            vec![Expr::When(
                "cond".into(),
                vec![Expr::Describe("Only nested prose.".into())],
            )],
        )],
    };
    assert!(lint_no_bare_kind(&h).is_empty());
}

#[test]
fn top_level_prose_is_not_bare() {
    let h = Harness {
        name: "top-prose".into(),
        body: vec![entity(
            "worker",
            vec![Expr::Describe("Configured.".into())],
        )],
    };
    assert!(lint_no_bare_kind(&h).is_empty());
}

#[test]
fn resources_without_prose_is_bare() {
    let h = Harness {
        name: "resource-only".into(),
        body: vec![entity(
            "docs",
            vec![Expr::Resource(Resource::Dir("docs/".into()))],
        )],
    };
    assert_eq!(lint_no_bare_kind(&h), vec!["docs".to_string()]);
}
