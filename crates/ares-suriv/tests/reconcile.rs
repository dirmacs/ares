//! Golden tests for [`ares_suriv::reconcile`], mirroring the Lean `#eval`s.
//!
//! Each case pairs a current/desired source with the exact action list the
//! Lean development computes (Reconcile.lean §5 + the §6 worked example +
//! the idempotency/retire/begin `#eval`s at Eval.lean:537-551).

use ares_suriv::codegen::compile_source;
use ares_suriv::reconcile::{LoaderAction, dry_run, reconcile};

fn entries(src: &str) -> Vec<ares_suriv::codegen::Entry> {
    compile_source(src).expect("fixture compiles").entries
}

fn action_ids(actions: &[LoaderAction]) -> Vec<&str> {
    actions.iter().map(LoaderAction::action_id).collect()
}

const HARNESS: &str = r#"
this harness begins as demo

bring in router, an agent, in support.
Summarize each thread into one paragraph.

bring in corpus, a knowledge source, in research.
Our product documentation lives here.

this harness ends
"#;

const HARNESS_PROMPT_CHANGED_PLUS_REVIEWER: &str = r#"
this harness begins as demo

bring in router, an agent, in support.
Summarize each thread into two paragraphs.

bring in corpus, a knowledge source, in research.
Our product documentation lives here.

bring in reviewer, an agent, in support.
Review every summary for accuracy.

this harness ends
"#;

#[test]
fn self_stable_empty_diff() {
    // reconcile H H = [] (reconcile_self_stable; distinct ids hold).
    let cur = entries(HARNESS);
    let actions = reconcile(&cur, &cur);
    assert!(actions.is_empty(), "expected [], got {actions:?}");
}

#[test]
fn retire_all_on_empty_desired() {
    // reconcile H [] = one retire per entry, in order.
    let cur = entries(HARNESS);
    let actions = reconcile(&cur, &[]);
    assert_eq!(action_ids(&actions), vec!["router", "corpus"]);
    assert!(actions.iter().all(|a| matches!(a, LoaderAction::Retire(_))));
}

#[test]
fn begin_all_on_empty_current() {
    // reconcile [] H = one begin per id, in order.
    let des = entries(HARNESS);
    let actions = reconcile(&[], &des);
    assert_eq!(action_ids(&actions), vec!["router", "corpus"]);
    assert!(
        actions
            .iter()
            .all(|a| matches!(a, LoaderAction::Begin { .. }))
    );
}

#[test]
fn worked_example_begin_before_update() {
    // Paper §6 worked example shape: changed prompt + new entity yields
    // begin before update (concatenation order).
    let cur = entries(HARNESS);
    let des = entries(HARNESS_PROMPT_CHANGED_PLUS_REVIEWER);
    let actions = reconcile(&cur, &des);
    assert_eq!(action_ids(&actions), vec!["reviewer", "router"]);
    assert!(matches!(
        actions[0],
        LoaderAction::Begin { ref plugin, .. } if plugin == "AgentResolver"
    ));
    assert!(matches!(actions[1], LoaderAction::UpdateConfig { .. }));
}

#[test]
fn every_action_warranted() {
    // Theorem 10 over the worked pair: the retire names an id the desired
    // list lacks, the begin names one the current list lacks, the update
    // carries the desired entry's own verdict.
    let cur = entries(HARNESS);
    let des = entries(HARNESS_PROMPT_CHANGED_PLUS_REVIEWER);
    let actions = reconcile(&cur, &des);
    let cur_ids: Vec<&str> = cur.iter().map(|e| e.id.as_str()).collect();
    let des_ids: Vec<&str> = des.iter().map(|e| e.id.as_str()).collect();
    for a in &actions {
        match a {
            LoaderAction::Retire(id) => {
                assert!(cur_ids.contains(&id.as_str()));
                assert!(!des_ids.contains(&id.as_str()));
            }
            LoaderAction::Begin { id, .. } => {
                assert!(des_ids.contains(&id.as_str()));
                assert!(!cur_ids.contains(&id.as_str()));
            }
            LoaderAction::UpdateConfig { id, config } => {
                let d = des.iter().find(|e| &e.id == id).expect("desired entry");
                assert_eq!(config, &d.config);
            }
            LoaderAction::RebuildFiber { id, plugin } => {
                let d = des.iter().find(|e| &e.id == id).expect("desired entry");
                assert_eq!(plugin, &d.plugin);
            }
        }
    }
}

#[test]
fn dry_run_renders_actions_without_applying() {
    let cur = entries(HARNESS);
    let out = dry_run(&cur, HARNESS_PROMPT_CHANGED_PLUS_REVIEWER).expect("dry run");
    assert_eq!(out, "begin reviewer AgentResolver\nupdate router\n");
    // Empty diff renders empty.
    let out_empty = dry_run(&cur, HARNESS).expect("dry run");
    assert_eq!(out_empty, "");
}

#[test]
fn dry_run_errors_name_compile_failure() {
    let cur = entries(HARNESS);
    let err = dry_run(&cur, "this is not a harness").expect_err("must fail");
    assert!(!err.is_empty());
}
