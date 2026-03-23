# ARES OSS/Proprietary Split — Combined Plan

**Authors:** Baala (damage assessment + audit, 2026-03-23) + Supra (architecture + execution plan, 2026-03-20)
**Status:** PLANNING — not yet executing

---

## TL;DR

ARES is PUBLIC on GitHub but has ~3,600 lines of proprietary DIRMACS code + leaked credentials in git history. Create **`ares-dirmacs`** (private), which imports `ares` as a library and adds all proprietary layers on top. Clean ares to be a generic OSS runtime. Scrub secrets with `git-filter-repo`.

**Model:** GitLab CE/EE, Grafana OSS/Enterprise, PostHog open-core.

---

## 1. What's Leaked (Damage Assessment)

### Secrets in Git History

| Secret | File | Severity |
|--------|------|----------|
| PostgreSQL password `REDACTED_DB_PASSWORD` | `ares.toml`, `pawan.toml` | **CRITICAL** — DB access |
| Service password `REDACTED_SERVICE_PASSWORD` | `src/mcp/eruka_proxy.rs` diff | **CRITICAL** — Eruka impersonation |
| Eruka user UUID `REDACTED_USER_UUID` | `pawan.toml` | HIGH |
| Client names (ehb, kasino, dinkedin) | `eruka-context.toml` | HIGH — reveals client list |
| All internal domains | `ares.toml` CORS | MEDIUM — maps infra |

### Proprietary Code (All Unconditionally Compiled)

| What | Lines | Files | Why it's private |
|------|-------|-------|-----------------|
| **DSprint** (sales funnel) | 1,912 | `src/dsprint/*`, `src/api/handlers/dsprint.rs` | Pricing logic, tier recommendations, lead capture, onboarding |
| **Eruka integration** (THE MOAT) | 1,267 | `src/middleware/eruka_context.rs`, `src/mcp/eruka_proxy.rs`, `src/mcp/tools.rs`, `src/tools/eruka.rs` | Core differentiator — per-agent knowledge injection |
| **POM tools** | 569 | `src/tools/pom.rs`, `src/api/handlers/public.rs` | Internal project ops + lead capture |
| **DCRM tools** | 406 | `src/dcrm/*`, `src/tools/dcrm.rs` | Internal CRM integration |
| **Deploy handler** | ~100 | `src/api/handlers/deploy.rs` | VPS-specific deploy scripts |
| **Kasino migration** | ~50 | `migrations/002_kasino.sql` | Client-specific DB schema |
| **Client templates** | ~200 | `src/db/tenant_agents.rs` seed data | Hardcoded Kasino/eHB/Dinkedin agent templates |
| **Configs** | — | `eruka-context.toml`, `pawan.toml`, `fly.toml` | Credentials, client mappings, deploy config |

**Total: ~4,500 lines (12%) must move. ~32,600 lines (88%) stay as generic OSS.**

### Cross-References (Tricky Decoupling Points)

| Location | Issue |
|----------|-------|
| `src/agents/configurable.rs:209,305` | Core agent logic calls `get_current_eruka_context()` — needs dual-path |
| `src/api/routes.rs:274` | Eruka middleware applied to ALL V1 routes |
| `src/main.rs:378-393` | Registers 12 proprietary tools unconditionally |
| `src/main.rs:674-701` | Mounts DSprint routes unconditionally |
| `src/api/handlers/admin.rs` | `provision_client()` calls DSprint + template cloning |
| `src/db/tenant_agents.rs:265-470` | `seed_default_templates()` has 200+ lines of client prompts |

---

## 2. Existing Repos — None Can House This Code

We audited every private DIRMACS repo:

| Repo | What it is | Can it house ares business logic? |
|------|-----------|-----------------------------------|
| ares-config | TOML/TOON config only | No — no Rust code |
| dirmacs-admin | Leptos WASM frontend (3.8K LOC) | No — pure frontend |
| enterprise-portal | Leptos WASM frontend (629 LOC) | No — thin portal |
| dirmacs-auth | JWT library (205 LOC) | No — auth infra only |
| dcrm | Standalone Dioxus CRM app | No — unrelated |
| ehb | Health buddy microservice | No — HTTP client |
| doltares | Orchestration daemon | No — different purpose |
| Kasi-NO | Gambling accountability app | No — client vertical |

**Conclusion: A new private repo is needed → `dirmacs/ares-dirmacs`**

---

## 3. Architecture

### ares-dirmacs Imports ares (Not Vice Versa)

```
┌─────────────────────────────────┐
│  ares-dirmacs (PRIVATE)         │  Port 3000 (production binary)
│  ┌───────────────────────────┐  │
│  │  ares (PUBLIC, MIT lib)   │  │  Generic LLM, tools, RAG, auth,
│  │  via Cargo dependency     │  │  agents, MCP, workflows
│  └───────────────────────────┘  │
│  + DSprint pipeline + handlers  │  Proprietary DIRMACS layer
│  + Eruka middleware (THE MOAT)  │  bolted on top
│  + Eruka proxy + MCP tools      │
│  + DCRM/POM tools + handlers    │
│  + Deploy handler               │
│  + Client agent templates       │
│  + Kasino migration             │
└─────────────────────────────────┘
```

**This pattern already works.** Doltares does the same thing — imports `ares-server` as a lib, adds DAG workflows, runs as its own binary on port 3100.

### ares exports `base_router()`

```rust
// ares/src/lib.rs
pub fn base_router(state: AppState) -> Router {
    Router::new()
        .merge(api::routes::health_routes())
        .merge(api::routes::auth_routes(state.clone()))
        .merge(api::routes::chat_routes(state.clone()))
        .merge(api::routes::v1_routes(state.clone()))
        .merge(api::routes::admin_routes(state.clone()))
        // ... all generic routes
        .layer(middleware::usage_layer())
        .with_state(state)
}
```

### ares-dirmacs composes on top

```rust
// ares-dirmacs/src/main.rs
use ares::{base_router, AppState};

#[tokio::main]
async fn main() {
    let state = AppState::from_env();
    let eruka_config = config::load_eruka_context("eruka-context.toml");

    // Seed client templates, run migrations
    dsprint::seed_client_templates(&state.db).await;
    sqlx::migrate!("./migrations").run(&state.db).await.unwrap();

    // Register proprietary tools
    tools::register_dirmacs_tools(&state.tool_registry);

    // Compose: generic base + proprietary layers + THE MOAT
    let app = base_router(state.clone())
        .merge(handlers::dsprint_routes(state.clone()))
        .merge(handlers::deploy_routes(state.clone()))
        .merge(handlers::public_routes(state.clone()))
        .layer(middleware::eruka_context::inject_context);

    axum::serve(listener, app).await.unwrap();
}
```

### Repo Structure

```
/opt/ares-dirmacs/
├── Cargo.toml                    # depends on ares = { path = "../ares" }
├── src/
│   ├── main.rs                   # production binary
│   ├── handlers/
│   │   ├── dsprint.rs            # DSprint HTTP API
│   │   ├── deploy.rs             # VPS deploy
│   │   ├── public.rs             # lead capture
│   │   └── admin_ext.rs          # provision_client()
│   ├── middleware/
│   │   └── eruka_context.rs      # THE MOAT
│   ├── tools/
│   │   ├── eruka.rs              # Eruka read/search
│   │   ├── dcrm.rs               # 4 DCRM tools
│   │   └── pom.rs                # 6 POM tools
│   ├── mcp/
│   │   └── eruka_proxy.rs        # Eruka MCP bridge
│   ├── dcrm/
│   │   └── client.rs             # DCRM HTTP client
│   ├── dsprint/
│   │   ├── recommend.rs          # agent recommendation engine
│   │   ├── email.rs              # email delivery
│   │   ├── email_templates.rs    # DIRMACS-branded templates
│   │   └── stage_tracker.rs      # sales pipeline stages
│   └── config.rs                 # eruka-context.toml reader
├── seeds/
│   ├── kasino.toml               # Kasino agent templates
│   ├── ehb.toml                  # eHB agent templates
│   └── dinkedin.toml             # Dinkedin agent templates
├── migrations/
│   └── 002_kasino.sql            # Kasino-specific tables
├── eruka-context.toml            # agent-to-Eruka mapping
├── pawan.toml                    # MCP config
├── fly.toml                      # Fly.io deploy
└── LICENSE                       # Proprietary
```

---

## 4. Execution (8 Phases, ~3 Days)

### Phase 0: Immediate
- [ ] Make ares private temporarily during the split
- [ ] Backup: `cp -a /opt/ares /opt/ares-backup-$(date +%Y%m%d)`

### Phase 1: Create ares-dirmacs (Day 1, ~2h)
- [ ] `gh repo create dirmacs/ares-dirmacs --private`
- [ ] `cargo init`, set up workspace, add `ares = { path = "../ares" }` dep
- [ ] Create directory structure

### Phase 2: Copy Proprietary Code (Day 1, ~4h)
Copy (not move yet) all proprietary files from ares → ares-dirmacs:
- `src/dsprint/*` → `src/dsprint/`
- `src/dcrm/*` → `src/dcrm/`
- `src/api/handlers/dsprint.rs` → `src/handlers/dsprint.rs`
- `src/api/handlers/deploy.rs` → `src/handlers/deploy.rs`
- `src/api/handlers/public.rs` → `src/handlers/public.rs`
- `src/middleware/eruka_context.rs` → `src/middleware/eruka_context.rs`
- `src/mcp/eruka_proxy.rs` → `src/mcp/eruka_proxy.rs`
- `src/tools/{eruka,dcrm,pom}.rs` → `src/tools/`
- `eruka-context.toml`, `pawan.toml`, `fly.toml`
- `migrations/002_kasino.sql`

### Phase 3: Make ares Export `base_router()` (Day 1, ~2h)
- [ ] Add `pub fn base_router()` to `src/lib.rs`
- [ ] Update `src/main.rs` to use `base_router()` (standalone generic mode)
- [ ] Verify: `cargo build` ares standalone compiles

### Phase 4: Build ares-dirmacs main.rs (Day 1, ~2h)
- [ ] Import `ares::base_router`, register tools, merge routes, layer middleware
- [ ] Wire eruka config, DCRM client, template seeding
- [ ] Verify: `cargo build` ares-dirmacs compiles

### Phase 5: Clean ares (Day 2, ~3h)
- [ ] Delete all moved files from ares
- [ ] Update module declarations (lib.rs, tools/mod.rs, handlers/mod.rs, etc.)
- [ ] Clean `configurable.rs` — remove `get_current_eruka_context()` calls
- [ ] Clean `tenant_agents.rs` — remove Kasino/eHB/Dinkedin seed templates
- [ ] Clean `admin.rs` — extract `provision_client()` to ares-dirmacs
- [ ] Clean `main.rs` — remove proprietary tool registration, DSprint routes, dirmacs URLs
- [ ] Clean `mcp/tools.rs` — remove Eruka types
- [ ] Clean configs — localhost defaults, placeholder domains in example
- [ ] Verify: `cargo build` ares → zero DIRMACS references
- [ ] Verify: `cargo build` ares-dirmacs → everything works

### Phase 6: Deploy on VPS (Day 2, ~2h)
- [ ] Build: `cd /opt/ares-dirmacs && cargo build --release`
- [ ] Update systemd: `ExecStart=/opt/ares-dirmacs/target/release/ares-dirmacs`
- [ ] `systemctl restart ares`
- [ ] Verify: health, chat, DSprint, admin, MCP, Eruka context injection

### Phase 7: Scrub Git History (Day 2, ~1h)
- [ ] `git-filter-repo --replace-text` to scrub all secrets
- [ ] Force push all branches
- [ ] Verify: `git log --all -p | grep "2ab91004" | wc -l` → 0

### Phase 8: Publish + Rotate (Day 3)
- [ ] Make `dirmacs/ares` public again
- [ ] Keep `dirmacs/ares-dirmacs` private forever
- [ ] Rotate ALL exposed credentials (DB password, JWT secret, API keys, service passwords)
- [ ] Update ares README for OSS audience
- [ ] Add CONTRIBUTING.md + proper .gitignore
- [ ] Add pre-commit hook + CI guard (see Guardrails below)

---

## 5. Guardrails Against Future Leaks

### CLAUDE.md for ares (PUBLIC)
```
NEVER add: client names, DIRMACS business logic, eruka integration,
DCRM/POM tools, dirmacs.com URLs, credentials, deploy scripts.
Proprietary code → /opt/ares-dirmacs.
```

### Pre-commit Hook
```bash
grep -rn "kasino\|ehb\|dinkedin\|dsprint\|dirmacs-service\|bom@dirmacs\|eruka-context\.toml" src/ migrations/ && exit 1
```

### CI Guard (.github/workflows/oss-guard.yml)
Same grep in GitHub Actions — blocks PRs with proprietary content.

---

## 6. Comparison: Supra's Plan vs Baala's Audit

| Aspect | Supra's Plan (Mar 20) | Baala's Audit (Mar 23) |
|--------|----------------------|----------------------|
| Repo name | `ares-dirmacs` | `ares-managed` → **aligned to `ares-dirmacs`** |
| Pattern | Private imports public | Same |
| File list | Complete (14 moves) | Same + deploy.rs we missed |
| History scrub | BFG or squash | `git-filter-repo` (more precise) |
| configurable.rs | Not mentioned | Identified as HIGH RISK (lines 209, 305) |
| tenant_agents.rs | Identified seed cleanup | Not mentioned → **adding** |
| Kasino migration | Identified | Not mentioned → **adding** |
| admin.rs provision_client | Identified for extraction | Not mentioned → **adding** |
| Guardrails | CLAUDE.md + hook + CI | Not covered → **adding** |
| Downstream impact | Not covered | doltares verified safe |

**Verdict: Plans are 90% aligned. Merging both for completeness.**

---

## 7. Quick Reference — Where Does New Code Go?

| Adding... | Repo | Why |
|-----------|------|-----|
| New LLM provider | ares | Generic |
| New vector store | ares | Generic |
| New built-in tool | ares | Generic |
| New MCP transport | ares | Generic |
| New API endpoint (generic) | ares | Generic |
| Client agent templates | ares-dirmacs | Proprietary |
| Eruka anything | ares-dirmacs | Proprietary |
| DCRM/POM anything | ares-dirmacs | Proprietary |
| DSprint anything | ares-dirmacs | Proprietary |
| Deploy/ops automation | ares-dirmacs | Proprietary |
| DIRMACS branding/URLs | ares-dirmacs | Proprietary |
