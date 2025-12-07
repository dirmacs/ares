# Contributing to A.R.E.S

Thank you for your interest in contributing to A.R.E.S (Agentic Retrieval Enhanced Server)! This document provides guidelines and instructions for contributing.

## Table of Contents

- [Code of Conduct](#code-of-conduct)
- [Getting Started](#getting-started)
- [Development Setup](#development-setup)
- [Feature Flags](#feature-flags)
- [Making Changes](#making-changes)
- [Testing](#testing)
- [Code Style](#code-style)
- [Pull Request Process](#pull-request-process)
- [Release Process](#release-process)

## Code of Conduct

This project follows the [Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct). Please be respectful and constructive in all interactions.

## Getting Started

### Prerequisites

- **Rust 1.91+**: Install via [rustup](https://rustup.rs/)
- **Git**: For version control
- **Docker** (optional): For running Qdrant vector database
- **Ollama** (optional): For local LLM inference

### Fork and Clone

1. Fork the repository on GitHub
2. Clone your fork:
   ```bash
   git clone https://github.com/YOUR_USERNAME/ares.git
   cd ares
   ```
3. Add the upstream remote:
   ```bash
   git remote add upstream https://github.com/original-org/ares.git
   ```

## Development Setup

### Environment Variables

Create a `.env` file from the example:

```bash
cp .env.example .env
```

Configure the following variables based on your setup:

```bash
# Server Configuration
HOST=127.0.0.1
PORT=3000

# Database (local SQLite for development)
TURSO_URL=file:local.db
TURSO_AUTH_TOKEN=

# LLM Provider (choose one or more)
# Option 1: Ollama (recommended for local development)
OLLAMA_BASE_URL=http://localhost:11434
OLLAMA_MODEL=llama3.2

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

### Building the Project

```bash
# Build with default features (local-db + ollama)
cargo build

# Build with specific features
cargo build --features "ollama,openai"

# Build with all features
cargo build --all-features

# Build release version
cargo build --release
```

### Running Locally

```bash
# Start with default configuration
cargo run

# Run with specific features
cargo run --features "ollama"

# With logging
RUST_LOG=debug cargo run
```

## Feature Flags

A.R.E.S uses feature flags for conditional compilation. Understanding these is crucial for development:

### LLM Providers

| Feature | Description | Dependencies |
|---------|-------------|--------------|
| `ollama` | Ollama integration | `ollama-rs` |
| `openai` | OpenAI API support | `async-openai` |
| `llamacpp` | Direct GGUF loading | `llama-cpp-2` |
| `llamacpp-cuda` | LlamaCpp + CUDA | GPU drivers |
| `llamacpp-metal` | LlamaCpp + Metal | macOS only |

### Database Backends

| Feature | Description |
|---------|-------------|
| `local-db` | Local SQLite via libsql (default) |
| `turso` | Remote Turso database |
| `qdrant` | Qdrant vector database |

### Feature Bundles

| Feature | Includes |
|---------|----------|
| `all-llm` | ollama + openai + llamacpp |
| `all-db` | local-db + turso + qdrant |
| `full` | All optional features |
| `minimal` | No optional features |

### Working with Features

```bash
# Test with specific feature combination
cargo test --features "ollama,qdrant"

# Check that code compiles with minimal features
cargo check --features "minimal"

# Run clippy on all feature combinations
cargo clippy --all-features
```

## Making Changes

### Branch Naming

Use descriptive branch names:

- `feature/add-anthropic-provider`
- `fix/ollama-streaming-bug`
- `docs/update-readme`
- `refactor/llm-client-trait`

### Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer(s)]
```

Types:
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting, etc.)
- `refactor`: Code refactoring
- `test`: Adding or updating tests
- `chore`: Maintenance tasks

Examples:
```
feat(llm): add streaming support for LlamaCpp

Implements token-by-token streaming using tokio channels.
Resolves #123

fix(auth): handle expired refresh tokens correctly

test(api): add concurrent login tests
```

### Adding New Features

1. **Discuss First**: For significant changes, open an issue to discuss the approach
2. **Feature Gate**: Use Cargo features for optional functionality
3. **Document**: Update README and add doc comments
4. **Test**: Add unit and integration tests
5. **Example**: Consider adding usage examples

### Adding a New LLM Provider

1. Create `src/llm/your_provider.rs`
2. Implement the `LLMClient` trait
3. Add feature flag in `Cargo.toml`
4. Update `Provider` enum in `src/llm/client.rs`
5. Add tests
6. Document environment variables

### Adding a New Tool

1. Create `src/tools/your_tool.rs`
2. Implement the `Tool` trait
3. Register in the tool registry
4. Add tests
5. Document the tool's purpose and parameters

## Testing

### Running Tests

```bash
# Run all tests
cargo test

# Run with specific features
cargo test --features "ollama,openai"

# Run a specific test
cargo test test_name

# Run tests with output
cargo test -- --nocapture

# Run only integration tests
cargo test --test '*'

# Run only unit tests
cargo test --lib
```

### Test Coverage

```bash
# Install coverage tool
cargo install cargo-llvm-cov

# Generate HTML coverage report
cargo llvm-cov --html --open

# Generate lcov report
cargo llvm-cov --lcov --output-path lcov.info
```

### Writing Tests

- Place unit tests in the same file using `#[cfg(test)]` modules
- Place integration tests in the `tests/` directory
- Use `mockall` for mocking traits
- Use `wiremock` for HTTP mocking
- Use `tempfile` for temporary file/database tests

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

## Code Style

### Formatting

```bash
# Format code
cargo fmt

# Check formatting (CI will fail if not formatted)
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

- All public items should have doc comments
- Use `///` for item documentation
- Use `//!` for module-level documentation
- Include examples in doc comments when helpful

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
///     base_url: "http://localhost:11434".into(),
///     model: "llama3.2".into(),
/// }).await?;
/// ```
pub async fn create_client(provider: Provider) -> Result<Box<dyn LLMClient>> {
    // ...
}
```

## Pull Request Process

### Before Submitting

1. [ ] Rebase on latest `main`
2. [ ] Run `cargo fmt`
3. [ ] Run `cargo clippy --all-features`
4. [ ] Run `cargo test --all-features`
5. [ ] Update documentation if needed
6. [ ] Add/update tests for changes

### PR Description Template

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

### Review Process

1. Automated CI checks must pass
2. At least one maintainer approval required
3. Address review feedback
4. Squash commits if requested
5. Maintainer will merge when ready

## Release Process

Releases are managed by maintainers:

1. Update version in `Cargo.toml`
2. Update CHANGELOG.md
3. Create git tag: `git tag v0.x.y`
4. Push tag: `git push origin v0.x.y`
5. GitHub Actions will create release

### Versioning

We follow [Semantic Versioning](https://semver.org/):

- MAJOR: Breaking API changes
- MINOR: New features, backward compatible
- PATCH: Bug fixes, backward compatible

## Getting Help

- **Issues**: Search existing issues or create a new one
- **Discussions**: For questions and ideas
- **Discord**: [Link if available]

## Recognition

Contributors will be recognized in:
- CHANGELOG.md for their specific contributions
- README.md contributors section
- GitHub release notes

Thank you for contributing to A.R.E.S! 🚀
