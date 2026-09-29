//! Structural regression tests for item 1.16 (VERIFY-2026-09-22.md §4 row 20).
//!
//! The goal: every admin write and every key-lifecycle write leaves exactly
//! one `admin_audit_log` row, with its actor, **before the response is
//! returned**. Two properties are enforced here by reading this crate's own
//! source text (no database, no network, no `include_str!` list to forget to
//! extend: the whole of `crates/ares-http/src` is walked at run time).
//!
//! 1. `audit_is_awaited_on_every_admin_write`: every audit call site
//!    (`audit_log::record(` and `log_admin_action(`) is `.await`ed exactly
//!    where it is called, and does not sit inside the argument of any spawn
//!    form (`tokio::spawn`, `tokio::task::spawn`, `spawn_blocking`,
//!    `spawn_local`, `JoinSet::spawn`, any `.spawn(`), judged by the balanced
//!    extent of the spawned argument, nor inside an `async` block that could
//!    be spawned elsewhere. The test also asserts that it scanned at least a
//!    stated number of sites (in total and per file), so it can never go
//!    vacuous again: at `6aaf2da` a scan keyed on the old function name
//!    matched 0 sites and a spawn-wrapped `record(...)` passed.
//! 2. `every_mutating_admin_route_is_audited_or_exempt`: every mutating
//!    (POST / PUT / PATCH / DELETE) route behind the admin middleware, and
//!    every key-lifecycle route on `/v1`, reaches a handler that writes an
//!    audit row, unless the handler is on an explicit, reasoned exemption
//!    list (telemetry ingest, executions, read-only probes). A new admin
//!    write with no audit call, or a handler that silently loses its call,
//!    turns this red.
//!
//! Base `1fa9d9c` had 54 `log_admin_action(` sites in `ares-http`, every one
//! of the form `tokio::spawn(async move { let _ = log_admin_action(..).await; })`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Floors: counted on the HEAD of `v4-1.16` after 1.16-FIX-1. They are minimums
// (adding a site or a route is fine; losing one is a regression).
// ---------------------------------------------------------------------------

/// Total audit call sites under `crates/ares-http/src` (all `audit_log::record(`;
/// no direct `log_admin_action(` call is left in this crate).
const AUDIT_SITE_FLOOR: usize = 79;

/// Minimum sites per file that carries admin or key-lifecycle writes (the exact
/// count on HEAD; the sum is `AUDIT_SITE_FLOOR`).
const PER_FILE_FLOOR: &[(&str, usize)] = &[
    ("api/handlers/admin/agents.rs", 11),
    ("api/handlers/admin/audit.rs", 8),
    ("api/handlers/admin/billing.rs", 5),
    ("api/handlers/admin/connectors.rs", 9),
    ("api/handlers/admin/cordis.rs", 9),
    ("api/handlers/admin/fleet_provider_keys.rs", 2),
    ("api/handlers/admin/health.rs", 2),
    ("api/handlers/admin/pipelines.rs", 4),
    ("api/handlers/admin/providers.rs", 2),
    ("api/handlers/admin/schedules.rs", 4),
    ("api/handlers/admin/shared.rs", 1),
    ("api/handlers/admin/tenants.rs", 6),
    ("api/handlers/admin/tools.rs", 5),
    ("api/handlers/admin/triggers.rs", 5),
    ("api/handlers/deploy.rs", 1),
    ("api/handlers/v1/agents.rs", 5),
];

/// Mutating routes found in `create_router`'s admin and `/v1` route tables.
const MUTATING_ROUTE_FLOOR: usize = 90;

// ---------------------------------------------------------------------------
// Source access
// ---------------------------------------------------------------------------

fn src_root() -> PathBuf {
    PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by cargo test"),
    )
    .join("src")
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|e| e.expect("dir entry").path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_rs(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

/// `(path relative to src/, file text)` for every `.rs` file under `src/`.
fn all_sources() -> Vec<(String, String)> {
    let root = src_root();
    let mut files = Vec::new();
    collect_rs(&root, &mut files);
    files
        .into_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(&root)
                .expect("under src root")
                .to_string_lossy()
                .replace('\\', "/");
            let text =
                std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
            (rel, text)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// A small Rust lexer: enough to scan code, not prose
// ---------------------------------------------------------------------------

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn blank(out: &mut [char], from: usize, to: usize) {
    for slot in out.iter_mut().take(to).skip(from) {
        if *slot != '\n' {
            *slot = ' ';
        }
    }
}

/// A copy of `src` (same length, same offsets, same newlines) with comments
/// and the *contents* of string and char literals replaced by spaces, so a
/// scan sees code only: a `// audit_log::record(` comment or a `"log_admin_action("`
/// string is not a call site, and a `'('` char cannot unbalance a paren match.
fn mask(src: &str) -> Vec<char> {
    let c: Vec<char> = src.chars().collect();
    let mut out = c.clone();
    let n = c.len();
    let mut i = 0;
    while i < n {
        let ch = c[i];
        if ch == '/' && c.get(i + 1) == Some(&'/') {
            let s = i;
            while i < n && c[i] != '\n' {
                i += 1;
            }
            blank(&mut out, s, i);
            continue;
        }
        if ch == '/' && c.get(i + 1) == Some(&'*') {
            let s = i;
            let mut depth = 0usize;
            while i < n {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            blank(&mut out, s, i);
            continue;
        }
        // Raw strings: r"..", r#".."#, br#".."#.
        let raw_start = ch == 'r'
            && (i == 0
                || !is_ident(c[i - 1])
                || (c[i - 1] == 'b' && (i < 2 || !is_ident(c[i - 2]))));
        if raw_start {
            let mut j = i + 1;
            let mut hashes = 0;
            while c.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if c.get(j) == Some(&'"') {
                let start = j + 1;
                let mut k = start;
                let mut closed = false;
                while k < n {
                    if c[k] == '"' {
                        let mut h = 0;
                        while h < hashes && c.get(k + 1 + h) == Some(&'#') {
                            h += 1;
                        }
                        if h == hashes {
                            blank(&mut out, start, k);
                            i = k + 1 + hashes;
                            closed = true;
                            break;
                        }
                    }
                    k += 1;
                }
                if !closed {
                    i = n;
                }
                continue;
            }
        }
        if ch == '"' {
            let start = i + 1;
            i += 1;
            while i < n && c[i] != '"' {
                if c[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            blank(&mut out, start, i.min(n));
            i += 1;
            continue;
        }
        if ch == '\'' {
            if c.get(i + 1) == Some(&'\\') {
                // Escaped char literal: '\n', '\'', '\u{..}'.
                let mut j = i + 3;
                while j < n && c[j] != '\'' {
                    j += 1;
                }
                blank(&mut out, i + 1, j);
                i = j + 1;
            } else if c.get(i + 2) == Some(&'\'') {
                blank(&mut out, i + 1, i + 2);
                i += 3;
            } else {
                // A lifetime or a loop label.
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    out
}

fn skip_ws(c: &[char], mut i: usize) -> usize {
    while i < c.len() && c[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Index of the bracket that closes the one at `open`, or `None` if unbalanced.
fn matching(c: &[char], open: usize, o: char, cl: char) -> Option<usize> {
    let mut depth = 0i32;
    for (k, &ch) in c.iter().enumerate().skip(open) {
        if ch == o {
            depth += 1;
        } else if ch == cl {
            depth -= 1;
            if depth == 0 {
                return Some(k);
            }
        }
    }
    None
}

fn line_of(c: &[char], pos: usize) -> usize {
    1 + c[..pos].iter().filter(|&&x| x == '\n').count()
}

/// Positions where the identifier path `needle` occurs as a whole token run
/// (not glued to a longer identifier on either side).
fn find_all(c: &[char], needle: &str) -> Vec<usize> {
    let nd: Vec<char> = needle.chars().collect();
    let mut out = Vec::new();
    if c.len() < nd.len() {
        return out;
    }
    for i in 0..=(c.len() - nd.len()) {
        if c[i..i + nd.len()] != nd[..] {
            continue;
        }
        if i > 0 && is_ident(c[i - 1]) {
            continue;
        }
        if c.get(i + nd.len()).is_some_and(|&x| is_ident(x)) {
            continue;
        }
        out.push(i);
    }
    out
}

/// The identifier ending just before `idx` (skipping whitespace), if any.
fn word_before(c: &[char], idx: usize) -> Option<(String, usize)> {
    let mut e = idx;
    while e > 0 && c[e - 1].is_whitespace() {
        e -= 1;
    }
    let mut s = e;
    while s > 0 && is_ident(c[s - 1]) {
        s -= 1;
    }
    if s == e {
        None
    } else {
        Some((c[s..e].iter().collect(), s))
    }
}

/// `(open paren, close paren, name)` of every call whose callee identifier
/// starts with `spawn` (`tokio::spawn(..)`, `tokio::task::spawn(..)`,
/// `spawn_blocking(..)`, `spawn_local(..)`, `JoinSet::spawn(..)`, `x.spawn(..)`,
/// `spawn::<T>(..)`). An unbalanced paren extends to the end of the file, the
/// conservative reading.
fn spawn_extents(c: &[char]) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let n = c.len();
    let mut i = 0;
    while i < n {
        let starts_ident =
            (c[i].is_ascii_alphabetic() || c[i] == '_') && (i == 0 || !is_ident(c[i - 1]));
        if !starts_ident {
            i += 1;
            continue;
        }
        let s = i;
        while i < n && is_ident(c[i]) {
            i += 1;
        }
        let name: String = c[s..i].iter().collect();
        if !name.starts_with("spawn") {
            continue;
        }
        let mut j = skip_ws(c, i);
        if c.get(j) == Some(&':') && c.get(j + 1) == Some(&':') {
            let lt = skip_ws(c, j + 2);
            if c.get(lt) == Some(&'<') {
                if let Some(gt) = matching(c, lt, '<', '>') {
                    j = skip_ws(c, gt + 1);
                }
            }
        }
        if c.get(j) == Some(&'(') {
            let close = matching(c, j, '(', ')').unwrap_or(n - 1);
            out.push((j, close, name));
        }
    }
    out
}

/// True when `pos` is lexically inside an `async { .. }` / `async move { .. }`
/// block: such a block is a future that can be spawned, joined or dropped away
/// from the call site.
fn inside_async_block(c: &[char], pos: usize) -> bool {
    let mut stack: Vec<usize> = Vec::new();
    for (k, &ch) in c.iter().enumerate().take(pos) {
        match ch {
            '{' => stack.push(k),
            '}' => {
                stack.pop();
            }
            _ => {}
        }
    }
    stack.iter().any(|&b| match word_before(c, b) {
        Some((w, _)) if w == "async" => true,
        Some((w, at)) if w == "move" => word_before(c, at).is_some_and(|(p, _)| p == "async"),
        _ => false,
    })
}

// ---------------------------------------------------------------------------
// Audit call sites
// ---------------------------------------------------------------------------

/// The two writers. `record` awaits the insert and logs a failure at `error`;
/// `log_admin_action` is the raw insert it wraps.
const WRITERS: [&str; 2] = ["audit_log::record", "log_admin_action"];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Defect {
    file: String,
    line: usize,
    reason: String,
}

#[derive(Debug, Default)]
struct Scan {
    /// Char offset (in the file) of the start of every call site.
    sites: Vec<usize>,
    defects: Vec<Defect>,
}

fn scan(file: &str, src: &str) -> Scan {
    let c = mask(src);
    let spawns = spawn_extents(&c);
    let mut result = Scan::default();
    let mut defect = |pos: usize, reason: String| {
        result.defects.push(Defect {
            file: file.to_string(),
            line: line_of(&c, pos),
            reason,
        });
    };
    let mut sites: Vec<usize> = Vec::new();

    for writer in WRITERS {
        for pos in find_all(&c, writer) {
            // A definition (`fn log_admin_action(`) is not a call site.
            if word_before(&c, pos).is_some_and(|(w, _)| w == "fn") {
                continue;
            }
            let after = skip_ws(&c, pos + writer.chars().count());
            if c.get(after) != Some(&'(') {
                defect(
                    pos,
                    format!(
                        "`{writer}` is referenced without being called: it could be handed to a \
                         helper or spawned as a function value"
                    ),
                );
                continue;
            }
            sites.push(pos);

            let Some(close) = matching(&c, after, '(', ')') else {
                defect(pos, format!("`{writer}(` has an unbalanced argument list"));
                continue;
            };

            // Rule 1: not inside the argument of any spawn form.
            if let Some((open, _, name)) = spawns.iter().find(|(o, cl, _)| *o < pos && pos < *cl) {
                defect(
                    pos,
                    format!(
                        "`{writer}(` sits inside the argument of `{name}(` opened at line {}: the \
                         write is detached from the response",
                        line_of(&c, *open)
                    ),
                );
            }

            // Rule 2: awaited where it is called.
            let tail = skip_ws(&c, close + 1);
            let awaited = c.get(tail) == Some(&'.') && {
                let w = skip_ws(&c, tail + 1);
                c[w..].iter().take(5).collect::<String>() == "await"
                    && !c.get(w + 5).is_some_and(|&x| is_ident(x))
            };
            if !awaited {
                defect(
                    pos,
                    format!(
                        "`{writer}(..)` is not `.await`ed where it is called (handed to a helper, \
                         bound to a variable, or dropped)"
                    ),
                );
            } else if writer == "log_admin_action" {
                // The Result must be handled, not thrown away.
                // Skip a path prefix (`ares_store::audit_log::`) to reach the
                // start of the call expression.
                let mut start = pos;
                while start > 0 && (is_ident(c[start - 1]) || c[start - 1] == ':') {
                    start -= 1;
                }
                let head: String = c[..start].iter().collect();
                let discarded_by_let = head
                    .trim_end()
                    .strip_suffix('=')
                    .map(str::trim_end)
                    .and_then(|t| t.strip_suffix('_'))
                    .is_some_and(|t| t.trim_end().ends_with("let"));
                let w = skip_ws(&c, tail + 1);
                let after_await = skip_ws(&c, w + 5);
                let discarded_by_ok = c.get(after_await) == Some(&'.') && {
                    let m: String = c[skip_ws(&c, after_await + 1)..]
                        .iter()
                        .take_while(|&&x| is_ident(x))
                        .collect();
                    matches!(
                        m.as_str(),
                        "ok" | "unwrap_or" | "unwrap_or_default" | "unwrap_or_else"
                    )
                };
                if discarded_by_let || discarded_by_ok {
                    defect(
                        pos,
                        "the Result of `log_admin_action(..)` is discarded".to_string(),
                    );
                }
            }

            // Rule 3: not inside an async block (which could be spawned later).
            if inside_async_block(&c, pos) {
                defect(
                    pos,
                    format!(
                        "`{writer}(` sits inside an `async` block: that future can be spawned or \
                         dropped away from the response; await the call in the handler body"
                    ),
                );
            }
        }
    }
    sites.sort_unstable();
    result.sites = sites;
    result
}

#[test]
fn audit_is_awaited_on_every_admin_write() {
    let mut total = 0usize;
    let mut per_file: BTreeMap<String, usize> = BTreeMap::new();
    let mut defects: Vec<Defect> = Vec::new();

    for (file, src) in all_sources() {
        let s = scan(&file, &src);
        if !s.sites.is_empty() {
            per_file.insert(file.clone(), s.sites.len());
        }
        total += s.sites.len();
        defects.extend(s.defects);
    }

    eprintln!(
        "audit call sites scanned: {total} in {} files",
        per_file.len()
    );
    for (f, k) in &per_file {
        eprintln!("  {k:3}  {f}");
    }
    if !defects.is_empty() {
        eprintln!("{} defect(s):", defects.len());
        for d in &defects {
            eprintln!("  {}:{}: {}", d.file, d.line, d.reason);
        }
    }

    assert!(
        defects.is_empty(),
        "{} audit call site(s) are not awaited directly in the handler (see stderr for the list): \
         every write must go through `ares_store::audit_log::record(..).await` where it is called, \
         never inside a spawn or an async block",
        defects.len()
    );
    assert!(
        total >= AUDIT_SITE_FLOOR,
        "scanned {total} audit call sites, fewer than the floor of {AUDIT_SITE_FLOOR}: an audit call \
         was removed, or the scan stopped matching the call shape (vacuous)"
    );
    for (file, floor) in PER_FILE_FLOOR {
        let got = per_file.get(*file).copied().unwrap_or(0);
        assert!(
            got >= *floor,
            "{file}: {got} audit call site(s), fewer than its floor of {floor}"
        );
    }
}

// ---------------------------------------------------------------------------
// Route coverage: every admin / key-lifecycle write reaches an audit call
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct MutatingRoute {
    method: String,
    path: String,
    module: &'static str,
    name: String,
}

enum Exempt {
    /// Not an admin or key-lifecycle configuration write; the reason says why.
    Reason(&'static str),
    /// The handler forwards to another handler in the same module that audits.
    Delegates(&'static str),
}

/// Mutating handlers that are deliberately not audited. Each entry is a
/// judgement, stated here so a reviewer can disagree with it in one place.
fn exemptions() -> Vec<((&'static str, &'static str), Exempt)> {
    use Exempt::*;
    vec![
        (("admin", "test_tenant_agent_handler"), Reason("runs a draft agent config once for the admin: an execution whose writes are agent_runs telemetry, not configuration")),
        (("admin", "run_skill"), Reason("runs a skill once: an execution whose writes are agent_runs telemetry, not configuration")),
        (("admin", "verify_fleet_provider"), Reason("read-only probe of a provider's model list; writes nothing")),
        (("admin", "insert_llm_call"), Reason("run-history telemetry ingest, one machine-written row per LLM call; the row is itself the record")),
        (("admin", "insert_tool_call"), Reason("run-history telemetry ingest, one machine-written row per tool call; the row is itself the record")),
        (("admin", "insert_health_metrics"), Reason("run-history telemetry ingest of agent health metrics; the row is itself the record")),
        (("admin", "update_tenant_schedule"), Delegates("update_schedule")),
        (("v1", "v1_chat"), Reason("metered agent execution on a tenant key; not an admin or key-lifecycle write")),
        (("v1", "v1_research"), Reason("metered agent execution on a tenant key; not an admin or key-lifecycle write")),
        (("v1", "run_agent"), Reason("metered agent execution on a tenant key; not an admin or key-lifecycle write")),
        (("v1", "semantic_search"), Reason("read-only similarity search; writes nothing")),
        (("v1", "ingest_usage_events"), Reason("tenant usage-metering ingest (usage_events); not an admin or key-lifecycle write")),
    ]
}

fn find_seq(c: &[char], needle: &str, from: usize) -> Option<usize> {
    let nd: Vec<char> = needle.chars().collect();
    if c.len() < nd.len() {
        return None;
    }
    (from..=c.len() - nd.len()).find(|&i| c[i..i + nd.len()] == nd[..])
}

/// Every POST / PUT / PATCH / DELETE registration in `create_router`'s admin
/// route table and its `/v1` route table.
fn mutating_routes(routes_src: &str) -> Vec<MutatingRoute> {
    let orig: Vec<char> = routes_src.chars().collect();
    let c = mask(routes_src);
    let tables = [
        (
            "let admin_routes = Router::new()",
            "admin_middleware(req, next)",
        ),
        (
            "let v1_metered_routes = Router::new()",
            "api_key_auth_middleware",
        ),
    ];
    let mut out = Vec::new();
    for (start_marker, end_marker) in tables {
        let start = find_seq(&c, start_marker, 0)
            .unwrap_or_else(|| panic!("routes.rs: start marker `{start_marker}` not found"));
        let end = find_seq(&c, end_marker, start)
            .unwrap_or_else(|| panic!("routes.rs: end marker `{end_marker}` not found"));
        let seg = &c[start..end];
        let mut i = 0;
        while i < seg.len() {
            let starts_ident = (seg[i].is_ascii_alphabetic() || seg[i] == '_')
                && (i == 0 || !is_ident(seg[i - 1]));
            if !starts_ident {
                i += 1;
                continue;
            }
            let s = i;
            while i < seg.len() && is_ident(seg[i]) {
                i += 1;
            }
            let name: String = seg[s..i].iter().collect();
            if !matches!(name.as_str(), "post" | "put" | "patch" | "delete") {
                continue;
            }
            let open = skip_ws(seg, i);
            if seg.get(open) != Some(&'(') {
                continue;
            }
            let Some(close) = matching(seg, open, '(', ')') else {
                continue;
            };
            let arg: String = seg[open + 1..close]
                .iter()
                .collect::<String>()
                .split_whitespace()
                .collect();
            let arg = arg.trim_end_matches(',').to_string();
            if arg.is_empty() || !arg.chars().all(|ch| is_ident(ch) || ch == ':') {
                continue;
            }
            let segments: Vec<&str> = arg.split("::").collect();
            let module = segments.iter().find_map(|s| match *s {
                "admin" => Some("admin"),
                "v1" => Some("v1"),
                "deploy" => Some("deploy"),
                _ => None,
            });
            let Some(module) = module else {
                panic!("routes.rs: cannot place mutating handler `{arg}` in admin/v1/deploy");
            };
            // The route's path literal, from the unmasked text.
            let abs = start + s;
            let route_at = (0..abs)
                .rev()
                .find(|&k| c[k..].starts_with(&['.', 'r', 'o', 'u', 't', 'e', '(']));
            let path = route_at
                .and_then(|r| {
                    let q1 = (r..orig.len()).find(|&k| orig[k] == '"')?;
                    let q2 = (q1 + 1..orig.len()).find(|&k| orig[k] == '"')?;
                    Some(orig[q1 + 1..q2].iter().collect::<String>())
                })
                .unwrap_or_default();
            // `create_router` is nested under `/api`; the v1 table under `/api/v1`.
            let path = match module {
                "v1" => format!("/api/v1{path}"),
                _ => format!("/api{path}"),
            };
            out.push(MutatingRoute {
                method: name.to_uppercase(),
                path,
                module,
                name: segments.last().expect("non-empty").to_string(),
            });
        }
    }
    out
}

/// `(file, body-open, body-close)` of every `fn <name>` in the files of `module`.
fn handler_bodies<'a>(
    sources: &'a [(String, Vec<char>)],
    module: &str,
    name: &str,
) -> Vec<(&'a str, &'a [char], usize, usize)> {
    let in_module = |file: &str| match module {
        "admin" => file == "api/handlers/admin.rs" || file.starts_with("api/handlers/admin/"),
        "v1" => file == "api/handlers/v1.rs" || file.starts_with("api/handlers/v1/"),
        "deploy" => file == "api/handlers/deploy.rs",
        _ => false,
    };
    let mut out = Vec::new();
    for (file, c) in sources.iter().filter(|(f, _)| in_module(f)) {
        for pos in find_all(c, "fn") {
            let nm_start = skip_ws(c, pos + 2);
            let nm: String = c[nm_start..].iter().take_while(|&&x| is_ident(x)).collect();
            if nm != name {
                continue;
            }
            let mut j = skip_ws(c, nm_start + nm.len());
            if c.get(j) == Some(&'<') {
                match matching(c, j, '<', '>') {
                    Some(gt) => j = skip_ws(c, gt + 1),
                    None => continue,
                }
            }
            if c.get(j) != Some(&'(') {
                continue;
            }
            let Some(params_end) = matching(c, j, '(', ')') else {
                continue;
            };
            // First `{` before any `;` after the parameter list is the body.
            let Some(rel) = c[params_end..].iter().position(|&x| x == '{' || x == ';') else {
                continue;
            };
            let open = params_end + rel;
            if c[open] != '{' {
                continue;
            }
            let Some(close) = matching(c, open, '{', '}') else {
                continue;
            };
            out.push((file.as_str(), c.as_slice(), open, close));
        }
    }
    out
}

#[test]
fn every_mutating_admin_route_is_audited_or_exempt() {
    let files = all_sources();
    let routes_src = &files
        .iter()
        .find(|(f, _)| f == "api/routes.rs")
        .expect("api/routes.rs exists")
        .1;
    let routes = mutating_routes(routes_src);
    let masked: Vec<(String, Vec<char>)> =
        files.iter().map(|(f, s)| (f.clone(), mask(s))).collect();
    let exempt = exemptions();

    let mut unaudited: Vec<String> = Vec::new();
    let mut used_exemptions: Vec<(&str, &str)> = Vec::new();
    let mut inventory: Vec<String> = Vec::new();

    for r in &routes {
        let bodies = handler_bodies(&masked, r.module, &r.name);
        let label = format!("{:6} {} -> {}::{}", r.method, r.path, r.module, r.name);
        if bodies.len() != 1 {
            unaudited.push(format!(
                "{label}: expected exactly one handler definition in module `{}`, found {}",
                r.module,
                bodies.len()
            ));
            continue;
        }
        let (_file, c, open, close) = bodies[0];
        let sites = WRITERS
            .iter()
            .flat_map(|w| find_all(&c[open..=close], w))
            .count();

        let ex = exempt
            .iter()
            .find(|((m, n), _)| *m == r.module && *n == r.name);
        match ex {
            Some((key, Exempt::Reason(why))) => {
                used_exemptions.push((key.0, key.1));
                inventory.push(format!("{label}  [exempt: {why}]"));
            }
            Some((key, Exempt::Delegates(target))) => {
                used_exemptions.push((key.0, key.1));
                let calls_target = !find_all(&c[open..=close], target).is_empty();
                let target_sites = handler_bodies(&masked, r.module, target)
                    .first()
                    .map(|(_, tc, to, tcl)| find_all(&tc[*to..=*tcl], "audit_log::record").len())
                    .unwrap_or(0);
                inventory.push(format!(
                    "{label}  [delegates to {target}: {target_sites} site(s)]"
                ));
                if !calls_target || target_sites == 0 {
                    unaudited.push(format!(
                        "{label}: declared as delegating to `{target}`, which must be called from \
                         the handler and must audit"
                    ));
                }
            }
            None => {
                inventory.push(format!("{label}  [{sites} audit site(s)]"));
                if sites == 0 {
                    unaudited.push(format!("{label}: no audit call in the handler"));
                }
            }
        }
    }

    eprintln!("mutating routes found: {}", routes.len());
    for line in &inventory {
        eprintln!("  {line}");
    }

    for ((m, n), _) in &exempt {
        assert!(
            used_exemptions.contains(&(*m, *n)),
            "stale exemption `{m}::{n}`: no mutating route reaches it any more; delete the entry"
        );
    }
    assert!(
        unaudited.is_empty(),
        "{} mutating route(s) behind the admin middleware / on the /v1 key lifecycle reach no audit \
         call and are not exempt:\n  {}",
        unaudited.len(),
        unaudited.join("\n  ")
    );
    assert!(
        routes.len() >= MUTATING_ROUTE_FLOOR,
        "found {} mutating routes, fewer than the floor of {MUTATING_ROUTE_FLOOR}: the route table \
         changed shape or the extractor stopped matching (vacuous)",
        routes.len()
    );
}

// ---------------------------------------------------------------------------
// The scanner's own tests: each defect shape it must catch, and the shapes it
// must let through, on in-memory fixtures.
// ---------------------------------------------------------------------------

fn fixture(body: &str) -> Scan {
    scan(
        "fixture.rs",
        &format!("async fn handler(pool: PgPool) {{\n{body}\n}}\n"),
    )
}

const CALL: &str = "audit_log::record(&pool, \"a\", \"b\", \"c\", None, None, None)";

#[test]
fn scanner_accepts_an_awaited_call() {
    let s = fixture(&format!("    {CALL}.await;"));
    assert_eq!(s.sites.len(), 1);
    assert!(s.defects.is_empty(), "{:?}", s.defects);
}

#[test]
fn scanner_accepts_await_on_the_next_line_and_a_path_prefix() {
    let s = fixture(
        "    ares_store::audit_log::record(\n        &pool,\n        \"a\",\n        \"b\",\n        \"c\",\n        None,\n        None,\n        None,\n    )\n    .await;",
    );
    assert_eq!(s.sites.len(), 1);
    assert!(s.defects.is_empty(), "{:?}", s.defects);
}

#[test]
fn scanner_flags_a_spawn_wrapped_call_in_every_spawn_form() {
    for spawn in [
        "tokio::spawn",
        "tokio::task::spawn",
        "tokio::task::spawn_blocking",
        "tokio::task::spawn_local",
        "set.spawn",
        "handle.spawn",
        "JoinSet::spawn",
        "spawn",
        "tokio::spawn::<()>",
    ] {
        let s = fixture(&format!(
            "    {spawn}(async move {{\n        {CALL}.await;\n    }});"
        ));
        assert_eq!(s.sites.len(), 1, "{spawn}");
        assert!(
            s.defects
                .iter()
                .any(|d| d.reason.contains("inside the argument of")),
            "{spawn}: {:?}",
            s.defects
        );
    }
}

#[test]
fn scanner_flags_a_spawn_even_after_earlier_closing_braces_in_it() {
    // The old line-window scan stopped at the first `});` it met and missed
    // this. The extent is the balanced argument of the spawn, whatever it holds.
    let s = fixture(&format!(
        "    tokio::spawn(async move {{\n        items.iter().for_each(|x| {{ touch(x) }});\n        \
         other(|| {{ 1 }});\n        {CALL}.await;\n    }});"
    ));
    assert!(
        s.defects
            .iter()
            .any(|d| d.reason.contains("inside the argument of")),
        "{:?}",
        s.defects
    );
}

#[test]
fn scanner_flags_a_call_handed_to_a_helper_bound_or_dropped() {
    for (label, body) in [
        ("helper", format!("    detach({CALL});")),
        ("bound", format!("    let fut = {CALL};\n    detach(fut);")),
        ("let _", format!("    let _ = {CALL};")),
        ("dropped", format!("    {CALL};")),
        (
            "timeout",
            format!("    tokio::time::timeout(d, {CALL}).await.ok();"),
        ),
    ] {
        let s = fixture(&body);
        assert!(
            s.defects
                .iter()
                .any(|d| d.reason.contains("not `.await`ed")),
            "{label}: {:?}",
            s.defects
        );
    }
}

#[test]
fn scanner_flags_a_function_value_reference() {
    let s = scan(
        "fixture.rs",
        "use ares_store::audit_log::record;\nfn f() { let g = audit_log::record; }\n",
    );
    assert_eq!(s.defects.len(), 2, "{:?}", s.defects);
    assert!(s
        .defects
        .iter()
        .all(|d| d.reason.contains("without being called")));
}

#[test]
fn scanner_flags_a_call_inside_an_async_block_bound_to_a_variable() {
    let s = fixture(&format!(
        "    let job = async move {{\n        {CALL}.await;\n    }};\n    tokio::spawn(job);"
    ));
    assert!(
        s.defects
            .iter()
            .any(|d| d.reason.contains("inside an `async` block")),
        "{:?}",
        s.defects
    );
}

#[test]
fn scanner_flags_a_discarded_log_admin_action_result() {
    let call = "log_admin_action(&pool, \"a\", \"b\", \"c\", None, None, None)";
    for (label, body) in [
        ("let _", format!("    let _ = {call}.await;")),
        (
            "let _ with a path",
            format!("    let _ = ares_store::audit_log::{call}.await;"),
        ),
        ("ok()", format!("    {call}.await.ok();")),
    ] {
        let s = fixture(&body);
        assert!(
            s.defects.iter().any(|d| d.reason.contains("discarded")),
            "{label}: {:?}",
            s.defects
        );
    }
    // Handled: an `if let Err` on the awaited call is the wrapper's shape.
    let s = fixture(&format!(
        "    if let Err(e) = {call}.await {{ tracing::error!(%e); }}"
    ));
    assert!(s.defects.is_empty(), "{:?}", s.defects);
}

#[test]
fn scanner_lets_a_nearby_spawn_that_does_not_contain_the_call_through() {
    // deploy.rs shape: a spawn for the deploy process, then the audit call
    // after it, outside the spawned argument.
    let s = fixture(&format!(
        "    tokio::spawn(async move {{ run_deploy().await; }});\n    {CALL}.await;"
    ));
    assert_eq!(s.sites.len(), 1);
    assert!(s.defects.is_empty(), "{:?}", s.defects);
}

#[test]
fn scanner_ignores_comments_and_strings() {
    let s = scan(
        "fixture.rs",
        "// tokio::spawn(audit_log::record(..));\n/* log_admin_action( */\nfn f() { let s = \"audit_log::record(\"; let r = r#\"log_admin_action(\"#; let c = '('; }\n",
    );
    assert!(s.sites.is_empty(), "{:?}", s.sites);
    assert!(s.defects.is_empty(), "{:?}", s.defects);
}

#[test]
fn scanner_does_not_take_a_definition_or_a_longer_name_for_a_call() {
    let s = scan(
        "fixture.rs",
        "pub async fn log_admin_action(pool: &PgPool) -> Result<()> { Ok(()) }\nfn g() { audit_log::record_failure(1); my_log_admin_action(2); }\n",
    );
    assert!(s.sites.is_empty(), "{:?}", s.sites);
    assert!(s.defects.is_empty(), "{:?}", s.defects);
}

// ---------------------------------------------------------------------------
// 1.16-FIX-2: the shapes gate round 2 compiled past the scan (a nested async
// fn, an async closure, a macro; each first case below is the reviewer's own
// sabotage, verbatim in shape), the helper they generalise to, and the route
// inventory's silent skip of a handler that is not a plain path.
// ---------------------------------------------------------------------------

#[test]
fn scanner_flags_a_call_in_a_nested_fn() {
    for (label, body) in [
        // Gate round 2, `acknowledge_budget_alert`: a nested async fn, spawned.
        (
            "nested async fn, spawned",
            format!(
                "    async fn detached_audit(pool: PgPool) {{\n        {CALL}.await;\n    }}\n    \
                 tokio::spawn(detached_audit(pool.clone()));"
            ),
        ),
        (
            "nested async fn, awaited",
            format!(
                "    async fn inline_audit(pool: &PgPool) {{\n        {CALL}.await;\n    }}\n    \
                 inline_audit(&pool).await;"
            ),
        ),
        (
            "method of a type declared in the body",
            format!(
                "    struct Auditor(PgPool);\n    impl Auditor {{\n        async fn write(&self) {{\n            \
                 let pool = &self.0;\n            {CALL}.await;\n        }}\n    }}\n    \
                 Auditor(pool.clone()).write().await;"
            ),
        ),
    ] {
        let s = fixture(&body);
        assert!(
            s.defects.iter().any(|d| d.reason.contains("nested")),
            "{label}: {:?}",
            s.defects
        );
    }
}

#[test]
fn scanner_flags_a_call_in_an_async_closure() {
    for (label, body) in [
        // Gate round 2, `reset_token_budget_period`: an async closure run in a spawn.
        (
            "async move || in a spawn",
            format!(
                "    let job = async move || {{\n        {CALL}.await;\n    }};\n    \
                 tokio::spawn(async move {{ job().await }});"
            ),
        ),
        (
            "async ||",
            format!("    let job = async || {{\n        {CALL}.await;\n    }};\n    job().await;"),
        ),
        (
            "async |p|",
            format!(
                "    let job = async |p: PgPool| {{\n        {CALL}.await;\n    }};\n    \
                 job(pool.clone()).await;"
            ),
        ),
        (
            "async move |p| with an expression body",
            format!("    let job = async move |p: PgPool| {CALL}.await;\n    job(pool.clone()).await;"),
        ),
        (
            "async move || with a return type",
            format!(
                "    let job = async move || -> () {{\n        {CALL}.await;\n    }};\n    job().await;"
            ),
        ),
    ] {
        let s = fixture(&body);
        assert!(
            s.defects.iter().any(|d| d.reason.contains("async` closure")),
            "{label}: {:?}",
            s.defects
        );
    }
}

#[test]
fn scanner_flags_a_call_in_a_macro_invocation() {
    for (label, body) in [
        // Gate round 2, `delete_tenant_data`: a macro whose expansion spawns.
        (
            "macro_rules! wrapping a spawn",
            format!(
                "    macro_rules! in_background {{\n        ($($body:tt)*) => {{\n            \
                 tokio::spawn(async move {{ $($body)* }})\n        }};\n    }}\n    \
                 let _h = in_background!({CALL}.await;);"
            ),
        ),
        (
            "a call inside a macro_rules! body",
            format!(
                "    macro_rules! audit {{\n        () => {{\n            {CALL}.await\n        }};\n    }}\n    \
                 audit!();"
            ),
        ),
        ("tokio::join!", format!("    tokio::join!({CALL}, other());")),
        ("square brackets", format!("    run![{CALL}.await];")),
        ("braces", format!("    run! {{ {CALL}.await }}")),
    ] {
        let s = fixture(&body);
        assert!(
            s.defects
                .iter()
                .any(|d| d.reason.contains("inside the arguments of the macro")),
            "{label}: {:?}",
            s.defects
        );
    }
}

#[test]
fn scanner_follows_a_function_that_audits_to_its_callers() {
    let helper = format!("async fn audit_later(pool: PgPool) {{\n    {CALL}.await;\n}}\n");
    let method = format!(
        "struct Auditor(PgPool);\nimpl Auditor {{\n    async fn write(&self) {{\n        \
         let pool = &self.0;\n        {CALL}.await;\n    }}\n}}\n"
    );
    for (label, src) in [
        (
            "spawned",
            format!("{helper}pub async fn handler(pool: PgPool) {{\n    tokio::spawn(audit_later(pool.clone()));\n}}\n"),
        ),
        (
            "not awaited",
            format!("{helper}pub async fn handler(pool: PgPool) {{\n    let fut = audit_later(pool);\n    detach(fut);\n}}\n"),
        ),
        (
            "in an async block",
            format!("{helper}pub async fn handler(pool: PgPool) {{\n    let job = async move {{ audit_later(pool).await }};\n    tokio::spawn(job);\n}}\n"),
        ),
        (
            "in a macro",
            format!("{helper}pub async fn handler(pool: PgPool) {{\n    in_background!(audit_later(pool).await);\n}}\n"),
        ),
        (
            "taken as a value",
            format!("{helper}pub async fn handler(pool: PgPool) {{\n    let f = audit_later;\n    tokio::spawn(f(pool));\n}}\n"),
        ),
        (
            "imported under another name",
            format!("use self::audit_later as later;\n{helper}pub async fn handler(pool: PgPool) {{\n    tokio::spawn(later(pool));\n}}\n"),
        ),
        (
            "through a second helper",
            format!("{helper}async fn relay(pool: PgPool) {{\n    audit_later(pool).await;\n}}\npub async fn handler(pool: PgPool) {{\n    tokio::spawn(relay(pool));\n}}\n"),
        ),
        (
            "a method, spawned",
            format!("{method}pub async fn handler(a: Auditor) {{\n    tokio::spawn(async move {{ a.write().await }});\n}}\n"),
        ),
    ] {
        let s = scan("fixture.rs", &src);
        assert!(
            s.defects
                .iter()
                .any(|d| d.reason.contains("a function that writes an audit row")),
            "{label}: {:?}",
            s.defects
        );
    }
}

#[test]
fn scanner_lets_an_awaited_helper_a_route_registration_and_a_same_named_method_through() {
    let s = scan(
        "fixture.rs",
        &format!(
            "use super::tenants::{{audit_now, other}};\n\
             async fn audit_now(pool: &PgPool) {{\n    {CALL}.await;\n}}\n\
             pub async fn handler(pool: PgPool, store: Store) {{\n    \
             store.audit_now(1).map(|x| x);\n    tracing::info!(\"before\");\n    \
             audit_now(&pool).await;\n    \
             audit_log::record(&pool, \"a\", \"b\", &format!(\"{{}}\", 1), None, None, None).await;\n}}\n\
             pub fn routes() -> Router {{\n    Router::new()\n        \
             .route(\"/x\", post(crate::api::handlers::admin::handler))\n        \
             .route(\"/y\", get(audit_now).delete(handler))\n}}\n"
        ),
    );
    assert_eq!(s.sites.len(), 2, "{:?}", s.sites);
    assert!(s.defects.is_empty(), "{:?}", s.defects);
}

/// A small `create_router` with both tables and their markers; `/*EXTRA*/`
/// marks where a test adds to the admin table.
const ROUTES_FIXTURE: &str = r#"pub fn create_router() -> Router {
    let admin_routes = Router::new()
        .route(
            "/admin/tenants",
            post(crate::api::handlers::admin::create_tenant)
                .get(crate::api::handlers::admin::list_tenants),
        )
        /*EXTRA*/
        .layer(middleware::from_fn(move |req: Request, next: Next| async move {
            crate::api::handlers::admin::admin_middleware(req, next).await
        }));
    let v1_metered_routes = Router::new()
        .route("/chat", post(crate::api::handlers::v1::v1_chat));
    let v1_routes = Router::new()
        .merge(v1_metered_routes)
        .route(
            "/api-keys/{id}",
            delete(crate::api::handlers::v1::revoke_api_key),
        );
    let v1_routes = v1_routes.layer(middleware::from_fn(
        crate::middleware::api_key_auth::api_key_auth_middleware,
    ));
    public_routes.merge(admin_routes).nest("/v1", v1_routes)
}
"#;

fn routes_with(extra: &str) -> String {
    ROUTES_FIXTURE.replace("/*EXTRA*/", extra)
}

#[test]
fn route_inventory_reads_both_tables_and_an_inline_merge() {
    let got: Vec<String> = mutating_routes(&routes_with(""))
        .iter()
        .map(|r| format!("{} {} {}::{}", r.method, r.path, r.module, r.name))
        .collect();
    assert_eq!(
        got,
        vec![
            "POST /api/admin/tenants admin::create_tenant",
            "POST /api/v1/chat v1::v1_chat",
            "DELETE /api/v1/api-keys/{id} v1::revoke_api_key",
        ]
    );
}

#[test]
#[should_panic(expected = "is not a plain path")]
fn route_inventory_fails_closed_on_a_closure_handler() {
    // Gate round 2's sabotage: a closure handler that writes and never audits.
    mutating_routes(&routes_with(
        r#".route(
            "/admin/tenants/{id}/budget-wipe",
            delete(
                |axum::extract::State(ctx): axum::extract::State<Arc<Context>>,
                 axum::extract::Path(id): axum::extract::Path<String>| async move {
                    if let Some(db) = ctx.get::<TenantDb>() {
                        let _ = sqlx::query("DELETE FROM tenant_budgets WHERE tenant_id = $1")
                            .bind(id)
                            .execute(db.pool())
                            .await;
                    }
                    axum::http::StatusCode::NO_CONTENT
                },
            ),
        )"#,
    ));
}

#[test]
#[should_panic(expected = "a router the inventory cannot see")]
fn route_inventory_fails_closed_on_a_merge_it_cannot_see() {
    mutating_routes(&routes_with(
        ".merge(crate::api::handlers::admin::billing::routes())",
    ));
}

#[test]
#[should_panic(expected = "a router the inventory cannot see")]
fn route_inventory_fails_closed_on_a_nest_it_cannot_see() {
    mutating_routes(&routes_with(".nest(\"/extra\", extra_routes)"));
}

#[test]
fn route_inventory_fails_closed_on_a_router_call_it_does_not_know() {
    for extra in [
        ".route(\"/admin/x\", any(crate::api::handlers::admin::wipe))",
        ".route(\"/admin/x\", on(MethodFilter::DELETE, crate::api::handlers::admin::wipe))",
        ".route(\"/admin/x\", wipe_router)",
        ".route_service(\"/admin/x\", wipe_service)",
        ".fallback(crate::api::handlers::admin::wipe)",
    ] {
        let src = routes_with(extra);
        let read = std::panic::catch_unwind(|| mutating_routes(&src));
        assert!(read.is_err(), "`{extra}` was read without complaint");
    }
}
