# Tenant FS permission fence

File-manipulation tools give agents real power over the host filesystem. That power needs a fence. This page defines the fence that will gate every future file tool. The policy core ships now in `crates/ares-tools/src/fence.rs`. Tool wiring comes later. This document is the contract for both halves.

## Status

The module `ares_tools::fence` holds the decision types and the L0, L1, and L2 checks. It exposes `check_path`, `check_read`, and `check_write`. No tool calls these functions yet. Layer L3 needs the read-hash ledger of the future file tools. It joins at wiring time.

## Design principles

The fence makes decisions before any tool touches the disk where possible. Every denial names the layer that produced it. Agents see the failing rule and can correct their plan. Policies are plain values, so tests construct them without setup.

## The four layers

A request must pass all four layers. The fence checks layers in a fixed order: L0, L1, L2, L3. The first failure wins. Its layer appears in the returned decision, so outcomes stay deterministic for tests and for error messages.

### L0 - Sandbox mode

Each session carries exactly one mode:

| Mode | Reads | Writes |
|------|-------|--------|
| `ReadOnly` | Judged by L1 and L2 | Always denied |
| `WorkspaceWrite` (default) | Judged by L1 and L2 | Allowed below the workspace root |
| `Full` | Judged by L2 | Allowed anywhere except blocklist hits |

Mode is per-session state. It is never global. A session can switch modes at runtime. The policy value clones cheaply, so each conversation holds its own copy. One user cannot change the mode of another session.

### L1 - Workspace boundary

The fence resolves every candidate path before judgment. Resolution anchors relative paths on the tenant workspace root. It removes `.` parts and folds `..` parts lexically. Then it canonicalizes the deepest existing ancestor and rejoins the missing tail. Fresh tail names need no resolution because they do not exist yet.

The canonical result must start with the workspace root. The comparison works component by component. Lexical neighbors such as `/workspace` and `/workspace-evil` never satisfy it. Canonicalization follows symlink targets, so a link planted inside the workspace that points outside becomes a violation. A failed canonicalization is also a violation. The fence reports the boundary layer either way.

Roots come from per-tenant isolation. Each tenant sees exactly one root. `Full` mode waives this layer, and L2 still judges every path.

### L2 - Sensitive-file blocklist

Some names stay forbidden in every mode. The blocklist covers credential files, key material, database files, `.env` variants, and ssh directories. Deployments extend the list through configuration, so operators add site-specific entries without code changes.

The matcher walks every component of the path. Each component name is compared against all patterns. Comparison ignores letter case. Patterns accept the `*` and `?` wildcards. An illustrative deployment list looks like this:

```toml
blocklist = [".env", ".env.*", "*.pem", "*.key", "id_rsa*", ".ssh", "*.db"]
```

The fence scans the raw spelling first and the canonical spelling second. A symlink therefore cannot smuggle a protected name past the scan. The blocklist applies to reads and writes alike. `Full` mode does not weaken it.

### L3 - Write guards

Writes face four extra guards once the file tools arrive:

1. **Read-before-edit.** The tool records the content hash of the last read. At write time the current file hash must match that record. A mismatch means another writer changed the file in between. The write fails and directs the agent to re-read.
2. **Size ceiling.** Writes above a configured byte limit are denied. The limit stops runaway output from filling the disk.
3. **Binary-extension denylist.** Writes to configured executable or archive extensions are denied. Text-oriented agents corrupt binaries by accident. The denylist removes that accident class.
4. **Atomic write.** The tool writes to a temporary file in the target directory and renames it into place. Rename within one directory is atomic on local filesystems. Interrupted writes then leave no torn file behind.

These guards live beside the tools, not in the path checker. They need the read ledger that only the tools own. Their denials still report the same layer name, so callers see one uniform scheme.

## Threat model

Each layer stops a different attack path. L0 stops a hijacked or confused agent from mutating anything during a review session. `ReadOnly` refuses every write before the process opens a file. L1 stops escapes from the tenant workspace. Parent traversal, planted symlinks, and absolute paths into other tenants or system locations all fail the prefix test. One tenant cannot reach the files of another tenant through the fence. L2 stops quiet credential theft from inside the walls. Workspaces often hold `.env` files or private keys beside ordinary project files. The blocklist denies those reads in every mode, including `Full`. L3 stops silent data damage. Stale-hash edits lose concurrent updates, oversized writes fill disks, careless writes corrupt binaries, and killed processes leave half-written files. One guard addresses each of those four failures.

## API sketch

The shipped skeleton matches this shape:

```rust
use std::path::{Path, PathBuf};

pub struct FencePolicy {
    pub mode: FenceMode,
    pub workspace_root: PathBuf,
    pub blocklist: Vec<String>,
}

pub enum FenceMode {
    ReadOnly,
    WorkspaceWrite,
    Full,
}

pub enum FenceLayer {
    L0Mode,
    L1Boundary,
    L2Blocklist,
    L3WriteGuard,
}

pub enum FenceDecision {
    Allowed,
    Denied {
        layer: FenceLayer,
        reason: String,
    },
}

impl FencePolicy {
    pub fn check_read(&self, raw: &Path) -> FenceDecision;
    pub fn check_write(&self, raw: &Path) -> FenceDecision;
}

// Free function behind both methods. `write == false` means read.
pub fn check_path(policy: &FencePolicy, raw: &Path, write: bool) -> FenceDecision;
```

Callers turn a denial into a tool error without building extra context. The reason string carries a human-readable cause, for example `blocked name: .env`. A typical call site looks like this:

```rust
let policy = FencePolicy::new(
    FenceMode::WorkspaceWrite,
    &session.workspace_root,
    config.blocklist.clone(),
);

if let FenceDecision::Denied { layer, reason } = policy.check_write(&candidate) {
    return Err(format!("fence rejected the write at {layer:?}: {reason}"));
}
```

## Attachment through tenant isolation

Each tenant gets its own fence instance. The attachment reuses the per-tenant service isolate pattern that already exists in the kernel. `cordis::Context::isolate_type` builds a child context in which one service type resolves to a per-realm registration instead of the parent registration. `ares_store::TenantRealms::open` applies that pattern today. It creates the child with `root.extend().isolate_type(tools_type_id, tenant_id)` and caches one child per tenant id.

The fence joins that pattern at wiring time. Each realm will register its own tools wrapper carrying the tenant `FencePolicy` and the tenant workspace root. Mode changes will mutate only the policy copy of that realm. Other tenants will never observe them. Shared engine services stay outside the isolate, so execution remains common while file access stays fenced.

## Testing matrix

| Layer | Case | Expected decision |
|-------|------|-------------------|
| L0 | Write while mode is `ReadOnly` | Denied at L0Mode |
| L0 | Read while mode is `ReadOnly` | Passes L0, L1 and L2 still judge |
| L1 | Relative path inside the workspace | Allowed |
| L1 | Path with `..` climbing above the root | Denied at L1Boundary |
| L1 | Symlink inside the root pointing outside | Denied at L1Boundary |
| L1 | Absolute path outside the workspace | Denied at L1Boundary |
| L1 | Missing intermediate directories | Resolved first, then judged normally |
| L1 | Any path while mode is `Full` | L1 steps aside, L2 still judges |
| L2 | `.env` at any depth, read or write | Denied at L2Blocklist |
| L2 | Wildcard match such as `server.pem` | Denied at L2Blocklist |
| L2 | Uppercase spelling such as `SERVER.PEM` | Denied at L2Blocklist |
| L2 | Protected name reached through a symlink | Denied at L2Blocklist |
| L3 | Edit without a recorded read | Denied at L3WriteGuard (planned) |
| L3 | Payload above the size ceiling | Denied at L3WriteGuard (planned) |
| L3 | Write to a denylisted binary extension | Denied at L3WriteGuard (planned) |
| L3 | File changed between read and write | Denied at L3WriteGuard (planned) |

The unit tests in `crates/ares-tools/src/fence.rs` cover the shipped rows today. Integration tests will cover the planned L3 rows together with the tool wiring.

## Non-goals

The fence judges filesystem paths only. It does not attempt:

- **Network egress control.** Outbound connection policy belongs to the subprocess service.
- **Process-level sandboxing.** Operating-system users, namespaces, and similar controls stay with the subprocess service.
- **Secret storage and rotation.** Store and authentication components own credential custody.
- **Content inspection.** The fence judges names and directory structure. It does not scan file bodies for secrets.
