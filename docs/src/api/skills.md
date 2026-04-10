# Skills

ARES supports SKILL.md file discovery and loading via the `skills` feature flag, powered by [thulp-skill-files](https://crates.io/crates/thulp-skill-files).

## Feature Flag

```toml
[dependencies]
ares-server = { version = "0.7", features = ["skills"] }
```

## Configuration

Configure skill directories in your `ares.toml`:

```toml
[skills]
project_dir = "./.claude/skills/"
personal_dir = "~/.claude/skills/"
plugin_dirs = ["./plugins/my-plugin/skills"]
```

## Library Usage

Skills are available as a library API only — there are no HTTP endpoints for skill discovery. Use the functions below in your Rust code:

```rust
use ares::skills::{SkillsConfig, load_skills, list_skills, get_skill};

let config = SkillsConfig {
    project_dir: Some("./.claude/skills/".into()),
    personal_dir: Some("~/.claude/skills/".into()),
    ..Default::default()
};

// Load all skills
let skills = load_skills(&config);

// List summaries (name, description, scope)
let summaries = list_skills(&config);

// Get specific skill
let skill = get_skill(&config, "my-skill");
```

## Skill File Format

Skills are SKILL.md files with YAML frontmatter:

```markdown
---
name: my-skill
description: What this skill does
---

# My Skill

Instructions for the AI agent...
```

## Scope Priority

When multiple skills share the same name, scope priority determines which wins:

1. **Project** — `./.claude/skills/` (highest priority)
2. **Personal** — `~/.claude/skills/`
3. **Enterprise** — organization-wide skills
4. **Plugin** — from plugin directories (lowest priority)
