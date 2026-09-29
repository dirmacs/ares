//! Structural regression tests for item 1.16 (VERIFY-2026-09-22.md §4 row 20).
//!
//! The goal: every admin write and every key-lifecycle write leaves exactly
//! one `admin_audit_log` row, with its actor, **before the response is
//! returned**. Two properties are enforced here by reading this crate's own
//! source text (no database, no network, no `include_str!` list to forget to
//! extend: the whole of `crates/ares-http/src` is walked at run time).
//!
//! 1. `audit_is_awaited_on_every_admin_write`. An audit call site is a call
//!    of `audit_log::record(` or `log_admin_action(`. Every site is `.await`ed
//!    exactly where it is called, in the body of a function that is not
//!    nested inside another function, and sits in none of these:
//!    - the argument of a spawn form (`tokio::spawn`, `tokio::task::spawn`,
//!      `spawn_blocking`, `spawn_local`, `JoinSet::spawn`, any `.spawn(`),
//!      judged by the balanced extent of the spawned argument;
//!    - an `async` block or an `async` closure (`async ||`, `async move ||`,
//!      `async |..|`): that future can be spawned or dropped elsewhere;
//!    - the arguments of any macro invocation, a `macro_rules!` body
//!      included: a macro can expand to a spawn. No macro is allowed, because
//!      no audit call sits in one today;
//!    - a `fn` or `async fn` nested inside another function's body.
//!
//!    The same rules hold, transitively, for every call of a function whose
//!    body holds a site or a checked call: a helper that audits cannot be
//!    spawned, left un-awaited, run in an async block, closure or macro, or
//!    nested, by any caller. The one exception is a macro listed by name in
//!    `CALLER_MACROS` (today only `assert!`, which evaluates its condition in
//!    place; a unit test calls a handler inside it); a `macro_rules!` or an
//!    `as` import that defines a listed name is itself a defect. Such a
//!    function is named without being called
//!    only as the plain-path handler of a route (`post(path::handler)`) or in
//!    a `use` list (never renamed with `as`), and the writer itself is never
//!    imported by a glob, a use list or `as`, where its calls could not be
//!    found by name. The test also asserts that it scanned at least a stated
//!    number of sites (in total and per file), so it can never go vacuous
//!    again: at `6aaf2da` a scan keyed on the old function name matched 0
//!    sites and a spawn-wrapped `record(...)` passed.
//! 2. `every_mutating_admin_route_is_audited_or_exempt`: every mutating
//!    (POST / PUT / PATCH / DELETE) route behind the admin middleware, and
//!    every key-lifecycle route on `/v1`, reaches a handler that writes an
//!    audit row, unless the handler is on an explicit, reasoned exemption
//!    list (telemetry ingest, executions, read-only probes). A new admin
//!    write with no audit call, or a handler that silently loses its call,
//!    turns this red. The two route tables are read fail-closed: a handler
//!    argument that is not a plain path (a closure, a variable), a method
//!    router other than `get`, `post`, `put`, `patch`, `delete`, `head`,
//!    `options` and `trace`, a router call other than `.route`, `.layer`,
//!    `.route_layer`, `.merge` and `.nest`, and a `.merge(` or `.nest(` of a
//!    router not written inline in the two tables each panic with the route
//!    and the text, instead of being skipped.
//!
//! What a source scan does not prove, stated so nobody reads more into it:
//! - calls are matched by name, not resolved: any function of the same name
//!   in the crate counts as the same function (the scan errs towards
//!   flagging), and method-call syntax (`x.name(`) is followed only when the
//!   auditing function takes `self`;
//! - macros are not expanded: an attribute macro that rewrites a function
//!   body is invisible to it (`ares-http` defines none; a new one needs a new
//!   dependency, which a diff shows);
//! - it proves where the call is and that it is awaited, not what the row
//!   says or that it follows the right write: `audit_writes_live.rs` tests
//!   the rows.
//!
//! Base `1fa9d9c` had 54 `log_admin_action(` sites in `ares-http`, every one
//! of the form `tokio::spawn(async move { let _ = log_admin_action(..).await; })`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Floors: counted on the HEAD of `v4-1.16` after 1.16-FIX-2 (79 after FIX-1;
// FIX-2 adds the `create_tenant` row in `provision_client` and the second
// `patch_cordis_entry` site, the one written before the save when a move ran).
// They are minimums (adding a site or a route is fine; losing one is a
// regression).
// ---------------------------------------------------------------------------

/// Total audit call sites under `crates/ares-http/src` (all `audit_log::record(`;
/// no direct `log_admin_action(` call is left in this crate).
const AUDIT_SITE_FLOOR: usize = 81;

/// Minimum sites per file that carries admin or key-lifecycle writes (the exact
/// count on HEAD; the sum is `AUDIT_SITE_FLOOR`).
const PER_FILE_FLOOR: &[(&str, usize)] = &[
    ("api/handlers/admin/agents.rs", 11),
    ("api/handlers/admin/audit.rs", 8),
    ("api/handlers/admin/billing.rs", 5),
    ("api/handlers/admin/connectors.rs", 9),
    ("api/handlers/admin/cordis.rs", 10),
    ("api/handlers/admin/fleet_provider_keys.rs", 2),
    ("api/handlers/admin/health.rs", 2),
    ("api/handlers/admin/pipelines.rs", 4),
    ("api/handlers/admin/providers.rs", 2),
    ("api/handlers/admin/schedules.rs", 4),
    ("api/handlers/admin/shared.rs", 1),
    ("api/handlers/admin/tenants.rs", 7),
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

/// The identifier that starts at `at` (empty when none does).
fn ident_at(c: &[char], at: usize) -> String {
    c.iter().skip(at).take_while(|&&x| is_ident(x)).collect()
}

/// The last non-whitespace character before `idx`, if any.
fn char_before(c: &[char], idx: usize) -> Option<char> {
    c[..idx.min(c.len())]
        .iter()
        .rev()
        .find(|x| !x.is_whitespace())
        .copied()
}

/// True when a `.await` follows the `)` at `close`.
fn awaited_after(c: &[char], close: usize) -> bool {
    let tail = skip_ws(c, close + 1);
    c.get(tail) == Some(&'.') && ident_at(c, skip_ws(c, tail + 1)) == "await"
}

/// Where an expression that starts at `from` ends: the first `,` or `;`
/// outside brackets, or the bracket that closes a group opened before `from`.
fn expression_end(c: &[char], from: usize) -> usize {
    let mut depth = 0i32;
    for (k, &ch) in c.iter().enumerate().skip(from) {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                if depth == 0 {
                    return k;
                }
                depth -= 1;
            }
            ',' | ';' if depth == 0 => return k,
            _ => {}
        }
    }
    c.len()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AsyncKind {
    Block,
    Closure,
}

/// `(start, end, kind)` of every `async { .. }` / `async move { .. }` block and
/// every async closure (`async || ..`, `async move |x| ..`, with a block or an
/// expression body): each is a future that can be spawned, joined or dropped
/// away from where it is written. An unbalanced extent runs to the end of the
/// file, the conservative reading.
fn async_extents(c: &[char]) -> Vec<(usize, usize, AsyncKind)> {
    let n = c.len();
    let mut out = Vec::new();
    for at in find_all(c, "async") {
        let mut j = skip_ws(c, at + 5);
        if ident_at(c, j) == "move" {
            j = skip_ws(c, j + 4);
        }
        match c.get(j) {
            Some('{') => {
                let end = matching(c, j, '{', '}').unwrap_or(n);
                out.push((at, end, AsyncKind::Block));
            }
            Some('|') => {
                // The parameters: `||`, or up to the next `|` outside brackets.
                let mut k = j + 1;
                if c.get(k) != Some(&'|') {
                    let mut depth = 0i32;
                    while k < n {
                        match c[k] {
                            '(' | '[' | '{' => depth += 1,
                            ')' | ']' | '}' => depth -= 1,
                            '|' if depth == 0 => break,
                            _ => {}
                        }
                        k += 1;
                    }
                }
                let body = skip_ws(c, k + 1);
                let end = if c.get(body) == Some(&'-') && c.get(body + 1) == Some(&'>') {
                    // A return type: the body is the block that follows it.
                    match (body..n).find(|&q| c[q] == '{') {
                        Some(open) => matching(c, open, '{', '}').unwrap_or(n),
                        None => n,
                    }
                } else if c.get(body) == Some(&'{') {
                    matching(c, body, '{', '}').unwrap_or(n)
                } else {
                    expression_end(c, body)
                };
                out.push((at, end, AsyncKind::Closure));
            }
            // `async fn`, or `async` in any other position.
            _ => {}
        }
    }
    out
}

/// Keywords that can stand before a `!` that is not a macro call
/// (`return !ok`, `if !(a)`, `impl !Send`, `match !x`).
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "box", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true",
    "type", "unsafe", "use", "where", "while", "yield",
];

/// `(open, close, name)` of every macro invocation: `name!(..)`, `name![..]`,
/// `name!{..}`, `path::name!(..)`, and `macro_rules! name {..}` itself. The
/// extent is the invocation's balanced delimiter; an unbalanced one runs to
/// the end of the file.
fn macro_extents(c: &[char]) -> Vec<(usize, usize, String)> {
    let n = c.len();
    let mut out = Vec::new();
    for (i, &ch) in c.iter().enumerate() {
        if ch != '!' || c.get(i + 1) == Some(&'=') {
            continue;
        }
        let Some((name, _)) = word_before(c, i) else {
            continue;
        };
        if KEYWORDS.contains(&name.as_str()) || name.starts_with(|x: char| x.is_ascii_digit()) {
            continue;
        }
        let mut j = skip_ws(c, i + 1);
        if name == "macro_rules" {
            j = skip_ws(c, j + ident_at(c, j).chars().count());
        }
        let (o, cl) = match c.get(j) {
            Some('(') => ('(', ')'),
            Some('[') => ('[', ']'),
            Some('{') => ('{', '}'),
            _ => continue,
        };
        out.push((j, matching(c, j, o, cl).unwrap_or(n), name));
    }
    out
}

/// One `fn` with a body.
#[derive(Debug, Clone)]
struct FnDef {
    name: String,
    /// Offsets of the `{` and the `}` of its body.
    open: usize,
    close: usize,
    /// Takes `self`, so its callers can use method-call syntax.
    is_method: bool,
}

/// True when a parameter list starts with `self`, `&self`, `&mut self`,
/// `&'a self`, `mut self` or `self: ..`.
fn takes_self(params: &str) -> bool {
    let mut t = params.trim_start().trim_start_matches('&').trim_start();
    if let Some(rest) = t.strip_prefix('\'') {
        t = rest.trim_start_matches(is_ident).trim_start();
    }
    if let Some(rest) = t.strip_prefix("mut") {
        if rest.starts_with(char::is_whitespace) {
            t = rest.trim_start();
        }
    }
    t.strip_prefix("self")
        .is_some_and(|rest| !rest.starts_with(is_ident))
}

/// Every `fn name(..) .. { .. }` in `c`. Declarations without a body and
/// `fn(..)` pointer types are skipped.
fn fn_defs(c: &[char]) -> Vec<FnDef> {
    let n = c.len();
    let mut out = Vec::new();
    for pos in find_all(c, "fn") {
        let name_at = skip_ws(c, pos + 2);
        let name = ident_at(c, name_at);
        if name.is_empty() {
            continue;
        }
        let mut j = skip_ws(c, name_at + name.chars().count());
        if c.get(j) == Some(&'<') {
            // Generics; the `>` of a `->` inside a bound is not a closer.
            let mut depth = 0i32;
            let mut k = j;
            while k < n {
                match c[k] {
                    '<' => depth += 1,
                    '>' if c[k - 1] != '-' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                k += 1;
            }
            j = skip_ws(c, k + 1);
        }
        if c.get(j) != Some(&'(') {
            continue;
        }
        let Some(params_end) = matching(c, j, '(', ')') else {
            continue;
        };
        // The first `{` before any `;` after the parameter list is the body.
        let Some(open) = (params_end..n).find(|&q| c[q] == '{' || c[q] == ';') else {
            continue;
        };
        if c[open] != '{' {
            continue;
        }
        let params: String = c[j + 1..params_end].iter().collect();
        out.push(FnDef {
            name,
            open,
            close: matching(c, open, '{', '}').unwrap_or(n),
            is_method: takes_self(&params),
        });
    }
    out
}

/// The functions whose bodies contain `pos`, outermost first.
fn enclosing_fns(fns: &[FnDef], pos: usize) -> Vec<&FnDef> {
    let mut v: Vec<&FnDef> = fns
        .iter()
        .filter(|f| f.open < pos && pos < f.close)
        .collect();
    v.sort_by_key(|f| f.open);
    v
}

// ---------------------------------------------------------------------------
// Route tables: where a function that writes an audit row may be named as a
// route's handler without being called
// ---------------------------------------------------------------------------

/// The two route tables in `create_router` (`api/routes.rs`), each read from
/// its start marker to its end marker.
const TABLES: [(&str, &str); 2] = [
    (
        "let admin_routes = Router::new()",
        "admin_middleware(req, next)",
    ),
    (
        "let v1_metered_routes = Router::new()",
        "api_key_auth_middleware",
    ),
];

/// A route table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RouteTable {
    /// One of the two tables in `create_router` (`api/routes.rs`).
    CreateRouter,
    /// A method chain that starts at an inline `Router::new()`: the Cordis
    /// RouteSet routers (`pub fn routes()` in each handler module) and
    /// `build_routes`.
    InlineRouter,
}

/// A route's handler: the plain path from `from` to `to` that is the only
/// argument of a method router in a route table's `.route(` call.
#[derive(Debug, Clone, Copy)]
struct RouteSlot {
    from: usize,
    to: usize,
    table: RouteTable,
}

/// `(start, end)` of each of the two tables found in `c`, the masked text of
/// `api/routes.rs` (the route inventory panics on a missing one).
fn table_extents(c: &[char]) -> Vec<(usize, usize)> {
    TABLES
        .iter()
        .filter_map(|&(start_marker, end_marker)| {
            let start = find_seq(c, start_marker, 0)?;
            Some((start, find_seq(c, end_marker, start)?))
        })
        .collect()
}

/// Index of the bracket that opens the one that closes at `close`.
fn matching_back(c: &[char], close: usize, o: char, cl: char) -> Option<usize> {
    let mut depth = 0i32;
    for (k, &ch) in c[..=close].iter().enumerate().rev() {
        if ch == cl {
            depth += 1;
        } else if ch == o {
            depth -= 1;
            if depth == 0 {
                return Some(k);
            }
        }
    }
    None
}

/// The last position before `idx` that is not whitespace, if any.
fn prev_non_ws(c: &[char], idx: usize) -> Option<usize> {
    (0..idx.min(c.len())).rev().find(|&k| !c[k].is_whitespace())
}

/// Where the path segment before `end` ends, stepping back over a turbofish
/// (`name::<..>`) when one ends just before `end`; `None` when a `>` there
/// closes no turbofish.
fn before_turbofish(c: &[char], end: usize) -> Option<usize> {
    let gt = prev_non_ws(c, end)?;
    if c[gt] != '>' {
        return Some(end);
    }
    let lt = matching_back(c, gt, '<', '>')?;
    let colon = prev_non_ws(c, lt)?;
    (colon >= 1 && c[colon] == ':' && c[colon - 1] == ':').then_some(colon - 1)
}

/// True when the method chain that the call at the `.` at `dot` belongs to
/// starts at an inline `Router::new()` (also `axum::Router::new()` and
/// `Router::<S>::new()`): an axum router built in place, the way a route
/// table is written.
fn chain_starts_at_router_new(c: &[char], dot: usize) -> bool {
    let mut dot = dot;
    loop {
        // The receiver ends just before this `.`, with the `)` of a call.
        let Some(close) = prev_non_ws(c, dot) else {
            return false;
        };
        if c[close] != ')' {
            return false;
        }
        let Some(open) = matching_back(c, close, '(', ')') else {
            return false;
        };
        let Some((name, start)) = before_turbofish(c, open).and_then(|end| word_before(c, end))
        else {
            return false;
        };
        match prev_non_ws(c, start) {
            // An earlier call in the same chain.
            Some(p) if c[p] == '.' => dot = p,
            // The root: `Router::new()`.
            Some(p) if name == "new" && p >= 1 && c[p] == ':' && c[p - 1] == ':' => {
                return before_turbofish(c, p - 1)
                    .and_then(|end| word_before(c, end))
                    .is_some_and(|(ty, _)| ty == "Router");
            }
            _ => return false,
        }
    }
}

/// The handlers in the method-router argument of a `.route(`, which runs from
/// `from` to `to`: a chain of method routers (`get(h)`,
/// `axum::routing::post(h)`, then `.delete(h)`), with `.layer(..)` and
/// `.route_layer(..)` allowed after the first. Each method router's argument
/// that is a plain path (a trailing comma allowed) is a handler, as
/// `(start, end)`. A chain with anything else in it has none.
fn method_router_handlers(c: &[char], from: usize, to: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut k = skip_ws(c, from);
    let mut first = true;
    while k < to {
        if !first {
            if c[k] != '.' {
                return Vec::new();
            }
            k = skip_ws(c, k + 1);
        }
        let mut name = ident_at(c, k);
        let mut j = k + name.chars().count();
        while first && c.get(j) == Some(&':') && c.get(j + 1) == Some(&':') {
            let next = ident_at(c, j + 2);
            if next.is_empty() {
                break;
            }
            j += 2 + next.chars().count();
            name = next;
        }
        let paren = skip_ws(c, j);
        if name.is_empty() || c.get(paren) != Some(&'(') {
            return Vec::new();
        }
        let Some(end) = matching(c, paren, '(', ')').filter(|&e| e < to) else {
            return Vec::new();
        };
        if METHOD_ROUTERS.contains(&name.as_str()) {
            let a = skip_ws(c, paren + 1);
            let mut b = end;
            while b > a && c[b - 1].is_whitespace() {
                b -= 1;
            }
            if b > a && c[b - 1] == ',' {
                b -= 1;
                while b > a && c[b - 1].is_whitespace() {
                    b -= 1;
                }
            }
            if b > a && c[a..b].iter().all(|&x| is_ident(x) || x == ':') {
                out.push((a, b));
            }
        } else if first || !(name == "layer" || name == "route_layer") {
            return Vec::new();
        }
        k = skip_ws(c, end + 1);
        first = false;
    }
    out
}

/// Every route's handler in one file: the method routers' plain-path
/// arguments in each `.route("literal", method_router)` call of a route table,
/// meaning one of the two tables in `create_router` (when `file` is
/// `api/routes.rs`) or a method chain that starts at an inline `Router::new()`.
fn route_slots(file: &str, c: &[char]) -> Vec<RouteSlot> {
    let tables = if file == "api/routes.rs" {
        table_extents(c)
    } else {
        Vec::new()
    };
    let mut out = Vec::new();
    for (dot, name, open, close) in chained_calls(c, 0, c.len()) {
        if name != "route" {
            continue;
        }
        let table = if tables.iter().any(|&(s, e)| s <= dot && dot < e) {
            RouteTable::CreateRouter
        } else if chain_starts_at_router_new(c, dot) {
            RouteTable::InlineRouter
        } else {
            continue;
        };
        let args = split_args(c, open, close);
        let [(ps, pe), (ms, me)] = args[..] else {
            continue;
        };
        let path: String = c[ps..pe].iter().collect();
        let path = path.trim();
        if path.len() < 2 || !path.starts_with('"') || !path.ends_with('"') {
            continue;
        }
        out.extend(
            method_router_handlers(c, ms, me)
                .into_iter()
                .map(|(from, to)| RouteSlot { from, to, table }),
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Audit call sites
// ---------------------------------------------------------------------------

/// The two writers. `record` awaits the insert and logs a failure at `error`;
/// `log_admin_action` is the raw insert it wraps.
const WRITERS: [&str; 2] = ["audit_log::record", "log_admin_action"];

/// The macros that a call of a function that writes an audit row may sit in,
/// by name. An audit call site itself may sit in no macro: none does today.
/// - `assert`: std's `assert!` evaluates its condition in place, so it cannot
///   spawn or drop the call. The one such call today is a unit test in
///   `admin/cordis.rs`: `assert!(provide_cordis_service(..).await.is_err())`.
///
/// A `macro_rules!` or an `as` import that defines one of these names is a
/// defect, so the allowance cannot be borrowed by a macro of the same name.
const CALLER_MACROS: &[&str] = &["assert"];

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

/// The scan of a set of files: the audit call sites per file (files with none
/// left out), every defect, and every place a function that writes an audit
/// row is named as a route's handler, as `(file, line, name, table)`.
#[derive(Debug, Default)]
struct CrateScan {
    sites: BTreeMap<String, Vec<usize>>,
    defects: Vec<Defect>,
    handlers: std::collections::BTreeSet<(String, usize, String, RouteTable)>,
}

/// One source file, masked, with every extent the rules need.
struct Parsed {
    file: String,
    c: Vec<char>,
    spawns: Vec<(usize, usize, String)>,
    asyncs: Vec<(usize, usize, AsyncKind)>,
    macros: Vec<(usize, usize, String)>,
    fns: Vec<FnDef>,
    slots: Vec<RouteSlot>,
}

impl Parsed {
    fn new(file: &str, src: &str) -> Self {
        let c = mask(src);
        let spawns = spawn_extents(&c);
        let asyncs = async_extents(&c);
        let macros = macro_extents(&c);
        let fns = fn_defs(&c);
        let slots = route_slots(file, &c);
        Parsed {
            file: file.to_string(),
            c,
            spawns,
            asyncs,
            macros,
            fns,
            slots,
        }
    }

    fn defect(&self, out: &mut Vec<Defect>, pos: usize, reason: String) {
        out.push(Defect {
            file: self.file.clone(),
            line: line_of(&self.c, pos),
            reason,
        });
    }
}

/// The rules every checked call obeys: an audit call site, or a call of a
/// function that writes an audit row. `what` names the call in the message;
/// `pos` is where its callee's name starts and `close` is the `)` of its
/// argument list; `allowed_macros` are the macros it may sit in (none for an
/// audit call site). Returns the rules it breaks and the innermost function
/// whose body holds it.
fn check_call<'a>(
    p: &'a Parsed,
    what: &str,
    pos: usize,
    close: usize,
    allowed_macros: &[&str],
) -> (Vec<String>, Option<&'a FnDef>) {
    let c = &p.c;
    let mut reasons = Vec::new();

    // Rule 1: not inside the argument of any spawn form.
    if let Some((open, _, name)) = p.spawns.iter().find(|(o, cl, _)| *o < pos && pos < *cl) {
        reasons.push(format!(
            "{what} sits inside the argument of `{name}(` opened at line {}: the write is \
             detached from the response",
            line_of(c, *open)
        ));
    }

    // Rule 2: awaited where it is called.
    if !awaited_after(c, close) {
        reasons.push(format!(
            "{what} is not `.await`ed where it is called (handed to a helper, bound to a \
             variable, or dropped)"
        ));
    }

    // Rule 3: not inside an async block or an async closure.
    for (start, end, kind) in &p.asyncs {
        if *start < pos && pos < *end {
            let shape = match kind {
                AsyncKind::Block => "an `async` block",
                AsyncKind::Closure => "an `async` closure",
            };
            reasons.push(format!(
                "{what} sits inside {shape} (line {}): that future can be spawned or dropped \
                 away from the response; await the call in the handler body",
                line_of(c, *start)
            ));
        }
    }

    // Rule 4: not inside the arguments of any macro invocation.
    for (open, end, name) in &p.macros {
        if *open < pos && pos < *end && !allowed_macros.contains(&name.as_str()) {
            reasons.push(format!(
                "{what} sits inside the arguments of the macro `{name}!` opened at line {}: a \
                 macro can expand to a spawn or drop the future",
                line_of(c, *open)
            ));
        }
    }

    // Rule 5: in the body of a function that is not nested in another one.
    let encl = enclosing_fns(&p.fns, pos);
    match encl.as_slice() {
        [] => reasons.push(format!("{what} is not inside any function body")),
        [_] => {}
        [.., outer, inner] => reasons.push(format!(
            "{what} sits inside `fn {}`, which is nested in the body of `fn {}`: a nested \
             function can be spawned or detached by its caller",
            inner.name, outer.name
        )),
    }
    (reasons, encl.last().copied())
}

/// The route table in which `name`, starting at `pos`, is a route's handler
/// (the last segment of a [`RouteSlot`]'s plain path), if it is one. A
/// method router anywhere else (`x.post(name)`, `let m = post(name)`, a
/// `.route(` on another receiver) is no route's handler.
fn route_handler_at(p: &Parsed, pos: usize, name: &str) -> Option<RouteTable> {
    let end = pos + name.chars().count();
    p.slots
        .iter()
        .find(|s| s.from <= pos && s.to == end)
        .map(|s| s.table)
}

/// True when `pos` names an item in a `use` declaration without renaming it
/// (`use a::b::name;`, `use a::{name, other};`).
fn is_plain_use(c: &[char], pos: usize, name: &str) -> bool {
    let end = skip_ws(c, pos + name.chars().count());
    if !matches!(c.get(end), Some(',' | '}' | ';')) {
        return false;
    }
    // Walk back over the use tree, word by word, to the `use` keyword.
    let mut k = pos;
    loop {
        while k > 0 && (c[k - 1].is_whitespace() || matches!(c[k - 1], ':' | ',' | '{')) {
            k -= 1;
        }
        let e = k;
        while k > 0 && is_ident(c[k - 1]) {
            k -= 1;
        }
        if k == e {
            return false;
        }
        if c[k..e].iter().collect::<String>() == "use" {
            return true;
        }
    }
}

/// The audit writer imported where its calls could not be found by name:
/// `audit_log::*`, `audit_log::{.., record, ..}`, `audit_log as ..`.
fn writer_renames(c: &[char]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for pos in find_all(c, "audit_log") {
        let j = skip_ws(c, pos + "audit_log".len());
        if ident_at(c, j) == "as" {
            out.push((
                pos,
                "the `audit_log` module is imported under another name: its `record(` calls \
                 would not be found"
                    .to_string(),
            ));
            continue;
        }
        if !(c.get(j) == Some(&':') && c.get(j + 1) == Some(&':')) {
            continue;
        }
        let k = skip_ws(c, j + 2);
        let hidden = match c.get(k) {
            Some('*') => true,
            Some('{') => {
                let close = matching(c, k, '{', '}').unwrap_or(c.len());
                let inner: String = c[k + 1..close].iter().collect();
                inner.contains('*') || inner.split(|x: char| !is_ident(x)).any(|w| w == "record")
            }
            _ => false,
        };
        if hidden {
            out.push((
                pos,
                "`audit_log::record` is imported by a glob or a use list: its calls would not be \
                 found as `audit_log::record(`"
                    .to_string(),
            ));
        }
    }
    out
}

/// Record `f` as a function that writes an audit row (by name, with whether
/// one of that name takes `self`), and queue its callers for checking when it
/// is new or newly a method.
fn note_writer(f: Option<&FnDef>, writers: &mut BTreeMap<String, bool>, queue: &mut Vec<String>) {
    let Some(f) = f else {
        return;
    };
    match writers.get_mut(&f.name) {
        None => {
            writers.insert(f.name.clone(), f.is_method);
            queue.push(f.name.clone());
        }
        Some(method) if f.is_method && !*method => {
            *method = true;
            queue.push(f.name.clone());
        }
        Some(_) => {}
    }
}

fn scan_files(files: &[(String, String)]) -> CrateScan {
    let parsed: Vec<Parsed> = files.iter().map(|(f, s)| Parsed::new(f, s)).collect();
    let mut out = CrateScan::default();
    // Functions that write an audit row, directly or through a checked call.
    let mut writers_of_rows: BTreeMap<String, bool> = BTreeMap::new();
    let mut queue: Vec<String> = Vec::new();

    // Pass 1: the audit call sites.
    for p in &parsed {
        let c = &p.c;
        let mut sites: Vec<usize> = Vec::new();
        for writer in WRITERS {
            for pos in find_all(c, writer) {
                // A definition (`fn log_admin_action(`) is not a call site.
                if word_before(c, pos).is_some_and(|(w, _)| w == "fn") {
                    continue;
                }
                let after = skip_ws(c, pos + writer.chars().count());
                if c.get(after) != Some(&'(') {
                    p.defect(
                        &mut out.defects,
                        pos,
                        format!(
                            "`{writer}` is referenced without being called: it could be handed to \
                             a helper or spawned as a function value"
                        ),
                    );
                    continue;
                }
                sites.push(pos);
                let Some(close) = matching(c, after, '(', ')') else {
                    p.defect(
                        &mut out.defects,
                        pos,
                        format!("`{writer}(` has an unbalanced argument list"),
                    );
                    continue;
                };
                let (reasons, holder) = check_call(p, &format!("`{writer}(..)`"), pos, close, &[]);
                for reason in reasons {
                    p.defect(&mut out.defects, pos, reason);
                }
                note_writer(holder, &mut writers_of_rows, &mut queue);

                if writer == "log_admin_action" && awaited_after(c, close) {
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
                    let w = skip_ws(c, skip_ws(c, close + 1) + 1);
                    let after_await = skip_ws(c, w + 5);
                    let discarded_by_ok = c.get(after_await) == Some(&'.')
                        && matches!(
                            ident_at(c, skip_ws(c, after_await + 1)).as_str(),
                            "ok" | "unwrap_or" | "unwrap_or_default" | "unwrap_or_else"
                        );
                    if discarded_by_let || discarded_by_ok {
                        p.defect(
                            &mut out.defects,
                            pos,
                            "the Result of `log_admin_action(..)` is discarded".to_string(),
                        );
                    }
                }
            }
        }
        for (pos, reason) in writer_renames(c) {
            p.defect(&mut out.defects, pos, reason);
        }
        for name in CALLER_MACROS {
            for pos in find_all(c, name) {
                let mut k = pos;
                while k > 0 && c[k - 1].is_whitespace() {
                    k -= 1;
                }
                let by_rules = k > 0
                    && c[k - 1] == '!'
                    && word_before(c, k - 1).is_some_and(|(w, _)| w == "macro_rules");
                let by_as = word_before(c, pos).is_some_and(|(w, _)| w == "as");
                if by_rules || by_as {
                    p.defect(
                        &mut out.defects,
                        pos,
                        format!(
                            "`{name}` is defined here (`macro_rules!` or `as`): it would borrow \
                             the scan's allowance for calls inside `{name}!`"
                        ),
                    );
                }
            }
        }
        if !sites.is_empty() {
            sites.sort_unstable();
            out.sites.insert(p.file.clone(), sites);
        }
    }

    // Pass 2: every call of a function that writes an audit row obeys the same
    // rules, transitively; such a function is named without a call only as a
    // route's handler or in a `use` list.
    let mut done: std::collections::BTreeSet<(String, bool)> = std::collections::BTreeSet::new();
    while let Some(name) = queue.pop() {
        let method = writers_of_rows.get(&name).copied().unwrap_or(false);
        if !done.insert((name.clone(), method)) {
            continue;
        }
        let what = format!("`{name}(..)`, a function that writes an audit row,");
        for p in &parsed {
            let c = &p.c;
            for pos in find_all(c, &name) {
                // Its own definition.
                if word_before(c, pos).is_some_and(|(w, _)| w == "fn") {
                    continue;
                }
                let dotted = char_before(c, pos) == Some('.');
                if dotted && !method {
                    // A method of the same name: not this free function.
                    continue;
                }
                let mut after = skip_ws(c, pos + name.chars().count());
                if c.get(after) == Some(&':') && c.get(after + 1) == Some(&':') {
                    let lt = skip_ws(c, after + 2);
                    if c.get(lt) == Some(&'<') {
                        if let Some(gt) = matching(c, lt, '<', '>') {
                            after = skip_ws(c, gt + 1);
                        }
                    }
                }
                if c.get(after) == Some(&'(') {
                    let Some(close) = matching(c, after, '(', ')') else {
                        p.defect(
                            &mut out.defects,
                            pos,
                            format!("{what} has an unbalanced argument list"),
                        );
                        continue;
                    };
                    let (reasons, holder) = check_call(p, &what, pos, close, CALLER_MACROS);
                    for reason in reasons {
                        p.defect(&mut out.defects, pos, reason);
                    }
                    note_writer(holder, &mut writers_of_rows, &mut queue);
                    continue;
                }
                if dotted || is_plain_use(c, pos, &name) {
                    // A field of that name, or a plain import.
                    continue;
                }
                if let Some(table) = route_handler_at(p, pos, &name) {
                    // A route's handler, in a route table.
                    out.handlers
                        .insert((p.file.clone(), line_of(c, pos), name.clone(), table));
                    continue;
                }
                p.defect(
                    &mut out.defects,
                    pos,
                    format!(
                        "`{name}`, a function that writes an audit row, is referenced as a value: \
                         it could be spawned, stored or dropped away from the response"
                    ),
                );
            }
        }
    }
    // A name re-checked because it became a method reports its free calls twice.
    let mut seen = std::collections::BTreeSet::new();
    out.defects
        .retain(|d| seen.insert((d.file.clone(), d.line, d.reason.clone())));
    out
}

/// One file on its own (the scanner's fixtures).
fn scan(file: &str, src: &str) -> Scan {
    let r = scan_files(&[(file.to_string(), src.to_string())]);
    Scan {
        sites: r.sites.into_values().flatten().collect(),
        defects: r.defects,
    }
}

#[test]
fn audit_is_awaited_on_every_admin_write() {
    let result = scan_files(&all_sources());
    let per_file: BTreeMap<String, usize> = result
        .sites
        .iter()
        .map(|(f, s)| (f.clone(), s.len()))
        .collect();
    let total: usize = per_file.values().sum();
    let defects = result.defects;

    eprintln!(
        "audit call sites scanned: {total} in {} files",
        per_file.len()
    );
    for (f, k) in &per_file {
        eprintln!("  {k:3}  {f}");
    }
    let outside: Vec<&(String, usize, String, RouteTable)> = result
        .handlers
        .iter()
        .filter(|h| h.3 == RouteTable::InlineRouter)
        .collect();
    eprintln!(
        "functions that write an audit row, named as a route's handler: {} in create_router's \
         two tables, {} in inline `Router::new()` chains outside them:",
        result.handlers.len() - outside.len(),
        outside.len()
    );
    for (file, line, name, _) in &outside {
        eprintln!("  {file}:{line}  {name}");
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
         never inside a spawn, an async block or closure, a macro or a nested function, and so \
         must every call of a function that writes one",
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

/// The method routers the inventory reads; the first four are mutating.
const METHOD_ROUTERS: [&str; 8] = [
    "post", "put", "patch", "delete", "get", "head", "options", "trace",
];

/// The source text from `from` to `to`, with its whitespace collapsed.
fn text_of(orig: &[char], from: usize, to: usize) -> String {
    orig[from..to.min(orig.len())]
        .iter()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `(start, end)` of each top-level argument between the parentheses at
/// `open` and `close` (an empty trailing argument is dropped).
fn split_args(c: &[char], open: usize, close: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut s = open + 1;
    for (k, &ch) in c.iter().enumerate().take(close).skip(open + 1) {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push((s, k));
                s = k + 1;
            }
            _ => {}
        }
    }
    out.push((s, close));
    out.retain(|&(a, b)| c[a..b].iter().any(|x| !x.is_whitespace()));
    out
}

/// `(dot, name, open, close)` of every method call `.name(..)` (or
/// `.name::<..>(..)`) that starts between `from` and `to`, in order.
fn chained_calls(c: &[char], from: usize, to: usize) -> Vec<(usize, String, usize, usize)> {
    let mut out = Vec::new();
    for dot in from..to {
        if c[dot] != '.' {
            continue;
        }
        let at = skip_ws(c, dot + 1);
        let name = ident_at(c, at);
        if name.is_empty() || name.starts_with(|x: char| x.is_ascii_digit()) {
            continue;
        }
        let mut open = skip_ws(c, at + name.chars().count());
        if c.get(open) == Some(&':') && c.get(open + 1) == Some(&':') {
            let lt = skip_ws(c, open + 2);
            if c.get(lt) == Some(&'<') {
                if let Some(gt) = matching(c, lt, '<', '>') {
                    open = skip_ws(c, gt + 1);
                }
            }
        }
        if c.get(open) != Some(&'(') {
            // `.await`, a field.
            continue;
        }
        let close = matching(c, open, '(', ')').unwrap_or(c.len());
        out.push((dot, name, open, close));
    }
    out
}

/// One `let`: where its keyword starts, the extent of its pattern, where its
/// initializer starts (after the `=`, when it has one), where the statement
/// ends, and the extent in which its binding is in scope.
struct LetBinding {
    at: usize,
    pattern: (usize, usize),
    init: Option<usize>,
    end: usize,
    scope: (usize, usize),
}

/// `(open, close)` of the innermost `{ .. }` around `pos`.
fn enclosing_block(c: &[char], pos: usize) -> Option<(usize, usize)> {
    let mut depth = 0i32;
    for (k, &ch) in c[..pos].iter().enumerate().rev() {
        match ch {
            '}' => depth += 1,
            '{' => {
                if depth == 0 {
                    return matching(c, k, '{', '}').map(|close| (k, close));
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// The `let` whose keyword starts at `at`: `let PAT = INIT;`, `let PAT;`,
/// `if let PAT = EXPR { .. }` or `while let PAT = EXPR { .. }`. A plain
/// binding is in scope from the end of its statement to the end of its block;
/// an `if let` / `while let` binding in its body.
fn let_binding(c: &[char], at: usize) -> LetBinding {
    let n = c.len();
    let conditional = word_before(c, at).is_some_and(|(w, _)| w == "if" || w == "while");
    let block = enclosing_block(c, at).unwrap_or((0, n));
    // The pattern runs to the first `=` (not `==`, `=>`, `!=`, `<=`, `>=`) or
    // `;` outside brackets.
    let pat_start = skip_ws(c, at + 3);
    let mut depth = 0i32;
    let mut k = pat_start;
    let mut eq = None;
    while k < n {
        match c[k] {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            ';' if depth == 0 => break,
            '=' if depth == 0
                && !matches!(c.get(k + 1), Some('=' | '>'))
                && !matches!(c[k - 1], '=' | '!' | '<' | '>') =>
            {
                eq = Some(k);
                break;
            }
            _ => {}
        }
        k += 1;
    }
    let pattern = (pat_start, k);
    let Some(eq) = eq else {
        return LetBinding {
            at,
            pattern,
            init: None,
            end: k,
            scope: (k, block.1),
        };
    };
    // The statement ends at a `;` outside brackets; an `if let` / `while let`
    // at the `{` of its body.
    let mut depth = 0i32;
    let mut end = n;
    for (q, &ch) in c.iter().enumerate().skip(eq + 1) {
        match ch {
            '{' if conditional && depth == 0 => {
                end = q;
                break;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                if depth == 0 {
                    end = q;
                    break;
                }
                depth -= 1;
            }
            ';' if depth == 0 => {
                end = q;
                break;
            }
            _ => {}
        }
    }
    let scope = if conditional {
        (end, matching(c, end, '{', '}').unwrap_or(n))
    } else {
        (end, block.1)
    };
    LetBinding {
        at,
        pattern,
        init: Some(skip_ws(c, eq + 1)),
        end,
        scope,
    }
}

/// Why the router `name`, taken by the `.merge(` or `.nest(` whose `.` is at
/// `dot`, is one the inventory cannot see; `None` when it can see it: the
/// most recent binding of `name` before the call, in the same function and
/// in scope there, is `let [mut] name = Router::new()` written inside one of
/// the two tables (`tables`), whose routes this parse reads, and `name` is
/// not reassigned between that binding and the call.
fn unreadable_router(
    c: &[char],
    fns: &[FnDef],
    tables: &[(usize, usize)],
    dot: usize,
    name: &str,
) -> Option<String> {
    if name.is_empty()
        || name.starts_with(|x: char| x.is_ascii_digit())
        || !name.chars().all(is_ident)
    {
        return Some(
            "is not a name bound in the two tables: write its routes inline in a table, or teach \
             this parser to read it"
                .to_string(),
        );
    }
    let Some(f) = enclosing_fns(fns, dot).last().copied() else {
        return Some("is not inside a function".to_string());
    };
    let binding = find_all(&c[f.open..dot], "let")
        .into_iter()
        .map(|at| let_binding(c, f.open + at))
        .filter(|b| !find_all(&c[b.pattern.0..b.pattern.1], name).is_empty())
        .filter(|b| b.scope.0 < dot && dot < b.scope.1)
        .max_by_key(|b| b.at);
    let Some(b) = binding else {
        return Some(format!(
            "has no `let` binding in scope before it in `fn {}`",
            f.name
        ));
    };
    let line = line_of(c, b.at);
    let pattern: String = c[b.pattern.0..b.pattern.1].iter().collect();
    let pattern = pattern.trim();
    let pattern = pattern
        .strip_prefix("mut")
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .map_or(pattern, str::trim_start);
    let plain = pattern == name
        || pattern.strip_prefix(name).is_some_and(|rest| {
            let rest = rest.trim_start();
            rest.starts_with(':') && !rest.starts_with("::")
        });
    let inline = b
        .init
        .is_some_and(|i| c[i..].iter().take(13).collect::<String>() == "Router::new()");
    let in_table = tables.iter().any(|&(s, e)| s <= b.at && b.end <= e);
    if !(plain && inline && in_table) {
        return Some(format!(
            "is bound at line {line} (its most recent binding before the call, in `fn {}`) to \
             something other than `let {name} = Router::new()` inside the two tables",
            f.name
        ));
    }
    for p in find_all(&c[b.end..dot], name)
        .into_iter()
        .map(|p| b.end + p)
    {
        let after = skip_ws(c, p + name.chars().count());
        if char_before(c, p) != Some('.')
            && c.get(after) == Some(&'=')
            && !matches!(c.get(after + 1), Some('=' | '>'))
        {
            return Some(format!(
                "is reassigned at line {} after its binding at line {line}",
                line_of(c, p)
            ));
        }
    }
    None
}

/// The mutating registrations of one `.route(path, method_router)` call whose
/// argument list runs from `open` to `close`. Anything but a string-literal
/// path and a chain of known method routers, each with a plain-path handler,
/// panics: the inventory fails closed.
fn parse_route(c: &[char], orig: &[char], open: usize, close: usize) -> Vec<MutatingRoute> {
    let whole = text_of(orig, open + 1, close);
    let args = split_args(c, open, close);
    let [(ps, pe), (ms, me)] = args[..] else {
        panic!("routes.rs: `.route({whole})` is not a path and a method router");
    };
    let lit = text_of(orig, ps, pe);
    let Some(path) = lit
        .strip_prefix('"')
        .and_then(|l| l.strip_suffix('"'))
        .filter(|p| !p.contains('"'))
    else {
        panic!("routes.rs: the path of `.route({whole})` is not a string literal");
    };

    let mut out = Vec::new();
    let mut k = skip_ws(c, ms);
    let mut first = true;
    while k < me {
        if !first {
            if c[k] != '.' {
                panic!(
                    "routes.rs: route `{path}`: `{}` in its method router is not a call the \
                     inventory reads",
                    text_of(orig, k, me)
                );
            }
            k = skip_ws(c, k + 1);
        }
        // The callee: a name, or for the first call a path (`axum::routing::post`).
        let mut name = ident_at(c, k);
        let mut j = k + name.chars().count();
        while first && c.get(j) == Some(&':') && c.get(j + 1) == Some(&':') {
            let next = ident_at(c, j + 2);
            if next.is_empty() {
                break;
            }
            j += 2 + next.chars().count();
            name = next;
        }
        let paren = skip_ws(c, j);
        if name.is_empty() || c.get(paren) != Some(&'(') {
            panic!(
                "routes.rs: route `{path}`: the method router `{}` is not a call the inventory \
                 reads",
                text_of(orig, ms, me)
            );
        }
        let end = matching(c, paren, '(', ')')
            .filter(|&e| e < me)
            .unwrap_or_else(|| panic!("routes.rs: route `{path}`: unbalanced `{name}(`"));
        if METHOD_ROUTERS.contains(&name.as_str()) {
            let arg = text_of(orig, paren + 1, end);
            let arg = arg.trim_end_matches(',').trim();
            if arg.is_empty() || !arg.chars().all(|ch| is_ident(ch) || ch == ':') {
                panic!(
                    "routes.rs: route `{} {path}`: the handler argument `{arg}` is not a plain \
                     path, so the inventory cannot check that its handler audits",
                    name.to_uppercase()
                );
            }
            if METHOD_ROUTERS[..4].contains(&name.as_str()) {
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
                // `create_router` is nested under `/api`; the v1 table under `/api/v1`.
                let full = match module {
                    "v1" => format!("/api/v1{path}"),
                    _ => format!("/api{path}"),
                };
                out.push(MutatingRoute {
                    method: name.to_uppercase(),
                    path: full,
                    module,
                    name: segments.last().expect("non-empty").to_string(),
                });
            }
        } else if !first && (name == "layer" || name == "route_layer") {
            // Middleware on this one route: no handler in it.
        } else {
            panic!(
                "routes.rs: route `{path}`: unrecognised method router `{name}(` (the inventory \
                 reads {METHOD_ROUTERS:?}, each with a plain-path handler)"
            );
        }
        k = skip_ws(c, end + 1);
        first = false;
    }
    if first {
        panic!("routes.rs: route `{path}` has no method router");
    }
    out
}

/// Every POST / PUT / PATCH / DELETE registration in `create_router`'s admin
/// route table and its `/v1` route table, read fail-closed: a shape the
/// parser does not know panics with the route and its text instead of being
/// skipped, so a registration can never go uncounted.
fn mutating_routes(routes_src: &str) -> Vec<MutatingRoute> {
    let orig: Vec<char> = routes_src.chars().collect();
    let c = mask(routes_src);
    let segments: Vec<(&str, usize, usize)> = TABLES
        .iter()
        .map(|&(start_marker, end_marker)| {
            let start = find_seq(&c, start_marker, 0)
                .unwrap_or_else(|| panic!("routes.rs: start marker `{start_marker}` not found"));
            let end = find_seq(&c, end_marker, start)
                .unwrap_or_else(|| panic!("routes.rs: end marker `{end_marker}` not found"));
            (start_marker, start, end)
        })
        .collect();
    // A `.merge(` or `.nest(` may only take a router whose binding is written
    // inline in the tables, whose routes this parse reads anyway.
    let extents: Vec<(usize, usize)> = segments.iter().map(|&(_, s, e)| (s, e)).collect();
    let fns = fn_defs(&c);

    let mut out = Vec::new();
    for &(table, start, end) in &segments {
        // Argument lists read already: `.route(` (parsed) and `.layer(` /
        // `.route_layer(` (middleware).
        let mut covered: Vec<(usize, usize)> = Vec::new();
        for (dot, name, open, close) in chained_calls(&c, start, end) {
            if covered.iter().any(|&(s, e)| s < dot && dot < e) {
                continue;
            }
            match name.as_str() {
                "route" => {
                    out.extend(parse_route(&c, &orig, open, close));
                    covered.push((open, close));
                }
                "layer" | "route_layer" => covered.push((open, close)),
                "merge" | "nest" => {
                    let router = split_args(&c, open, close)
                        .last()
                        .map(|&(s, e)| text_of(&orig, s, e))
                        .unwrap_or_default();
                    if let Some(why) = unreadable_router(&c, &fns, &extents, dot, &router) {
                        panic!(
                            "routes.rs: `.{name}({})` in the table at `{table}` takes a router \
                             the inventory cannot see: `{router}` {why}",
                            text_of(&orig, open + 1, close)
                        );
                    }
                }
                other => panic!(
                    "routes.rs: unrecognised router call `.{other}({})` in the table at `{table}`",
                    text_of(&orig, open + 1, close)
                ),
            }
        }
        // A method router built anywhere but inside a `.route(`.
        for at in start..end {
            if !is_ident(c[at]) || (at > 0 && is_ident(c[at - 1])) {
                continue;
            }
            let name = ident_at(&c, at);
            if !METHOD_ROUTERS.contains(&name.as_str()) || char_before(&c, at) == Some('.') {
                continue;
            }
            if c.get(skip_ws(&c, at + name.len())) != Some(&'(') {
                continue;
            }
            if covered.iter().any(|&(s, e)| s < at && at < e) {
                continue;
            }
            panic!(
                "routes.rs: `{name}(..)` at line {} in the table at `{table}` builds a method \
                 router outside a `.route(`: the inventory cannot place it",
                line_of(&c, at)
            );
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
fn scanner_allows_a_caller_in_assert_only_and_no_site_in_any_macro() {
    let helper = format!("async fn audit_now(pool: &PgPool) {{\n    {CALL}.await;\n}}\n");
    // A caller inside `assert!` (the cordis unit test's shape) passes.
    let s = scan(
        "fixture.rs",
        &format!(
            "{helper}async fn t(pool: PgPool) {{\n    assert!(audit_now(&pool).await == ());\n}}\n"
        ),
    );
    assert!(s.defects.is_empty(), "{:?}", s.defects);
    // An audit call site inside `assert!` does not.
    let s = fixture(&format!("    assert!({CALL}.await == ());"));
    assert!(
        s.defects.iter().any(|d| d
            .reason
            .contains("inside the arguments of the macro `assert!`")),
        "{:?}",
        s.defects
    );
    // Nor does a caller inside a macro that borrows the name.
    for shadow in [
        "macro_rules! assert {\n    ($($t:tt)*) => { tokio::spawn(async move { $($t)* }) };\n}\n",
        "use crate::detach as assert;\n",
    ] {
        let s = scan(
            "fixture.rs",
            &format!("{shadow}{helper}async fn t(pool: PgPool) {{\n    assert!(audit_now(&pool).await);\n}}\n"),
        );
        assert!(
            s.defects
                .iter()
                .any(|d| d.reason.contains("borrow the scan's allowance")),
            "{shadow}: {:?}",
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

// ---------------------------------------------------------------------------
// 1.16-FIX-3, under SR's bar for the scanner (it catches every audited route
// written the normal way): gate round 3's two shapes. A function that writes
// an audit row is named as a value only as a route's handler, inside a
// `.route(` of a route table; a `.merge(NAME)` or `.nest(.., NAME)` in the
// tables is read through NAME's binding, not its name. Each first case below
// is the reviewer's own sabotage, in shape.
// ---------------------------------------------------------------------------

#[test]
fn scanner_flags_a_writer_passed_as_a_value_outside_a_route_registration() {
    let writer = format!(
        "async fn write_patch_audit(a: PatchAudit) {{\n    let pool = a.pool;\n    {CALL}.await;\n}}\n"
    );
    for (label, src) in [
        // Gate round 3, rev2 finding 1: the writer handed to a spawning method named `post`.
        (
            "a spawning method named `post` on another type",
            format!(
                "struct PatchAudit {{\n    pool: PgPool,\n}}\n\
                 impl PatchAudit {{\n    fn post<F, Fut>(self, f: F)\n    where\n        \
                 F: FnOnce(PatchAudit) -> Fut,\n        \
                 Fut: std::future::Future<Output = ()> + Send + 'static,\n    {{\n        \
                 tokio::spawn(f(self));\n    }}\n}}\n\
                 {writer}pub async fn patch_cordis_entry(pool: PgPool) {{\n    \
                 PatchAudit {{ pool }}.post(write_patch_audit);\n}}\n"
            ),
        ),
        (
            "a method router built outside `.route(`",
            format!(
                "{writer}pub fn routes() -> Router {{\n    let mr = post(write_patch_audit);\n    \
                 Router::new().route(\"/x\", mr)\n}}\n"
            ),
        ),
        (
            "`.route(` on a receiver that is not a route table",
            format!(
                "{writer}pub async fn handler(auditor: Auditor) {{\n    \
                 auditor.route(\"/x\", post(write_patch_audit));\n}}\n"
            ),
        ),
        (
            "`.post(` of another type inside a `.route(`",
            format!(
                "{writer}pub fn routes() -> Router {{\n    \
                 Router::new().route(\"/x\", Spawner.post(write_patch_audit))\n}}\n"
            ),
        ),
    ] {
        let s = scan("fixture.rs", &src);
        assert!(
            s.defects.iter().any(|d| {
                d.reason.contains(
                "`write_patch_audit`, a function that writes an audit row, is referenced as a value"
            )
            }),
            "{label}: {:?}",
            s.defects
        );
    }
}

#[test]
fn scanner_lets_a_writer_through_as_a_handler_in_a_route_table() {
    // A handler module's own inline router (the Cordis RouteSet shape), with a
    // trailing comma after the path ...
    let handlers = format!(
        "pub async fn revoke_api_key(pool: PgPool) {{\n    {CALL}.await;\n}}\n\
         pub async fn rotate_api_key(pool: PgPool) {{\n    {CALL}.await;\n}}\n\
         pub fn routes() -> axum::Router {{\n    axum::Router::new()\n        \
         .route(\"/api-keys/{{id}}/rotate\", post(rotate_api_key))\n        \
         .route(\n            \"/api-keys/{{id}}\",\n            \
         get(list_api_keys).delete(\n                revoke_api_key,\n            ),\n        )\n}}\n"
    );
    // ... and `create_router`'s tables, including a registration on
    // `v1_routes` (not a `Router::new()` chain) inside the v1 table.
    let routes = ROUTES_FIXTURE.replace(
        "    let v1_routes = v1_routes.layer(",
        "    let v1_routes = v1_routes.route(\n        \"/api-keys/{id}/rotate\",\n        \
         post(crate::api::handlers::v1::rotate_api_key),\n    );\n    \
         let v1_routes = v1_routes.layer(",
    );
    let r = scan_files(&[
        ("api/handlers/v1/agents.rs".to_string(), handlers),
        ("api/routes.rs".to_string(), routes),
    ]);
    assert!(r.defects.is_empty(), "{:?}", r.defects);
    assert_eq!(r.sites.values().map(Vec::len).sum::<usize>(), 2);
}

/// The message of the panic `f` raises (it must raise one).
fn panic_message(f: impl FnOnce() + std::panic::UnwindSafe) -> String {
    let payload = std::panic::catch_unwind(f).expect_err("expected the inventory to panic");
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

fn assert_merge_refused(src: &str, name: &str) {
    let src = src.to_string();
    let msg = panic_message(move || {
        mutating_routes(&src);
    });
    let quoted = format!("`{name}`");
    for part in [
        "routes.rs",
        "a router the inventory cannot see",
        quoted.as_str(),
    ] {
        assert!(msg.contains(part), "missing `{part}` in: {msg}");
    }
}

#[test]
fn route_inventory_fails_closed_on_a_merge_of_a_router_bound_outside_the_tables() {
    // Gate round 3, rev2 finding 2: a router with an unaudited closure route,
    // bound under the admin table's own name before the table, then merged as
    // the table's first call (where `admin_routes` still names that router).
    let shadow = r#"    let admin_routes = Router::<Arc<Context>>::new().route(
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
    );
"#;
    let src = routes_with("").replace(
        "    let admin_routes = Router::new()\n",
        &format!("{shadow}    let admin_routes = Router::new()\n        .merge(admin_routes)\n"),
    );
    assert_merge_refused(&src, "admin_routes");
}

#[test]
fn route_inventory_fails_closed_on_a_merge_of_a_rebound_router() {
    // The table's inline router, bound again before the merge to a router the
    // inventory cannot read.
    let src = routes_with("").replace(
        "    let v1_routes = Router::new()\n",
        "    let v1_metered_routes = crate::api::handlers::v1::chat::routes();\n    \
         let v1_routes = Router::new()\n",
    );
    assert_merge_refused(&src, "v1_metered_routes");
}

#[test]
fn route_inventory_fails_closed_on_a_merge_of_a_reassigned_router() {
    // An inline router in the table, reassigned before the merge.
    let src = routes_with("").replace(
        "    let v1_routes = Router::new()\n        .merge(v1_metered_routes)\n",
        "    let mut v1_extra_routes = Router::new()\n        \
         .route(\"/usage\", get(crate::api::handlers::v1::get_usage));\n    \
         v1_extra_routes = crate::api::handlers::v1::chat::routes();\n    \
         let v1_routes = Router::new()\n        .merge(v1_metered_routes)\n        \
         .merge(v1_extra_routes)\n",
    );
    assert_merge_refused(&src, "v1_extra_routes");
}
