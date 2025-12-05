# Testing Guide

This guide explains how to test the tool calling implementation in ARES.

## Prerequisites

Before running tests, ensure you have:

1. **Rust toolchain** installed (1.75+)
2. **Internet connectivity** for dependency downloads
3. **Ollama** (optional, for local LLM testing)

## Build Issues

If you encounter build errors related to `ort-sys` or `fastembed`, this is due to ONNX Runtime download issues. This is a transient network problem. To resolve:

1. Wait a few minutes and try again
2. Check your internet connectivity
3. Try setting `ORT_STRATEGY=system` environment variable

```bash
ORT_STRATEGY=system cargo build
```

## Running Tests

### All Tests

Run the complete test suite:

```bash
cargo test
```

### Specific Test Suites

Run only tool calling tests:

```bash
cargo test --test tool_calling_tests
```

Run only LLM tests:

```bash
cargo test --test llm_tests
```

Run only database tests:

```bash
cargo test --test db_tests
```

### Unit Tests Only

Run unit tests without integration tests:

```bash
cargo test --lib
```

### Specific Module Tests

Test the tool registry:

```bash
cargo test --lib tools::registry::tests
```

Test Ollama client:

```bash
cargo test --lib llm::ollama::tests
```

## Test Coverage

### New Tests Added

1. **Tool Registry Tests** (`src/tools/registry.rs`)
   - Registry creation and initialization
   - Default tools registration
   - Tool execution
   - Tool definition schema validation

2. **Ollama Tool Calling Tests** (`src/llm/ollama.rs`)
   - Tool definition conversion to Ollama format
   - URL parsing for Ollama client

3. **Integration Tests** (`tests/tool_calling_tests.rs`)
   - Tool registry with default tools
   - Calculator tool execution
   - Custom tool registration and execution
   - Tool definition serialization for LLM APIs
   - Schema validation for OpenAI compatibility

### Expected Test Results

When all tests pass, you should see output similar to:

```
running 23 tests
test tools::registry::tests::test_registry_creation ... ok
test tools::registry::tests::test_registry_with_default_tools ... ok
test tools::registry::tests::test_get_tool_definitions ... ok
test tools::registry::tests::test_calculator_execution ... ok
test tools::registry::tests::test_nonexistent_tool ... ok
test llm::ollama::tests::test_url_parsing_full ... ok
test llm::ollama::tests::test_url_parsing_no_port ... ok
test llm::ollama::tests::test_url_parsing_custom_port ... ok
test llm::ollama::tests::test_tool_definition_conversion ... ok
...

test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## Manual Testing

### Testing Tool Registry

Create a simple program to test the tool registry:

```rust
use ares::tools::ToolRegistry;
use serde_json::json;

#[tokio::main]
async fn main() {
    let registry = ToolRegistry::with_default_tools();
    
    println!("Available tools: {:?}", registry.tool_names());
    
    // Test calculator
    let result = registry.execute("calculator", json!({
        "operation": "add",
        "a": 5.0,
        "b": 3.0
    })).await.unwrap();
    
    println!("5 + 3 = {}", result["result"]);
}
```

### Testing Ollama Tool Calling

**Prerequisites**: Ollama must be running with a compatible model

```bash
# Start Ollama
ollama serve

# Pull a tool-calling compatible model
ollama pull llama3.1
```

Then create a test program:

```rust
use ares::llm::{LLMClient, Provider};
use ares::tools::ToolRegistry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = Provider::Ollama {
        base_url: "http://localhost:11434".to_string(),
        model: "llama3.1".to_string(),
    };
    
    let client = provider.create_client().await?;
    let registry = ToolRegistry::with_default_tools();
    let tools = registry.get_tool_definitions();
    
    println!("Testing with {} tools", tools.len());
    
    let response = client.generate_with_tools(
        "Calculate 42 plus 17",
        &tools
    ).await?;
    
    println!("Response: {}", response.content);
    println!("Tool calls: {}", response.tool_calls.len());
    
    for call in &response.tool_calls {
        println!("  - Tool: {}", call.name);
        let result = registry.execute(&call.name, call.arguments.clone()).await?;
        println!("  - Result: {:?}", result);
    }
    
    Ok(())
}
```

### Testing Without Ollama

You can test without Ollama by running only the unit tests:

```bash
# Run tests that don't require external services
cargo test --lib

# Run specific test files
cargo test --test tool_calling_tests -- --skip web_search --skip fetch_page
```

## Validation Checklist

- [ ] **Build succeeds**: `cargo build` completes without errors
- [ ] **Unit tests pass**: `cargo test --lib` all tests pass
- [ ] **Integration tests pass**: `cargo test --test tool_calling_tests` all tests pass
- [ ] **Tool registry works**: Default tools are registered correctly
- [ ] **Calculator tool works**: Can perform arithmetic operations
- [ ] **Ollama client builds**: `OllamaClient::new()` succeeds
- [ ] **Tool definitions are valid**: All tools have proper JSON schemas
- [ ] **Custom tools can be registered**: User can create and add custom tools

## Common Test Failures

### "Tool not found" Errors

If you see errors like "Tool not found: web_search", ensure you're using `ToolRegistry::with_default_tools()` instead of `ToolRegistry::new()`.

### Ollama Connection Errors

If Ollama tests fail with connection errors:

1. Check Ollama is running: `curl http://localhost:11434/api/tags`
2. Verify the port is correct (default: 11434)
3. Ensure a model is pulled: `ollama list`

### Build Errors with ort-sys

This is a known issue with ONNX Runtime downloads. Solutions:

1. Wait and retry - it's usually a transient network issue
2. Use a VPN or different network
3. Contact the maintainers if the issue persists

## Performance Tests

To verify tool calling doesn't add significant overhead:

```rust
use std::time::Instant;

#[tokio::main]
async fn main() {
    let registry = ToolRegistry::with_default_tools();
    
    let start = Instant::now();
    for _ in 0..1000 {
        let _ = registry.execute("calculator", json!({
            "operation": "add",
            "a": 1.0,
            "b": 1.0
        })).await;
    }
    let duration = start.elapsed();
    
    println!("1000 calculations in {:?}", duration);
    println!("Average: {:?} per call", duration / 1000);
}
```

Expected: < 1ms per calculator call on modern hardware.

## Test Coverage Report

To generate a coverage report:

```bash
cargo install cargo-llvm-cov
cargo llvm-cov --html --open
```

This will open a browser with detailed coverage information.

## CI/CD Testing

The GitHub Actions workflow (`.github/workflows/ci.yml`) runs:

1. `cargo check` on all targets
2. `rustfmt` formatting check
3. `clippy` lints
4. `cargo test` on Linux, macOS, and Windows
5. Code coverage generation

Check the Actions tab in GitHub to see test results.

## Debugging Failed Tests

For detailed test output:

```bash
# Show all output including println!
cargo test -- --nocapture

# Show test names as they run
cargo test -- --test-threads=1

# Run a specific test with output
cargo test test_calculator_execution -- --exact --nocapture
```

## Next Steps

Once all tests pass:

1. Review the test coverage report
2. Add integration tests with real Ollama models (if available)
3. Test with real web search queries (daedra integration)
4. Performance test with concurrent tool calls
5. Test error handling with invalid inputs
6. Test tool calling with different LLM providers (OpenAI, etc.)

## Questions or Issues?

If you encounter any issues:

1. Check this guide for common solutions
2. Review the logs carefully for error messages
3. Check the relevant documentation (README.md, TOOL_CALLING.md)
4. Create an issue with:
   - Rust version (`rustc --version`)
   - Operating system
   - Full error message
   - Steps to reproduce
