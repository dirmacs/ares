# Known Issues

## OpenAI Integration

**Status**: ✅ Compiles against async-openai 0.31.1; needs live API verification

**Issue**: The provider was updated to the 0.31.1 API (tool enums, tool list conversion, and tool-call parsing). Compile errors are resolved; runtime/tool-calling correctness still needs validation with a real OpenAI endpoint.

**Impact**: 
- Builds now succeed with the `openai` feature
- Tool calling should work, but has not been exercised against the real API
- Further adjustments may be needed after end-to-end testing

**Workaround**:
- Prefer Ollama or LlamaCpp for local-first workflows
- If using OpenAI, run targeted E2E tests with a real API key

**Next Steps**:
1. Run live tests with a real OpenAI key to validate tool calling and streaming
2. Add mocked/OpenAI-contract tests if feasible
3. Update docs with any model-specific nuances

## GPU Backend Compilation

**Status**: ⚠️ Requires platform-specific SDKs

**Issue**: Building with GPU features requires installed SDKs:
- `llamacpp-cuda`: Requires CUDA Toolkit
- `llamacpp-metal`: macOS only, requires Xcode
- `llamacpp-vulkan`: Requires Vulkan SDK

**Impact**:
- `--all-features` builds will fail without SDKs installed
- Per-platform builds work fine

**Workaround**:
```bash
# Use specific features instead of --all-features
cargo build --features "ollama,llamacpp,local-db"

# Or enable GPU only if SDK is installed
cargo build --features "llamacpp-cuda"  # if CUDA is available
```

**Documentation**: See `docs/GGUF_USAGE.md` for GPU setup instructions

## Test Coverage

**Status**: ✅ Core features fully tested

**Details**:
- 72/72 tests passing for default features
- Ollama: Full coverage with wiremock
- LlamaCpp: Needs integration tests with real GGUF models
- OpenAI: Tests disabled pending API fixes

**Recommendations**:
1. Add E2E tests with real Ollama instance in CI
2. Add LlamaCpp tests with tiny test model
3. Fix OpenAI and re-enable tests

## Windows-Specific

**Status**: ✅ Works with minor notes

**Notes**:
- PowerShell script provided: `scripts/dev-setup.ps1`
- CUDA requires Visual Studio Build Tools
- Long path support may be needed for some GGUF models

**Fix**:
```powershell
# Enable long paths in Windows
Set-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem" -Name "LongPathsEnabled" -Value 1
```

## Docker Compose

**Status**: ✅ Functional with notes

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

## Memory Usage

**Status**: ⚠️ Large models require significant RAM

**Issue**: 
- 7B Q4 models: ~6-8GB RAM
- 13B Q4 models: ~10-12GB RAM
- 70B Q4 models: ~40-50GB RAM

**Workaround**:
1. Use smaller models (1B-3B) for development
2. Use more aggressive quantization (Q3_K_M, Q4_0)
3. Reduce context size: `LLAMACPP_N_CTX=2048`
4. Enable GPU offloading to move memory to VRAM

## MCP Integration

**Status**: 🚧 Incomplete

**Issue**: MCP feature flag exists but implementation is incomplete

**Impact**: Feature compiles but has no functional endpoints

**Priority**: Low (not part of current release scope)

**Plan**: Complete in future release

## Performance Notes

**Not Issues, Just Notes**:

1. **First Request Slowness**: Model loading can take 5-30 seconds on first request
2. **Context Building**: Large contexts (8K+) slow down generation significantly  
3. **Concurrent Requests**: CPU-only can handle 1-2 concurrent generations efficiently
4. **Streaming Latency**: First token can take 1-2 seconds to generate

---

## Reporting Issues

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

## Fixed in This Release

✅ Turso cloud dependency removed (local-first by default)  
✅ Qdrant cloud dependency removed (optional feature)  
✅ Ollama tool calling implemented and tested  
✅ LlamaCpp streaming working  
✅ 72 tests passing for core features  
✅ CI/CD pipeline configured  
✅ Documentation complete  

---

**Last Updated**: 2024-12-06  
**Version**: 0.1.1