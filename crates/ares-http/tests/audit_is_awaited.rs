//! Structural regression test for item 1.16 (VERIFY-2026-09-22.md §4 row 20): every
//! `log_admin_action` call site in the admin/v1 handlers must be awaited
//! directly, never fired into a detached `tokio::spawn` and never have its
//! `Result` discarded with `let _ = `.
//!
//! Red on `one-dirmacs-issuer` @ `1fa9d9c86400d715a19e36893a14cffeff9c20e9`
//! (56 offending call sites across 12 files); green once every call site
//! goes through `ares_store::audit_log::record`, which awaits the insert
//! itself and only ever returns `()`.
//!
//! Pure text/compile-time scan (`include_str!`), no I/O, no live database.

/// One `(path, source)` pair per handler file the brief names as a caller of
/// `log_admin_action`, plus `providers.rs` (currently zero calls; the fix
/// adds create/delete audit calls there, which must land awaited too).
fn handler_sources() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "admin/agents.rs",
            include_str!("../src/api/handlers/admin/agents.rs"),
        ),
        (
            "admin/audit.rs",
            include_str!("../src/api/handlers/admin/audit.rs"),
        ),
        (
            "admin/connectors.rs",
            include_str!("../src/api/handlers/admin/connectors.rs"),
        ),
        (
            "admin/tenants.rs",
            include_str!("../src/api/handlers/admin/tenants.rs"),
        ),
        (
            "admin/triggers.rs",
            include_str!("../src/api/handlers/admin/triggers.rs"),
        ),
        (
            "admin/tools.rs",
            include_str!("../src/api/handlers/admin/tools.rs"),
        ),
        (
            "admin/schedules.rs",
            include_str!("../src/api/handlers/admin/schedules.rs"),
        ),
        (
            "admin/pipelines.rs",
            include_str!("../src/api/handlers/admin/pipelines.rs"),
        ),
        (
            "admin/health.rs",
            include_str!("../src/api/handlers/admin/health.rs"),
        ),
        (
            "admin/fleet_provider_keys.rs",
            include_str!("../src/api/handlers/admin/fleet_provider_keys.rs"),
        ),
        (
            "admin/shared.rs",
            include_str!("../src/api/handlers/admin/shared.rs"),
        ),
        (
            "admin/providers.rs",
            include_str!("../src/api/handlers/admin/providers.rs"),
        ),
        (
            "v1/agents.rs",
            include_str!("../src/api/handlers/v1/agents.rs"),
        ),
    ]
}

/// A defect: a `log_admin_action(` call whose line binds the result to `_`
/// (`let _ = ... log_admin_action(`), OR that sits inside an unclosed
/// `tokio::spawn(` block opened within the preceding `SPAWN_LOOKBACK` lines.
#[derive(Debug)]
struct Defect {
    file: &'static str,
    line_no: usize,
    line: String,
    reason: &'static str,
}

const SPAWN_LOOKBACK: usize = 15;

fn find_defects(file: &'static str, source: &str) -> Vec<Defect> {
    let lines: Vec<&str> = source.lines().collect();
    let mut defects = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        if !line.contains("log_admin_action(") {
            continue;
        }
        let line_no = idx + 1;

        // Discarded-result check: this exact line binds the call to `_`.
        // (The call always opens on its own line in this codebase; a
        // multi-line-safe check would need a real parser, but every existing
        // and every newly-added site keeps this shape.)
        if line.contains("let _ =") {
            defects.push(Defect {
                file,
                line_no,
                line: line.to_string(),
                reason: "log_admin_action's Result is discarded with `let _ =`",
            });
            continue;
        }

        // Spawn check: walk backward up to SPAWN_LOOKBACK lines looking for
        // an unclosed `tokio::spawn(`. "Unclosed" = no `});` line seen yet
        // between the spawn line and the call line.
        let start = idx.saturating_sub(SPAWN_LOOKBACK);
        let mut spawn_open_at: Option<usize> = None;
        for back in (start..idx).rev() {
            if lines[back].contains("});") {
                break; // a spawn (or other block) closed before we found one opening
            }
            if lines[back].contains("tokio::spawn(") {
                spawn_open_at = Some(back);
                break;
            }
        }
        if let Some(spawn_line) = spawn_open_at {
            defects.push(Defect {
                file,
                line_no,
                line: line.to_string(),
                reason: "log_admin_action sits inside a tokio::spawn opened nearby (see spawn line noted below)",
            });
            eprintln!(
                "  ...spawn opened at {file}:{} : {}",
                spawn_line + 1,
                lines[spawn_line].trim()
            );
        }
    }

    defects
}

#[test]
fn audit_is_awaited_on_every_admin_write() {
    let mut all_defects = Vec::new();
    for (file, source) in handler_sources() {
        all_defects.extend(find_defects(file, source));
    }

    if !all_defects.is_empty() {
        eprintln!(
            "audit_is_awaited_on_every_admin_write: {} defect(s):",
            all_defects.len()
        );
        for d in &all_defects {
            eprintln!(
                "  {}:{}: {} :: {}",
                d.file,
                d.line_no,
                d.reason,
                d.line.trim()
            );
        }
    }

    assert!(
        all_defects.is_empty(),
        "{} log_admin_action call site(s) are spawned-and-discarded instead of \
         awaited directly (see stderr for the list). Every call must go through \
         ares_store::audit_log::record(pool, ...).await, never tokio::spawn + let _ =.",
        all_defects.len()
    );
}

/// Documents the base-commit defect count so a future change to this test
/// (or a revert of the fix) is visible as a number, not just pass/fail.
/// Not itself a red/green gate — informational only.
#[test]
fn audit_defect_inventory_by_file() {
    for (file, source) in handler_sources() {
        let defects = find_defects(file, source);
        eprintln!("{file}: {} defect(s)", defects.len());
    }
}
