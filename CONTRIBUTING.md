# Contributing to A.R.E.S

Thank you for your interest in contributing to A.R.E.S (Agentic Runtime Extensible Server). This document gives the guidelines and instructions for contributions.

## Table of contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Feature Flags](#feature-flags)
- [CLI Development](#cli-development)
- [UI Development](#ui-development)
- [Making Changes](#making-changes)
- [Testing](#testing)
- [Code Style](#code-style)
- [Pull Request Process](#pull-request-process)
- [Release Process](#release-process)

## Code of conduct

This project follows the [Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct). Be respectful and constructive in all interactions.

## Getting started

### Prerequisites

- Rust 1.98+: Install through [rustup](https://rustup.rs/)
- Git: for version control
- **just** (recommended): command runner — [Install just](https://just.systems)
- **Docker** (optional): for the Qdrant vector database
- **Ollama** (optional): for local LLM inference
- **Node.js runtime** (for UI development): bun, npm, or deno

### Fork and clone

1. Fork the repository on GitHub
2. Clone your fork:
 ```bash
  git clone https://github.com/dirmacs/ares.git
  cd ares
  ```
3. Add the upstream remote:
```bash
  git remote add upstream https://github.com/dirmacs/ares.git
  ```

## Development setup

### Environment variables

Create a `.env` file from the example:

```bash
cp .env.example .env
```

Set these variables for your setup:

```bash
# Server Configuration
HOST=127.0.0.1
PORT=3000

# Database (PostgreSQL via DATABASE_URL; Turso/libSQL optional)
DATABASE_URL=postgres://localhost/ares
TURSO_URL=
TURSO_AUTH_TOKEN=

# LLM Provider (choose one or more)
# Option 1: Ollama (recommended for local development)
OLLAMA_BASE_URL=http://localhost:11434
OLLAMA_MODEL=ministral-3:3b

# Option 2: OpenAI
# OPENAI_API_KEY=sk-your-key
# OPENAI_API_BASE=https://api.openai.com/v1
# OPENAI_MODEL=gpt-4

# Option 3: LlamaCpp (direct GGUF model loading)
# LLAMACPP_MODEL_PATH=/path/to/model.gguf

# Authentication
JWT_SECRET=your-development-secret-key-min-32-chars
API_KEY=dev-api-key

# Optional: Qdrant for vector search
QDRANT_URL=http://localhost:6334
# QDRANT_API_KEY=
```

### Building the project

```bash
# Build with default features (postgres, ares-vector, mcp). HTTP LLM providers come from genai.
cargo build
# Or: just build

# Optional in-process GGUF
cargo build --features llamacpp
# Or: just build-features llamacpp

# Build with all features
cargo build --all-features
# Or: just build-all

# Build release version
cargo build --release
# Or: just build-release
```

### Running locally

```bash
# Start with default configuration
cargo run
# Or: just run

# Run with optional GGUF
cargo run --features llamacpp
# Or: just run-features llamacpp

# With debug logging
RUST_LOG=debug cargo run
# Or: just run-debug

# With trace logging
RUST_LOG=trace cargo run
# Or: just run-trace
```

## Feature flags

A.R.E.S uses Cargo features for optional compile-time components. Know these flags before you develop:

### LLM providers

HTTP LLM providers ship through the default `genai` feature of `ares-llm`.
Select OpenAI, Azure, Anthropic, Gemini, Bedrock bearer, and Ollama at run time with `type` in `ares.toml`.
These Cargo features were removed: `openai`, `azure`, `bedrock`, `ollama`, `anthropic`, `all-llm`.

| Feature | Description | Dependencies |
|---------|-------------|--------------|
| `llamacpp` | Direct GGUF loading (root forwards to ares-llm) | `llama-cpp-2` |
| `llamacpp-cuda` | LlamaCpp + CUDA | GPU drivers |
| `llamacpp-metal` | LlamaCpp + Metal | macOS only |
| `llamacpp-vulkan` | LlamaCpp + Vulkan | Vulkan SDK |

### Database backends

| Feature | Description |
|---------|-------------|
| `postgres` | PostgreSQL through sqlx (default) |
| `turso` | Remote Turso / libSQL database |

### Vector stores

| Feature | Description |
|---------|-------------|
| `ares-vector` | Embedded pure-Rust vector store (default) |
| `qdrant` | Qdrant vector database |
| `lancedb` | LanceDB embedded store |
| `pgvector` | pgvector through PostgreSQL |
| `chromadb` | ChromaDB client |
| `pinecone` | Pinecone client (alpha) |

### UI & documentation features

| Feature | Description |
|---------|-------------|
| `ui` | Embedded Leptos web UI served from the backend |
| `swagger-ui` | Interactive Swagger UI API documentation at `/swagger-ui/` |

> **Note:** v0.2.5 made the `swagger-ui` feature optional. The binary is smaller. The build is faster. The feature needs network access during the build to download Swagger UI assets.

### Feature bundles

| Feature | Includes |
|---------|----------|
| `all-db` | postgres |
| `all-vectorstores` | ares-vector + qdrant + pgvector + chromadb + pinecone |
| `full` | postgres, qdrant, ares-vector, mcp, swagger-ui |
| `full-ui` | full + ui |
| `minimal` | No optional features |

### Working with features

```bash
# Test with a specific feature combination
cargo test --features qdrant

# Check that code compiles with minimal features
cargo check --features "minimal"

# Run clippy with the full feature set (except UI)
cargo clippy --features "full"
# Or: just lint-all

# Build with UI feature (requires Node.js runtime)
cargo build --features "ui"
# Or: just build-ui

# Build with Swagger UI (interactive API documentation)
cargo build --features "swagger-ui"

# Build with both UI and Swagger UI
cargo build --features "ui,swagger-ui"
```

> **Note about docs.rs:** Some features (`llamacpp`, `qdrant`, `swagger-ui`) cannot build on docs.rs because of their build requirements (native compilation, network access, or filesystem writes).

## Using just (recommended)

A.R.E.S uses [just](https://just.systems) as a command runner to simplify development workflows:

```bash
# Install just
brew install just          # macOS
cargo install just         # Any platform

# See all available commands
just --list

# Common development workflows
just build                 # Build debug
just build-ui              # Build with embedded UI
just test                  # Run tests
just lint                  # Run clippy
just fmt                   # Format code
just quality               # Run all quality checks (fmt-check + lint)
just ci                    # Run full CI checks

# CLI commands
just init                  # Initialize project (ares-server init)
just config                # Show configuration summary
just agents                # List all agents

# Docker workflows
just docker-up             # Start dev environment
just docker-down           # Stop services
just docker-logs           # View logs

# Testing workflows
just test-verbose          # Tests with output
just test-ignored          # Run live Ollama tests
just test-all              # Run all tests
just hurl                  # Run API tests
just hurl-verbose          # API tests with verbose output

# UI development
just ui-setup              # Install UI dependencies
just ui-dev                # Run UI dev server
just ui-build              # Build UI for production
just dev                   # Run backend + UI together
just check-node            # Check for Node.js runtime

# Pre-commit workflow
just pre-commit            # Format, lint, and test
```

## Making changes

### Branch naming

Use descriptive branch names:

- `feature/add-anthropic-provider`
- `fix/openai-streaming-bug`
- `docs/update-readme`
- `refactor/llm-client-trait`

### Commit messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer(s)]
```

Types:
- `feat`: new feature
- `fix`: bug fix
- `docs`: documentation changes
- `style`: code style changes (formatting and more)
- `refactor`: code refactoring
- `test`: added or updated tests
- `chore`: maintenance tasks

Examples:
```
feat(llm): add streaming support for Bedrock

Implements token-by-token streaming using tokio channels.
Resolves #123

fix(auth): handle expired refresh tokens correctly

test(api): add concurrent login tests
```

### Adding new features

1. For significant changes, open an issue first to discuss the approach
2. Gate optional functionality behind Cargo features
3. Update README and add doc comments
4. Add unit and integration tests
5. Consider usage examples where they help

### Adding a new LLM provider

1. Create `crates/ares-llm/src/your_provider.rs`
2. Implement the `LLMClient` trait
3. Add the feature flag in `Cargo.toml`
4. Extend the `Provider` enum in `crates/ares-llm/src/client.rs`
5. Add tests
6. Document the environment variables

### Adding a new tool

1. Create `crates/ares-tools/src/tools/your_tool.rs`
2. Implement the `Tool` trait
3. Register the tool in the tool registry factory
4. Add tests
5. Document the tool purpose and parameters

### Adding a new agent (via TOML)

New agents need only configuration in `ares.toml`:

```toml
[agents.my_custom_agent]
model = "balanced"                          # Reference a defined model
tools = ["calculator", "web_search"]        # Tools this agent can use
system_prompt = """
You are a custom agent specialized in...
Your role is to...
"""
```

The `ConfigurableAgent` picks up this configuration automatically.

### Adding a new workflow

Workflows also live in `ares.toml`:

```toml
[workflows.my_workflow]
entry_agent = "my_custom_agent"        # First agent to handle requests
fallback_agent = "product"             # Fallback if entry agent fails
max_depth = 5                          # Maximum routing depth
```

## CLI development

The CLI lives in `src/cli/` with this structure:

```
src/cli/
 mod.rs      # CLI argument parsing with clap
 init.rs     # Init command scaffolding logic
 output.rs   # Colored TUI output helpers
 rag.rs      # RAG subcommands
```

### Adding a new CLI command

1. Add the command variant to the `Commands` enum in `src/cli/mod.rs`
2. Implement the command handler in `src/main.rs`
3. Add tests in `tests/cli_tests.rs`

### CLI testing

```bash
# Run CLI unit tests
cargo test --lib cli::

# Run CLI integration tests
cargo test --test cli_tests

# Test the init command manually
cargo run -- init /tmp/test-project
cargo run -- config --config /tmp/test-project/ares.toml
cargo run -- agent list --config /tmp/test-project/ares.toml
```

## UI development

The embedded web UI is built with Leptos and requires a Node.js runtime (bun, npm, or deno).

### Prerequisites

```bash
# Check for Node.js runtime
just check-node

# Install WASM target
rustup target add wasm32-unknown-unknown

# Install trunk
cargo install trunk --locked

# Install UI dependencies
cd ui && bun install  # or npm install
```

### Development workflow

```bash
# Run UI dev server (hot reload)
just ui-dev
# Or: cd ui && trunk serve --open

# Run backend and UI together
just dev

# Build UI for production
just ui-build
# Or: cd ui && trunk build --release

# Build backend with embedded UI
just build-ui
# Or: cargo build --features "ui"
```

### UI project structure

```
ui/
 src/
    lib.rs        # Main app component
    api.rs        # API client
    state.rs      # Global state management
    types.rs      # Type definitions
    components/   # Reusable UI components
    pages/        # Page components
 index.html        # HTML template
 Trunk.toml        # Trunk configuration
 Cargo.toml        # Rust dependencies
 tailwind.config.js # Tailwind CSS config
```

### Node.js runtime detection

The build system detects available runtimes automatically:

1. bun (preferred) — fastest
2. npm — standard Node.js package manager
3. deno — alternative runtime

If no runtime exists, the build fails with instructions.

### Architecture: key registries

Know these core components when you contribute code:

- `AresConfigManager` (`crates/ares-http/src/overlay.rs`): Thread-safe configuration access with hot-reload. Also aliased as `Overlay` for the loader
- `ProviderRegistry` (`crates/ares-llm/src/provider_registry.rs`): creates LLM clients from configuration
- `AgentRegistry` (`crates/ares-agent/src/registry.rs`): creates agents from TOML definitions
- `ToolRegistry` (`crates/ares-tools/src/registry.rs`): manages tool availability and configuration
- `WorkflowEngine` (`crates/ares-agent/src/workflows/engine.rs`): executes declarative workflows
- `ConfigurableAgent` (`crates/ares-agent/src/configurable.rs`): generic configuration-driven agent

### Configuration validation

The configuration system validates:
- Reference integrity (models → providers, agents → models, workflows → agents)
- Circular references in workflows
- Environment variable availability

Use `config.validate_with_warnings()` to also get warnings about unused configuration items.

## Testing

### Running tests

```bash
# Run all tests (mocked, no external services required)
cargo test
# Or: just test

# Run CLI tests specifically
cargo test --lib cli::
cargo test --test cli_tests
# Or: just test-filter cli

# Run with specific features
cargo test --features llamacpp

# Run a specific test
cargo test test_name
# Or: just test-filter test_name

# Run tests with output
cargo test -- --nocapture
# Or: just test-verbose

# Run only integration tests
cargo test --test '*'
# Or: just test-integration

# Run only unit tests
cargo test --lib
# Or: just test-lib
```

### Live Ollama tests

Additional tests connect to a **real Ollama instance**. These tests are **ignored by default**, and an explicit flag enables them.

#### Prerequisites

1. A running Ollama server (default: `http://localhost:11434`)
2. A pulled model (for example, `ollama pull ministral-3:3b`)

#### Running live tests

**Option 1: use just (recommended)**

```bash
# Run all ignored tests (including live Ollama tests)
just test-ignored

# Run with verbose output
just test-ignored-verbose

# Run all tests (normal + ignored)
just test-all
```

**Option 2: set the environment variable in your shell**

```bash
# Bash/Zsh
OLLAMA_LIVE_TESTS=1 cargo test --test ollama_live_tests -- --ignored

# Nushell
$env.OLLAMA_LIVE_TESTS = "1"; cargo test --test ollama_live_tests -- --ignored

# PowerShell
$env:OLLAMA_LIVE_TESTS = "1"; cargo test --test ollama_live_tests -- --ignored
```

**Option 3: add to your `.env` file**

```bash
# Add to .env
OLLAMA_LIVE_TESTS=1
```

Then run:
```bash
# Source .env first if needed, or use a tool like dotenv
cargo test --test ollama_live_tests -- --ignored
```

#### Configuring live tests

Customize the Ollama connection:

```bash
# Custom Ollama URL
OLLAMA_URL=http://192.168.1.100:11434 OLLAMA_LIVE_TESTS=1 cargo test --test ollama_live_tests -- --ignored

# Custom model
OLLAMA_MODEL=mistral OLLAMA_LIVE_TESTS=1 cargo test --test ollama_live_tests -- --ignored
```

#### Live test coverage

The live tests cover:
- Connection verification
- Basic text generation
- System prompt handling
- Conversation history
- Streaming responses
- Tool calling
- Error handling (invalid models)
- Sequential and concurrent requests

### Test coverage

```bash
# Install coverage tool
cargo install cargo-llvm-cov

# Generate HTML coverage report
cargo llvm-cov --html --open

# Generate lcov report
cargo llvm-cov --lcov --output-path lcov.info
```

### Writing tests

- Place unit tests in the same file inside `#[cfg(test)]` modules
- Place integration tests in the `tests/` directory
- Use `mockall` to mock traits
- Use `wiremock` to mock HTTP endpoints
- Use `tempfile` for temporary file and database tests

Example test structure:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_functionality() {
        // Arrange
        let input = "test";

        // Act
        let result = function_under_test(input);

        // Assert
        assert_eq!(result, expected);
    }

    #[tokio::test]
    async fn test_async_functionality() {
        // Test async code
    }
}
```

## Code style

### Formatting

```bash
# Format code
cargo fmt

# Check formatting (CI fails on unformatted code)
cargo fmt -- --check
```

### Linting

```bash
# Run clippy
cargo clippy

# Treat warnings as errors (as in CI)
cargo clippy -- -D warnings

# With all features
cargo clippy --all-features -- -D warnings
```

### Documentation

- All public items carry doc comments
- Use `///` for item documentation
- Use `//!` for module-level documentation
- Include examples in doc comments where they help
- Update CHANGELOG.md for notable changes
- Update README.md for user-facing features
- Update docs/QUICK_REFERENCE.md for new commands

```rust
/// Creates a new LLM client for the specified provider.
///
/// # Arguments
///
/// * `provider` - The LLM provider configuration
///
/// # Returns
///
/// A boxed trait object implementing `LLMClient`
///
/// # Errors
///
/// Returns an error if the provider cannot be initialized
///
/// # Example
///
/// ```rust,ignore
/// let client = create_client(Provider::Ollama {
/// base_url: "http://localhost:11434".into(),
/// model: "ministral-3:3b".into(),
/// }).await?;
/// ```
pub async fn create_client(provider: Provider) -> Result<Box<dyn LLMClient>> {
    // ...
}
```

## Pull request process

### Before you submit

1. Rebase on latest `main`
2. Run `cargo fmt`
3. Run `cargo clippy --features "full"`
4. Run `cargo test`
5. Run `cargo test --test cli_tests` for CLI changes
6. Update documentation if needed (README, QUICK_REFERENCE, CHANGELOG)
7. Add or update tests for the changes

### PR description template

```markdown
## Description

Brief description of changes.

## Type of Change

- [ ] Bug fix
- [ ] New feature
- [ ] Breaking change
- [ ] Documentation update

## Related Issues

Fixes #(issue number)

## Testing

Describe testing done.

## Checklist

- [ ] Code follows project style guidelines
- [ ] Self-review completed
- [ ] Documentation updated
- [ ] Tests added/updated
- [ ] All tests pass
```

### Review process

1. Automated CI checks must pass
2. At least one maintainer must approve
3. Address review feedback
4. Squash commits on request
5. A maintainer merges when ready

## Release process

Maintainers manage releases:

1. Update the version in `Cargo.toml`
2. Update CHANGELOG.md
3. Create the git tag: `git tag v0.x.y`
4. Push the tag: `git push origin v0.x.y`
5. GitHub Actions creates the release

### Versioning

We follow [Semantic Versioning](https://semver.org/):

- MAJOR: breaking API changes
- MINOR: new features, backward compatible
- PATCH: bug fixes, backward compatible

## Getting help

- Issues: search existing issues or create a new one
- Discussions: for questions and ideas

## Recognition

Contributors receive recognition in:
- CHANGELOG.md for their specific contributions
- README.md contributors section
- GitHub release notes

Thank you for contributing to A.R.E.S!
