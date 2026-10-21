//! Golden test: the §9 worked example lexes, parses, and compiles to the
//! entries the spec states. Fixture is byte-verbatim from
//! `specs/suriv-spec-draft-v2.md` §9.

use ares_suriv::codegen::{Compiled, Config, Entry, compile_source};
use ares_suriv::lexer::{self, Kind, Resource, Token};

const FIXTURE: &str = include_str!("fixtures/customer-support.suriv");

fn golden() -> Compiled {
    compile_source(FIXTURE).expect("fixture compiles")
}

fn entry(c: &Compiled, id: &str) -> Entry {
    c.entries
        .iter()
        .find(|e| e.id == id)
        .expect("entry")
        .clone()
}

#[test]
fn fixture_lexes_to_the_closed_token_vocabulary() {
    let toks = lexer::lex(FIXTURE).expect("lex");
    // The 11 semantic token types (§1 table) must all appear; nothing
    // outside the closed vocabulary is ever produced.
    let mut seen = std::collections::BTreeSet::new();
    for t in &toks {
        let name = match t {
            Token::HarnessBegins => "harnessBegins",
            Token::HarnessEnds => "harnessEnds",
            Token::BringIn => "bringIn",
            Token::Kind(_) => "kind",
            Token::HandThisTo => "handThisTo",
            Token::Require => "require",
            Token::When => "when",
            Token::AtTheSameTime => "atTheSameTime",
            Token::Resource(_) => "resource",
            Token::CallThis => "callThis",
            Token::Recall => "recall",
            Token::Name(_) => "name",
            Token::Period => "period",
            Token::Newline => "newline",
        };
        seen.insert(name);
    }
    for kw in [
        "harnessBegins",
        "harnessEnds",
        "bringIn",
        "kind",
        "handThisTo",
        "require",
        "when",
        "atTheSameTime",
        "resource",
        "callThis",
        "recall",
    ] {
        assert!(seen.contains(kw), "token {kw} absent from fixture");
    }
    // Structural ceiling check. The phrase table holds one row per
    // closed phrase SPELLING: the 5 kind spellings share the single
    // semantic token `Token::Kind`, and the `resource` token is matched
    // by `match_resource` (never a table row). So the semantic-token
    // ceiling is:
    //   (rows - kind rows)          non-kind phrase tokens  = 9
    // + 1                           `Token::Kind`          = 10
    // + 1                           `Token::Resource`       = 11
    // which mirrors the Lean `semanticTokenCount := 11`
    // (Syntax.lean:116) and the const below. The reactive spellings
    // (`watch` / `on change of`) are desugar-only and add NO row
    // (reactive-spec header, ceiling rule): they lex as ordinary `Name`
    // words, so they can never appear here.
    let rows = lexer::phrase_table_len();
    let kind_rows = lexer::phrase_table_kind_rows();
    assert_eq!(rows - kind_rows + 1 + 1, lexer::SEMANTIC_TOKEN_COUNT);
    assert_eq!(lexer::SEMANTIC_TOKEN_COUNT, 11);
}

#[test]
fn prose_lines_become_describe_nodes_inside_bodies() {
    // §3.6: a non-keyword line inside an entity body is a describe node.
    // §7: top-level prose compiles to nothing. The fixture's prose lines
    // therefore appear as describe text in the entries, and the harness
    // body carries no describe nodes of its own.
    let c = golden();
    let total_describes = c
        .entries
        .iter()
        .map(|e| describe_count(&e.config.system_prompt))
        .sum::<usize>();
    assert_eq!(total_describes, 16, "one describe node per prose line");
}

fn describe_count(prompt: &str) -> usize {
    prompt
        .split_whitespace()
        .filter(|w| w.ends_with('.'))
        .count()
}

#[test]
fn fixture_kinds_and_resources_match_spec_spellings() {
    let toks = lexer::lex(FIXTURE).expect("lex");
    let kinds: Vec<Kind> = toks
        .iter()
        .filter_map(|t| match t {
            Token::Kind(k) => Some(*k),
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            Kind::Agent,
            Kind::Agent,
            Kind::Knowledge,
            Kind::Connector,
            Kind::Agent,
            Kind::Human,
        ]
    );
    let resources: Vec<Resource> = toks
        .iter()
        .filter_map(|t| match t {
            Token::Resource(r) => Some(r.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        resources,
        vec![
            Resource::File("prompts/router.md".into()),
            Resource::Inline("Refunds take 5-7 business days.".into()),
            Resource::Dir("docs/products/".into()),
            Resource::Env("DATABASE_URL".into()),
        ]
    );
}

#[test]
fn golden_entries() {
    let c = golden();
    assert_eq!(c.entries.len(), 5, "§9 declares five entities");

    let router = entry(&c, "router");
    assert_eq!(router.plugin, "AgentResolver");
    assert_eq!(router.isolate.as_deref(), Some("support"));
    assert!(!router.disabled);
    assert_eq!(
        router.config,
        Config {
            name: "router".into(),
            system_prompt: concat!(
                "Router reads customer messages and decides which specialist handles them. ",
                "It considers topic, urgency, and history. If unsure, it asks rather than guesses. ",
                "It uses claude-3.5-sonnet. It can classify and escalate. ",
                "It understands its role through @file(\"prompts/router.md\")."
            )
            .into()
        }
    );

    let billing = entry(&c, "billing-specialist");
    assert_eq!(billing.plugin, "AgentResolver");
    assert_eq!(billing.isolate, None);
    assert_eq!(
        billing.config.system_prompt,
        concat!(
            "The billing specialist handles refunds, account lookups, and payment disputes. ",
            "It uses gpt-4o. It can look-up-account and process-refund. ",
            "It answers timing questions from @inline(\"Refunds take 5-7 business days.\")."
        )
    );

    let docs = entry(&c, "product-docs");
    assert_eq!(docs.plugin, "RagPipeline");
    assert_eq!(docs.isolate, None);
    assert_eq!(
        docs.config.system_prompt,
        "Our product documentation lives in @dir(\"docs/products/\")."
    );

    let db = entry(&c, "customer-db");
    assert_eq!(db.plugin, "Store");
    assert_eq!(db.isolate.as_deref(), Some("billing"));
    assert_eq!(
        db.config.system_prompt,
        "The customer database speaks postgres at @env(\"DATABASE_URL\"). It can look-up-account."
    );

    let tech = entry(&c, "tech-agent");
    assert_eq!(tech.plugin, "AgentResolver");
    assert_eq!(tech.isolate, None);
    assert_eq!(
        tech.config.system_prompt,
        concat!(
            "The technical agent resolves faults using documentation and account history. ",
            "It uses gpt-4o. It can diagnose and respond."
        )
    );
}

#[test]
fn require_is_a_coeffect_not_an_entry_field() {
    let c = golden();
    // §3.9 / compileEntity: require never lands in Entry.
    assert_eq!(
        c.requires.get("router").cloned(),
        Some(vec![
            "billing-specialist".to_string(),
            "product-docs".to_string()
        ])
    );
    assert_eq!(c.requires.len(), 1, "only router has a coeffect");
    // router's require contributed no prose and no resource.
    let router = entry(&c, "router");
    assert!(
        !router
            .config
            .system_prompt
            .contains("billing-specialist, product-docs")
    );
    assert_eq!(router.config.name, "router");
}

#[test]
fn end_to_end_reports_the_five_entry_plugins() {
    let c = compile_source(FIXTURE).expect("compiles");
    let plugins: Vec<&str> = c.entries.iter().map(|e| e.plugin.as_str()).collect();
    assert_eq!(
        plugins,
        vec![
            "AgentResolver",
            "AgentResolver",
            "RagPipeline",
            "Store",
            "AgentResolver"
        ]
    );
}
