# Changelog

All notable changes to A.R.E.S will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.5] - 2024-12-21

### Changed

- **Swagger UI is now optional**: The interactive API documentation (Swagger UI) is now behind the `swagger-ui` feature flag
  - This reduces the default binary size and build time
  - The core server no longer requires network access during build
  - Enable with `cargo build --features swagger-ui` or use the `full` bundle
  - When enabled, Swagger UI is available at `/swagger-ui/`

- **Improved docs.rs compatibility**: Documentation builds now work on docs.rs
  - Removed problematic dependencies from docs.rs builds (`llamacpp`, `qdrant`, `swagger-ui`)
  - These features require native compilation or network access which docs.rs doesn't support

### Fixed

- **docs.rs build failures**: Fixed build failures caused by:
  - `utoipa-swagger-ui` requiring network access to download Swagger UI assets
  - `llama-cpp-sys-2` requiring native C++ compilation
  - `qdrant-client` build script requiring filesystem write access

## [0.2.4] - 2024-12-21

### Fixed

- **CI workflow**: Fixed rust-cache key validation errors caused by commas in feature matrix
- **Clippy errors**: Fixed various clippy warnings treated as errors in CI
- **Test compilation**: Fixed `ChatCompletionTools` enum pattern matching in OpenAI tests

## 0.2.3

### Added

- **CLI Commands**: Full-featured command-line interface with colored TUI output
  - `ares-server init` - Scaffold a new A.R.E.S project with all configuration files
  - `ares-server config` - View and validate configuration
  - `ares-server agent list` - List all configured agents
  - `ares-server agent show <name>` - Show details for a specific agent
  - Global options: `--config`, `--verbose`, `--no-color`
  - Init options: `--force`, `--minimal`, `--no-examples`, `--provider`, `--host`, `--port`

- **Embedded Web UI**: Leptos-based frontend that can be bundled with the backend
  - New `ui` feature flag to embed the UI in the server binary
  - New `full-ui` feature bundle (all features + UI)
  - UI served at `/` when enabled
  - SPA routing support for client-side navigation

- **Node.js Runtime Detection**: Build-time check for bun, npm, or deno when UI feature is enabled

- **CLI Integration Tests**: Comprehensive test suite for all CLI commands
  - Unit tests for output formatting
  - Unit tests for init scaffolding
  - Integration tests for command execution

### Changed

- **Installation Experience**: Users can now run `ares-server init` after installing via `cargo install`
  - No longer requires cloning the repository to get started
  - Auto-generates `ares.toml`, `.env.example`, and all TOON configuration files
  - Creates directory structure: `data/`, `config/agents/`, `config/models/`, etc.

- **Justfile**: Added new commands
  - `just init` - Initialize project using CLI
  - `just build-ui` - Build with embedded UI (auto-detects Node.js runtime)
  - `just build-full-ui` - Build with all features including UI
  - `just run-ui` - Run server with UI feature
  - `just check-node` - Check for available Node.js runtime

- **CI Workflow**: Updated to include CLI tests and UI builds
  - New `cli-tests` job for CLI integration tests
  - New `build-ui` job for UI feature compilation
  - Tests run on all supported platforms

- **Dockerfile**: Updated for new CLI and binary name
  - Multi-stage build with UI support
  - Non-root user for improved security
  - Proper binary name (`ares-server`)

- **Documentation**: Comprehensive updates
  - README.md: Added CLI commands, UI feature, troubleshooting, requirements sections
  - QUICK_REFERENCE.md: Added CLI quick reference
  - Added CHANGELOG.md

### Fixed

- Configuration loading no longer requires environment variables for info commands
  - `ares-server config` works without JWT_SECRET set
  - `ares-server agent list/show` works without environment variables

## [0.2.2] - 2024-12-20

### Added

- Hot-reload configuration support for `ares.toml`
- TOON format support for agent, model, tool, and workflow configurations
- Dynamic configuration manager for runtime config changes
- Per-agent tool filtering

### Changed

- Improved error messages for configuration validation
- Better handling of missing configuration files

## [0.2.1] - 2024-12-15

### Added

- Workflow engine for multi-agent orchestration
- Deep research endpoint with parallel subagents
- MCP (Model Context Protocol) server support

### Fixed

- Memory management for long conversations
- Token counting accuracy for streaming responses

## [0.2.0] - 2024-12-10

### Added

- Multi-provider LLM support (Ollama, OpenAI, LlamaCpp)
- Tool calling with automatic schema generation
- JWT-based authentication
- Swagger UI for API documentation
- RAG support with semantic search
- Web search tool (no API key required)

### Changed

- Migrated to Axum web framework
- Improved streaming response handling
- Better error handling and logging

## [0.1.0] - 2024-12-01

### Added

- Initial release
- Basic chat functionality with Ollama
- SQLite database support
- Simple agent framework
- REST API endpoints

---

[0.2.5]: https://github.com/dirmacs/ares/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/dirmacs/ares/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/dirmacs/ares/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/dirmacs/ares/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/dirmacs/ares/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/dirmacs/ares/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/dirmacs/ares/releases/tag/v0.1.0
