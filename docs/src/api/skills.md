# Skills

ARES discovers and loads SKILL.md files through the `skills` feature. This feature uses [thulp-skill-files](https://crates.io/crates/thulp-skill-files).

## Feature flag

```toml
[dependencies]
ares-server = { version = "0.9", features = ["skills"] }
```

## Configuration

You configure the skill directories in `ares-server`:

```toml
[skills]
project_dir = "./.claude/skills/"
personal_dir = "~/.claude/skills/"
plugin_dirs = ["./plugins/my-plugin/skills"]
```

## API

### List skills

```
GET /api/skills
```

This endpoint returns all discovered skills with scope-based priority: project > personal > enterprise > plugin.

### Get skill

```
GET /api/skills/{name}
```

This endpoint returns one skill by qualified name, with its full body content.

## Library usage

Skills also have a library API for direct Rust usage:

```rust
use ares_agent::skills::{SkillsConfig, get_skill, list_skills, load_skills};

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

## Skill file format

A skill is a SKILL.md file with YAML frontmatter:

```markdown
---
name: my-skill
description: What this skill does
---

# My skill

Instructions for the AI agent...
```

## Scope priority

When multiple skills share a name, scope priority selects the winner:

1. **Project**, `./.claude/skills/` (highest priority)
2. **Personal**, `~/.claude/skills/`
3. **Enterprise**, organization-wide skills
4. **Plugin**, from plugin directories (lowest priority)
