//! Entry composition: `@include` splice, `@group` flatten, and Rhai config
//! interpolation.
//!
//! These transforms run BEFORE the loader sees entries: `Loader` keeps
//! consuming plain [`Entry`] trees, while callers that want composition call
//! [`compose_all`] on the freshly parsed vector first.
//!
//! # Reserved plugin sentinels
//!
//! Two `plugin` values are reserved for composition and must never be
//! registered as factories:
//!
//! - [`INCLUDE_PLUGIN`] (`"@include"`): `config.path` names another entries
//!   TOML file (`{entry = [...]}` schema, like `config/cordis-entries.toml`);
//!   its entries are spliced in place at the include position, recursively.
//!   Relative paths resolve against `base_dir` (the directory of the
//!   top-level file). Missing files and include cycles (canonicalized-path
//!   revisit — a diamond that pulls one file twice also errors) abort
//!   composition with an error naming the path.
//! - [`GROUP_PLUGIN`] (`"@group"`): `config.entries` carries nested entry
//!   objects flattened into the parent vector in place. Child ids must be
//!   unique within the group; children may nest more `@group` entries, while
//!   an `@include` child defers to the outer resolver (same `base_dir`).
//!
//! # Config interpolation (feature `rhai`)
//!
//! Whole-value interpolation, primer `!!js` analogue: any STRING inside an
//! entry's `config` — value or key, any depth — matching `${rhai: <expr>}`
//! is evaluated by a sandboxed Engine (same limits as `RhaiService`, only
//! the `log` helper; no provide helpers) with `entry` bound to an object map
//! of that entry MINUS its `config` (`id`, `plugin`, `disabled`, `isolate`,
//! `intercept`). The scope variable is named `entry`, not `$entry`: rhai
//! 1.25 reserves `$` in the parser (`ErrorParsing(Reserved("$"))`, verified
//! empirically). Strings splice verbatim; any other result renders as
//! compact JSON; a `()` result is an error. Rendered text is never
//! re-scanned, so results cannot inject new placeholders. With the `rhai`
//! feature OFF the module still compiles and interpolation is a no-op —
//! `${rhai: …}` strings pass through untouched and composition is purely
//! structural.

use crate::loader::Entry;

/// Reserved `plugin` sentinel: splice entries from another TOML file.
pub const INCLUDE_PLUGIN: &str = "@include";
/// Reserved `plugin` sentinel: flatten inline child entries in place.
pub const GROUP_PLUGIN: &str = "@group";

/// Marker opening a Rhai interpolation placeholder inside a config string.
const INTERP_OPEN: &str = "${rhai:";

/// TOML wrapper for included files — same `[[entry]]` schema as the loader.
#[derive(Debug, serde::Deserialize)]
struct IncludedEntries {
    #[serde(default)]
    entry: Vec<Entry>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Run every composition pass over `entries`: `@include` resolution, `@group`
/// flattening, then per-entry Rhai interpolation of `config` (pass-through
/// when the `rhai` feature is off).
///
/// `base_dir` is the directory relative to which `@include` paths resolve
/// (for a file loaded from disk, its parent directory).
pub fn compose_all(entries: &mut Vec<Entry>, base_dir: &std::path::Path) -> Result<(), String> {
    resolve_includes(entries, base_dir)?;
    compose_entries(entries)
}

/// Interpolate `${rhai: …}` placeholders in every entry's `config` against
/// that entry's own metadata.
#[cfg(feature = "rhai")]
pub fn compose_entries(entries: &mut [Entry]) -> Result<(), String> {
    for slot in entries.iter_mut() {
        let id = slot.id.clone();
        let meta = entry_meta_json(slot)?;
        interpolate_config(&mut slot.config, &meta).map_err(|e| format!("entry {id}: {e}"))?;
    }
    Ok(())
}

/// Pass-through variant used when the `rhai` feature is disabled: config
/// strings are left untouched.
#[cfg(not(feature = "rhai"))]
pub fn compose_entries(_entries: &mut [Entry]) -> Result<(), String> {
    Ok(())
}

/// Resolve `@include` entries by loading the referenced TOML files and
/// splicing their entries IN PLACE at the include position, recursively.
/// `@group` entries are flattened in place too; see the module docs.
pub fn resolve_includes(
    entries: &mut Vec<Entry>,
    base_dir: &std::path::Path,
) -> Result<(), String> {
    let mut visited = std::collections::HashSet::new();
    resolve_walk(entries, base_dir, &mut visited)
}

/// Single left-to-right pass over the vector. Sentinel slots are rewritten in
/// place and re-scanned (their first replacement may itself be a sentinel),
/// so ordering is preserved and nesting terminates naturally.
fn resolve_walk(
    entries: &mut Vec<Entry>,
    base_dir: &std::path::Path,
    visited: &mut std::collections::HashSet<std::path::PathBuf>,
) -> Result<(), String> {
    let mut i = 0;
    while i < entries.len() {
        match entries[i].plugin.as_str() {
            INCLUDE_PLUGIN => {
                let id = entries[i].id.clone();
                let path_str = include_path_arg(&entries[i].config, &id)?;
                let resolved = base_dir.join(&path_str);
                let canon = resolved.canonicalize().map_err(|e| {
                    format!("@include {id}: cannot resolve {}: {e}", resolved.display())
                })?;
                if !visited.insert(canon.clone()) {
                    return Err(format!(
                        "@include {id}: include cycle detected at {}",
                        canon.display()
                    ));
                }
                let text = std::fs::read_to_string(&canon)
                    .map_err(|e| format!("@include {id}: cannot read {}: {e}", canon.display()))?;
                let parsed: IncludedEntries = toml::from_str(&text).map_err(|e| {
                    format!(
                        "@include {id}: invalid entries TOML in {}: {e}",
                        canon.display()
                    )
                })?;
                entries.splice(i..i + 1, parsed.entry);
            }
            GROUP_PLUGIN => {
                let entry = std::mem::take(&mut entries[i]);
                let id = entry.id.clone();
                let children = group_children(&entry.config, &id)?;
                let mut expanded = Vec::new();
                expand_group_children(children, &mut expanded, &id)?;
                ensure_unique_ids(&expanded, &id)?;
                entries.splice(i..i + 1, expanded);
            }
            _ => i += 1,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// @include / @group helpers
// ---------------------------------------------------------------------------

/// Extract the required string `path` from an `@include` entry's config.
fn include_path_arg(config: &serde_json::Value, id: &str) -> Result<String, String> {
    let Some(obj) = config.as_object() else {
        return Err(format!(
            "@include {id}: config must be an object with a \"path\" key"
        ));
    };
    match obj.get("path") {
        Some(serde_json::Value::String(p)) => Ok(p.clone()),
        Some(other) => Err(format!(
            "@include {id}: \"path\" must be a string, got {other}"
        )),
        None => Err(format!(
            "@include {id}: missing required \"path\" in config"
        )),
    }
}

/// Deserialize the required `entries` array of an `@group` entry's config.
fn group_children(config: &serde_json::Value, id: &str) -> Result<Vec<Entry>, String> {
    let Some(obj) = config.as_object() else {
        return Err(format!(
            "@group {id}: config must be an object with an \"entries\" array"
        ));
    };
    let Some(children) = obj.get("entries") else {
        return Err(format!(
            "@group {id}: missing required \"entries\" array in config"
        ));
    };
    let Some(arr) = children.as_array() else {
        return Err(format!("@group {id}: \"entries\" must be an array"));
    };
    arr.iter()
        .map(|v| {
            serde_json::from_value(v.clone())
                .map_err(|e| format!("@group {id}: invalid child entry: {e}"))
        })
        .collect()
}

/// Recursively flatten `@group` children. Nested `@group`s expand here;
/// `@include` children pass through untouched so the outer resolver (which
/// owns `base_dir` and the cycle-guard set) splices their files in place.
fn expand_group_children(
    children: Vec<Entry>,
    out: &mut Vec<Entry>,
    _group_id: &str,
) -> Result<(), String> {
    for child in children {
        if child.plugin == GROUP_PLUGIN {
            let sub_id = child.id.clone();
            let sub = group_children(&child.config, &sub_id)?;
            let mut nested = Vec::new();
            expand_group_children(sub, &mut nested, &sub_id)?;
            ensure_unique_ids(&nested, &sub_id)?;
            out.extend(nested);
        } else {
            out.push(child);
        }
    }
    Ok(())
}

/// Fail when two flattened children share an id — ids must be unique within
/// a group.
fn ensure_unique_ids(entries: &[Entry], owner: &str) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for e in entries {
        if !seen.insert(e.id.as_str()) {
            return Err(format!(
                "@group {owner}: duplicate child entry id '{}'",
                e.id
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Rhai config interpolation
// ---------------------------------------------------------------------------

/// Replace every `${rhai: <expr>}` string in `config` (values and keys, any
/// depth) with the evaluation of `<expr>` against `entry_meta` exposed to
/// scripts as the `entry` object map.
///
/// Errors name the offending JSON key path (`$.outer.inner`). Non-string
/// values are never touched, and mixed plain/interpolated objects are fine.
#[cfg(feature = "rhai")]
pub fn interpolate_config(
    config: &mut serde_json::Value,
    entry_meta: &serde_json::Value,
) -> Result<(), String> {
    let engine = build_compose_engine();
    let mut base_scope = rhai::Scope::new();
    base_scope.push("entry", crate::rhai_service::json_to_dynamic(entry_meta));
    let mut path = String::from("$");
    interp_walk(config, &mut path, &engine, &base_scope)
}

/// Feature-off pass-through: `${rhai: …}` strings survive untouched.
#[cfg(not(feature = "rhai"))]
pub fn interpolate_config(
    _config: &mut serde_json::Value,
    _entry_meta: &serde_json::Value,
) -> Result<(), String> {
    Ok(())
}

/// Sandboxed engine for config interpolation: the same limits as
/// `RhaiService::build_engine` plus only the `log` helper — deliberately no
/// provide helpers, no io/fs access.
#[cfg(feature = "rhai")]
fn build_compose_engine() -> rhai::Engine {
    let mut engine = rhai::Engine::new();
    engine.set_max_operations(50_000);
    engine.set_max_string_size(8192);
    engine.set_max_call_levels(64);
    engine.set_max_expr_depths(128, 64);
    engine.register_fn("log", |msg: String| {
        tracing::info!("{}", msg);
    });
    engine
}

/// JSON view of an entry WITHOUT its `config` — the object interpolation
/// scripts see as `entry`. Intercept keys are emitted sorted so the scope is
/// deterministic across runs.
#[cfg(feature = "rhai")]
fn entry_meta_json(entry: &Entry) -> Result<serde_json::Value, String> {
    let mut keys: Vec<&String> = entry.intercept.keys().collect();
    keys.sort();
    let mut intercept = serde_json::Map::new();
    for k in keys {
        intercept.insert((*k).clone(), entry.intercept[k].clone());
    }
    let mut obj = serde_json::Map::new();
    obj.insert(
        "id".to_string(),
        serde_json::Value::String(entry.id.clone()),
    );
    obj.insert(
        "plugin".to_string(),
        serde_json::Value::String(entry.plugin.clone()),
    );
    obj.insert(
        "disabled".to_string(),
        serde_json::Value::Bool(entry.disabled),
    );
    if let Some(isolate) = &entry.isolate {
        obj.insert(
            "isolate".to_string(),
            serde_json::Value::String(isolate.clone()),
        );
    }
    obj.insert(
        "intercept".to_string(),
        serde_json::Value::Object(intercept),
    );
    Ok(serde_json::Value::Object(obj))
}

/// Recursive walker: strings may hold placeholders, arrays and objects
/// recurse, scalars pass untouched. `path` accumulates the JSON location
/// (`$.a[0].b`) for error messages.
#[cfg(feature = "rhai")]
fn interp_walk(
    value: &mut serde_json::Value,
    path: &mut String,
    engine: &rhai::Engine,
    base_scope: &rhai::Scope,
) -> Result<(), String> {
    match value {
        serde_json::Value::String(_) => {
            if let serde_json::Value::String(s) = value {
                if s.contains(INTERP_OPEN) {
                    let rendered = eval_placeholder_string(s, path, engine, base_scope)?;
                    // A placeholder spanning the WHOLE string yields the typed
                    // expression value (numbers stay numbers); embedded
                    // placeholders splice into the surrounding text.
                    *value = rendered;
                }
            }
            Ok(())
        }
        serde_json::Value::Array(items) => {
            for (i, item) in items.iter_mut().enumerate() {
                let head = path.len();
                path.push_str(&format!("[{i}]"));
                interp_walk(item, path, engine, base_scope)?;
                path.truncate(head);
            }
            Ok(())
        }
        serde_json::Value::Object(map) => {
            // Keys cloned up front: interpolation may rename a key.
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                let head = path.len();
                let mut val = match map.remove(&key) {
                    Some(v) => v,
                    None => continue,
                };
                path.push('.');
                path.push_str(&key);
                let display_key = if key.contains(INTERP_OPEN) {
                    match eval_placeholder_string(&key, path, engine, base_scope)? {
                        serde_json::Value::String(k) => k,
                        other => {
                            return Err(format!(
                                "{path}: interpolated object keys must yield a string, got {other}"
                            ))
                        }
                    }
                } else {
                    key
                };
                path.truncate(head + 1);
                path.push_str(&display_key);
                interp_walk(&mut val, path, engine, base_scope)?;
                path.truncate(head);
                map.insert(display_key, val);
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Evaluate every `${rhai: …}` placeholder in `s` and produce the resulting
/// JSON value. A placeholder spanning the ENTIRE string yields the typed
/// expression value directly (`"${rhai: 40 + 2}"` becomes the number 42);
/// otherwise results are spliced into the surrounding text (strings
/// verbatim, other values as compact JSON). Brace depth tracking skips
/// `#{…}` map literals and braces inside string literals, so expressions
/// may themselves contain `}` characters.
#[cfg(feature = "rhai")]
fn eval_placeholder_string(
    s: &str,
    path: &str,
    engine: &rhai::Engine,
    base_scope: &rhai::Scope,
) -> Result<serde_json::Value, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(rel) = rest.find(INTERP_OPEN) else {
            out.push_str(rest);
            return Ok(serde_json::Value::String(out));
        };
        let body_start = rel + INTERP_OPEN.len();
        let Some(close_rel) = placeholder_close(&rest[body_start..]) else {
            return Err(format!("{path}: unterminated ${{rhai: …}} placeholder"));
        };
        let expr = rest[body_start..body_start + close_rel].trim();
        let value = eval_expr(expr, path, engine, base_scope)?;
        if rel == 0 && body_start + close_rel + 1 == rest.len() {
            return Ok(value);
        }
        out.push_str(&rest[..rel]);
        out.push_str(&render_json_text(&value));
        rest = &rest[body_start + close_rel + 1..];
    }
}

/// Byte offset (relative to `body`) of the `}` closing a placeholder whose
/// expression starts right at `body`. Tracks brace depth so `#{…}` map
/// literals nest correctly, and skips braces inside `"string literals"`
/// (backslash escapes honored).
#[cfg(feature = "rhai")]
fn placeholder_close(body: &str) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut depth = 1usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else {
            match b {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Evaluate one interpolation expression and render it to string form:
/// strings verbatim, other JSON values as compact JSON text, `()` is an
/// error (an interpolated value is required, unlike listener pass-through).
///
/// The expression is wrapped in a synthetic function and compiled once, so
/// any rhai expression works (arithmetic, string concat, if-else, `#{…}`
/// map literals) while staying inside the sandboxed limits.
/// Render an interpolation result as TEXT for embedding inside a larger
/// string: strings verbatim, any other JSON value as compact JSON.
#[cfg(feature = "rhai")]
fn render_json_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Evaluate one interpolation expression to its JSON value. Strings stay
/// strings; other dynamics convert through the existing bridge; `()` is an
/// error (an interpolated value is required, unlike listener pass-through).
///
/// The expression is wrapped in a synthetic function and compiled once, so
/// any rhai expression works (arithmetic, string concat, if-else, `#{…}`
/// map literals) while staying inside the sandboxed limits.
#[cfg(feature = "rhai")]
fn eval_expr(
    expr: &str,
    path: &str,
    engine: &rhai::Engine,
    base_scope: &rhai::Scope,
) -> Result<serde_json::Value, String> {
    let wrapped = format!("fn __cordis_interp__() {{ ({expr}) }}");
    let ast = engine
        .compile(&wrapped)
        .map_err(|e| format!("{path}: invalid rhai expression {expr:?}: {e}"))?;
    let mut scope = base_scope.clone();
    let result: rhai::Dynamic = engine
        .call_fn(&mut scope, &ast, "__cordis_interp__", ())
        .map_err(|e| format!("{path}: rhai evaluation failed for {expr:?}: {e}"))?;
    crate::rhai_service::dynamic_to_json(&result).ok_or_else(|| {
        format!("{path}: rhai expression {expr:?} evaluated to (); a value is required")
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(id: &str, plugin: &str, config: serde_json::Value) -> Entry {
        Entry {
            id: id.to_string(),
            plugin: plugin.to_string(),
            config,
            ..Entry::default()
        }
    }

    /// Write a fixture entries file into `dir` and return its path.
    fn write_entries(dir: &std::path::Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).expect("write fixture");
    }

    fn ids(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.id.as_str()).collect()
    }

    // -- @include -----------------------------------------------------------

    #[test]
    fn include_splices_in_place_preserving_order() {
        let dir = tempfile::tempdir().unwrap();
        write_entries(
            dir.path(),
            "common.toml",
            "[[entry]]\nid = \"inc1\"\nplugin = \"Echo\"\n\n[[entry]]\nid = \"inc2\"\nplugin = \"Echo\"\n",
        );
        let mut entries = vec![
            entry("before", "Echo", json!({})),
            entry("pull", INCLUDE_PLUGIN, json!({"path": "common.toml"})),
            entry("after", "Echo", json!({})),
        ];
        resolve_includes(&mut entries, dir.path()).unwrap();
        assert_eq!(ids(&entries), vec!["before", "inc1", "inc2", "after"]);
        assert_eq!(entries[1].plugin, "Echo");
    }

    #[test]
    fn include_chains_resolve_recursively() {
        let dir = tempfile::tempdir().unwrap();
        write_entries(
            dir.path(),
            "b.toml",
            "[[entry]]\nid = \"from-b\"\nplugin = \"Echo\"\n\n[[entry]]\nid = \"pull-c\"\nplugin = \"@include\"\n[entry.config]\npath = \"c.toml\"\n",
        );
        write_entries(
            dir.path(),
            "c.toml",
            "[[entry]]\nid = \"from-c\"\nplugin = \"Echo\"\n",
        );
        let mut entries = vec![entry("pull-b", INCLUDE_PLUGIN, json!({"path": "b.toml"}))];
        resolve_includes(&mut entries, dir.path()).unwrap();
        assert_eq!(ids(&entries), vec!["from-b", "from-c"]);
    }

    #[test]
    fn include_cycle_errors() {
        let dir = tempfile::tempdir().unwrap();
        write_entries(
            dir.path(),
            "a.toml",
            "[[entry]]\nid = \"pull-b\"\nplugin = \"@include\"\n[entry.config]\npath = \"b.toml\"\n",
        );
        write_entries(
            dir.path(),
            "b.toml",
            "[[entry]]\nid = \"pull-a\"\nplugin = \"@include\"\n[entry.config]\npath = \"a.toml\"\n",
        );
        let mut entries = vec![entry("start", INCLUDE_PLUGIN, json!({"path": "a.toml"}))];
        let err = resolve_includes(&mut entries, dir.path()).unwrap_err();
        assert!(err.contains("cycle"), "unexpected error: {err}");
    }

    #[test]
    fn include_missing_file_errors_naming_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut entries = vec![entry(
            "start",
            INCLUDE_PLUGIN,
            json!({"path": "does-not-exist.toml"}),
        )];
        let err = resolve_includes(&mut entries, dir.path()).unwrap_err();
        assert!(err.contains("does-not-exist.toml"), "unexpected: {err}");
    }

    #[test]
    fn include_requires_path_string() {
        let dir = tempfile::tempdir().unwrap();
        let mut entries = vec![entry("start", INCLUDE_PLUGIN, json!({"path": 7}))];
        let err = resolve_includes(&mut entries, dir.path()).unwrap_err();
        assert!(err.contains("must be a string"), "unexpected: {err}");
    }

    // -- @group -------------------------------------------------------------

    #[test]
    fn group_flattens_in_place() {
        let mut entries = vec![
            entry("head", "Echo", json!({})),
            entry(
                "grp",
                GROUP_PLUGIN,
                json!({"entries": [
                    {"id": "c1", "plugin": "Echo"},
                    {"id": "c2", "plugin": "Echo"}
                ]}),
            ),
            entry("tail", "Echo", json!({})),
        ];
        resolve_includes(&mut entries, std::path::Path::new(".")).unwrap();
        assert_eq!(ids(&entries), vec!["head", "c1", "c2", "tail"]);
    }

    #[test]
    fn nested_groups_flatten_recursively() {
        let mut entries = vec![entry(
            "outer",
            GROUP_PLUGIN,
            json!({"entries": [
                {"id": "c1", "plugin": "Echo"},
                {"id": "inner", "plugin": "@group", "config": {"entries": [
                    {"id": "d1", "plugin": "Echo"},
                    {"id": "d2", "plugin": "Echo"}
                ]}}
            ]}),
        )];
        resolve_includes(&mut entries, std::path::Path::new(".")).unwrap();
        assert_eq!(ids(&entries), vec!["c1", "d1", "d2"]);
    }

    #[test]
    fn group_include_child_splices_after_flatten() {
        let dir = tempfile::tempdir().unwrap();
        write_entries(
            dir.path(),
            "svc.toml",
            "[[entry]]\nid = \"from-file\"\nplugin = \"Echo\"\n",
        );
        let mut entries = vec![entry(
            "grp",
            GROUP_PLUGIN,
            json!({"entries": [
                {"id": "c1", "plugin": "Echo"},
                {"id": "pull", "plugin": "@include", "config": {"path": "svc.toml"}}
            ]}),
        )];
        resolve_includes(&mut entries, dir.path()).unwrap();
        assert_eq!(ids(&entries), vec!["c1", "from-file"]);
    }

    #[test]
    fn group_duplicate_ids_error() {
        let mut entries = vec![entry(
            "grp",
            GROUP_PLUGIN,
            json!({"entries": [
                {"id": "c1", "plugin": "Echo"},
                {"id": "c1", "plugin": "Echo"}
            ]}),
        )];
        let err = resolve_includes(&mut entries, std::path::Path::new(".")).unwrap_err();
        assert!(
            err.contains("duplicate child entry id 'c1'"),
            "unexpected: {err}"
        );
    }

    #[test]
    fn group_malformed_child_errors() {
        let mut entries = vec![entry(
            "grp",
            GROUP_PLUGIN,
            json!({"entries": [{"plugin": "NoId"}]}),
        )];
        let err = resolve_includes(&mut entries, std::path::Path::new(".")).unwrap_err();
        assert!(err.contains("invalid child entry"), "unexpected: {err}");
    }

    // -- interpolation (feature rhai) ----------------------------------------

    #[cfg(feature = "rhai")]
    #[test]
    fn interpolate_scalar_expression() {
        let mut config = json!({"replicas": "${rhai: 40 + 2}"});
        interpolate_config(&mut config, &json!({"id": "x"})).unwrap();
        assert_eq!(config, json!({"replicas": 42}));
    }

    #[cfg(feature = "rhai")]
    #[test]
    fn interpolate_concat_references_entry_metadata() {
        let mut config = json!({"pool_name": "${rhai: \"srv-\" + entry.id}"});
        let meta = json!({"id": "alpha", "plugin": "Store", "disabled": false});
        interpolate_config(&mut config, &meta).unwrap();
        assert_eq!(config, json!({"pool_name": "srv-alpha"}));
    }

    #[cfg(feature = "rhai")]
    #[test]
    fn interpolate_nested_objects_arrays_and_keys() {
        let mut config = json!({
            "svc": {
                "hosts": ["${rhai: entry.id + \"-1\"}", "plain"],
                "${rhai: \"dyn_\" + \"key\"}": true
            },
            "keep": "${rhail: not-a-marker}"
        });
        interpolate_config(&mut config, &json!({"id": "alpha"})).unwrap();
        assert_eq!(
            config,
            json!({
                "svc": {"hosts": ["alpha-1", "plain"], "dyn_key": true},
                "keep": "${rhail: not-a-marker}"
            })
        );
    }

    #[cfg(feature = "rhai")]
    #[test]
    fn interpolate_map_literal_result_renders_as_json() {
        let mut config = json!({"override": "${rhai: #{model: \"pinned-\" + entry.id}}"});
        interpolate_config(&mut config, &json!({"id": "chat"})).unwrap();
        assert_eq!(config["override"], json!({"model": "pinned-chat"}));
    }

    #[cfg(feature = "rhai")]
    #[test]
    fn interpolate_embedded_marker_concats_mid_string() {
        let mut config = json!({"url": "http://${rhai: entry.id}:8081/v1"});
        interpolate_config(&mut config, &json!({"id": "eruka"})).unwrap();
        assert_eq!(config, json!({"url": "http://eruka:8081/v1"}));
    }

    #[cfg(feature = "rhai")]
    #[test]
    fn bad_expression_error_names_key_path() {
        let mut config = json!({"a": {"b": "${rhai: nosuchfn(9)}"}});
        let err = interpolate_config(&mut config, &json!({"id": "x"})).unwrap_err();
        assert!(err.contains("$.a.b"), "unexpected: {err}");
    }

    #[cfg(feature = "rhai")]
    #[test]
    fn unit_result_is_an_error() {
        let mut config = json!({"x": "${rhai: ()}"});
        let err = interpolate_config(&mut config, &json!({"id": "x"})).unwrap_err();
        assert!(err.contains("evaluated to ()"), "unexpected: {err}");
    }

    #[cfg(feature = "rhai")]
    #[test]
    fn unterminated_placeholder_is_an_error() {
        let mut config = json!({"x": "${rhai: 1 + 2"});
        let err = interpolate_config(&mut config, &json!({"id": "x"})).unwrap_err();
        assert!(err.contains("unterminated"), "unexpected: {err}");
    }

    #[cfg(feature = "rhai")]
    #[test]
    fn braces_inside_string_literals_do_not_confuse_the_scanner() {
        let mut config = json!({"lit": "${rhai: \"}\" + entry.id}"});
        interpolate_config(&mut config, &json!({"id": "ok"})).unwrap();
        assert_eq!(config, json!({"lit": "}ok"}));
    }

    #[cfg(not(feature = "rhai"))]
    #[test]
    fn interpolation_passes_through_without_rhai() {
        let mut config = json!({"x": "${rhai: 40 + 2}", "y": "plain"});
        interpolate_config(&mut config, &json!({"id": "x"})).unwrap();
        assert_eq!(config, json!({"x": "${rhai: 40 + 2}", "y": "plain"}));
    }

    // -- combined -------------------------------------------------------------

    #[cfg(feature = "rhai")]
    #[test]
    fn compose_all_includes_then_interpolates() {
        let dir = tempfile::tempdir().unwrap();
        write_entries(
            dir.path(),
            "common.toml",
            "[[entry]]\nid = \"inc1\"\nplugin = \"Echo\"\n[entry.config]\ntag = \"${rhai: \\\"t-\\\" + entry.id}\"\n",
        );
        let mut entries = vec![
            entry("top", "Echo", json!({"who": "${rhai: entry.id}"})),
            entry("pull", INCLUDE_PLUGIN, json!({"path": "common.toml"})),
        ];
        compose_all(&mut entries, dir.path()).unwrap();
        assert_eq!(ids(&entries), vec!["top", "inc1"]);
        assert_eq!(entries[0].config["who"], json!("top"));
        assert_eq!(entries[1].config["tag"], json!("t-inc1"));
    }

    #[cfg(not(feature = "rhai"))]
    #[test]
    fn compose_all_structural_only_without_rhai() {
        let dir = tempfile::tempdir().unwrap();
        write_entries(
            dir.path(),
            "common.toml",
            "[[entry]]\nid = \"inc1\"\nplugin = \"Echo\"\n[entry.config]\ntag = \"${rhai: 1 + 1}\"\n",
        );
        let mut entries = vec![entry(
            "pull",
            INCLUDE_PLUGIN,
            json!({"path": "common.toml"}),
        )];
        compose_all(&mut entries, dir.path()).unwrap();
        assert_eq!(ids(&entries), vec!["inc1"]);
        assert_eq!(entries[0].config["tag"], json!("${rhai: 1 + 1}"));
    }
}
