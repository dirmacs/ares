# Known issues

## OpenAI integration

Status: Compiles against async-openai 0.31.1; needs live API verification

Issue: The provider was updated to the 0.31.1 API (tool enums, tool list conversion, and tool-call parsing). Compile errors are resolved; runtime/tool-calling correctness still needs validation with a real OpenAI endpoint.

Impact: 
- Builds now succeed with the `openai` feature
- Tool calling should work, but has not been exercised against the real API
- Further adjustments may be needed after end-to-end testing

Workaround:
- Prefer Ollama or LlamaCpp for local-first workflows
- If using OpenAI, run targeted E2E tests with a real API key

Next Steps:
1. Run live tests with a real OpenAI key to validate tool calling and streaming
2. Add mocked/OpenAI-contract tests if feasible
3. Update docs with any model-specific nuances

## GPU backend compilation

Status: Requires platform-specific SDKs

Issue: Building with GPU features requires installed SDKs:
- `llamacpp-cuda`: Requires CUDA Toolkit
- `llamacpp-metal`: macOS only, requires Xcode
- `llamacpp-vulkan`: Requires Vulkan SDK

Impact:
- `--all-features` builds will fail without SDKs installed
- Per-platform builds work fine

Workaround:
```bash
# Use specific features instead of --all-features
cargo build --features "ollama,llamacpp,local-db"

# Or enable GPU only if SDK is installed
cargo build --features "llamacpp-cuda"  # if CUDA is available
```

**Documentation**: See `docs/GGUF_USAGE.md` for GPU setup instructions

## Test coverage

**Status**: Core features fully tested

**Details**:
- 277+ tests total (152 lib + 125 integration)
- Ollama: Full coverage with wiremock
- LlamaCpp: Needs integration tests with real GGUF models
- OpenAI: Tests disabled pending API fixes

**Recommendations**:
1. Add E2E tests with real Ollama instance in CI
2. Add LlamaCpp tests with tiny test model
3. Fix OpenAI and re-enable tests

## Windows-specific

**Status**: Works with minor notes

**Notes**:
- PowerShell script provided: `scripts/dev-setup.ps1`
- CUDA requires Visual Studio Build Tools
- Long path support may be needed for some GGUF models

**Fix**:
```powershell
# Enable long paths in Windows
Set-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem" -Name "LongPathsEnabled" -Value 1
```

## Docker compose

**Status**: Functional with notes

**Notes**:
- GPU passthrough requires `nvidia-docker2` on Linux
- Windows/Mac GPU support varies by Docker Desktop version
- Health checks may timeout on slow systems

**Workaround**:
```yaml
# Increase health check intervals in docker-compose.dev.yml
healthcheck:
  interval: 60s  # was 30s
  timeout: 20s   # was 10s
  start_period: 120s  # was 60s
```

## Memory usage

Status: Large models require significant RAM

Issue: 
- 7B Q4 models: ~6-8GB RAM
- 13B Q4 models: ~10-12GB RAM
- 70B Q4 models: ~40-50GB RAM

Workaround:
1. Use smaller models (1B-3B) for development
2. Use more aggressive quantization (Q3_K_M, Q4_0)
3. Reduce context size: `LLAMACPP_N_CTX=2048`
4. Enable GPU offloading to move memory to VRAM

## MCP integration

Status: Complete

Implementation: Full MCP (Model Context Protocol) server with tool support.

Files:
- `src/mcp/server.rs` - MCP server implementation
- 14+ tests for MCP functionality

Features:
- Tool registration and execution
- Protocol compliance
- Tested with complete test suite

## Performance notes

Not Issues, Just Notes:

1. First Request Slowness: Model loading can take 5-30 seconds on first request
2. Context Building: Large contexts (8K+) slow down generation significantly 
3. Concurrent Requests: CPU-only can handle 1-2 concurrent generations efficiently
4. Streaming Latency: First token can take 1-2 seconds to generate

---

## Reporting issues

If you encounter issues not listed here:

1. Check `CONTRIBUTING.md` for development setup
2. Verify environment variables in `.env`
3. Run with debug logging: `RUST_LOG=debug cargo run`
4. Search existing GitHub issues
5. Open a new issue with:
 - OS and Rust version
 - Feature flags used
 - Full error message
 - Steps to reproduce

## Fixed in this release

Turso cloud dependency removed (local-first by default) 
Qdrant cloud dependency removed (optional feature) 
Ollama tool calling implemented and tested 
LlamaCpp streaming working 
 175+ tests passing for core features 
CI/CD pipeline configured 
Documentation complete 
MCP server fully implemented 
RAG pipeline with pure-Rust vector store 
Rate limiting infrastructure 
Improved CORS configuration 
Vector persistence bug fixed - Vectors now properly saved to disk (commit 354a771) 
Race condition in parallel model loading fixed - Added per-model initialization locks (commit 354a771) 
Fuzzy search query typo correction - Query-level typo correction implemented (commit 1eda28b, closes #4)
Embedding cache implemented - In-memory LRU cache for embeddings (commit c6c25dd)

## Open issues

*No major open issues at this time.*

## Dependency security sweep (2026-08-22)

Status: All open Dependabot alerts on the default branch were triaged in one pass. Every high, critical, and medium severity alert is resolved in the current `Cargo.lock` and `ui/` lockfiles.

Resolved (verified against locked versions):
- openssl @ 0.10.81 (was 0.10.76; CVE-2026-41676/41678/41898/42327/41681/41677/45784/44662)
- rmcp @ 3.1.4 (was 0.12.0; DNS-rebinding in the Streamable HTTP server transport). ARES runs its MCP server on stdio only, so the vulnerable transport was never reachable, but the crate was migrated forward.
- quinn-proto @ 0.11.17, lettre @ 0.11.23, serde_with @ 3.22.0, rand @ 0.8.7/0.9.5, time @ 0.3.47, bytes @ 1.11.1, aws-lc-sys @ 0.39.1, lz4_flex @ 0.11.6
- rustls-webpki @ 0.103.15 (high-severity alert; the 0.101/0.102 lines below are the only residual)
- thrift: removed entirely from the lockfile. The vulnerable chain was ares-server -> lance (direct) -> datafusion 50 -> parquet 56 -> thrift 0.17. The direct `lance` dependency was a redundant artifact (no code imports `lance::`); removing it and upgrading `lancedb` to 0.37.1 eliminated the chain.
- postcss @ 8.5.26, nanoid @ 3.3.18 (ui), resolved via bun and npm lockfiles.

Residual (low severity, accepted; also the cause of the stale high-severity alert 28):
- rustls-webpki 0.101.7 and 0.102.8 (alerts 19/20, GHSA-82j2-j2ch-gfr8 / -xgp8-3hg3-c2mh / -965h-392x-2mh5). These legacy TLS lines are reachable only through optional features (`chromadb` -> `minreq` 2.x -> rustls 0.21; `libsql`/`turso` and gRPC clients -> `tonic` 0.11 -> hyper-rustls 0.25 -> rustls 0.22), none of which is in the production build (`postgres, openai, ares-vector, mcp, inventory`) or the verification matrix (`openai, postgres, mcp`). Both holdouts are at their maximum stable release: `chromadb 2.3.0` pins `minreq ^2` (rustls ^0.21) and `libsql 0.6.0` pins `tonic ^0.11` (rustls ^0.22). No stable upgrade path exists (libsql's next release is 0.10.0-pre). CRL revocation checking is opt-in in rustls-webpki; the default configuration is not affected. Alert 28 (high) stays listed because Dependabot evaluates the advisory across every version in the lockfile; the production-relevant `rustls-webpki 0.103.15` (used by reqwest 0.12) is fixed.

Note on the root-crate integration tests: `cargo test` on `ares-server` (tests/api_tests.rs, tests/integration_toml_tests.rs, tests/v1_tenant_agent_runtime_tests.rs) fails to compile because those files still construct the pre-redesign `AppState`-style `Context`/`AresConfig` literals. This predates the Cordis `Context` redesign (the files were last edited in commit 58ea725) and is unrelated to this dependency sweep. The package test suites (cordis 46, ares-llm 237, ares-mcp 325, ares-store 49) and `cargo check`/`cargo clippy -D warnings` for both feature configs all pass.

---

Last Updated: 2026-02-01 
Version: 0.5.0