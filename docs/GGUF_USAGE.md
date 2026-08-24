# GGUF model usage guide

This guide describes how to run GGUF models locally and offline with A.R.E.S through the LlamaCpp integration.

## What is GGUF?

GGUF (GPT-Generated Unified Format) is a file format that stores models for inference with llama.cpp. Its design goals:
- Fast: optimized for CPU inference
- Flexible: supports 4-bit, 5-bit, and 8-bit quantization
- Portable: single-file format, easy to distribute
- Efficient: lower memory use than full-precision models

## Quick start

### 1. Enable the LlamaCpp feature

Build A.R.E.S with LlamaCpp support:

```bash
# CPU-only
cargo build --features "llamacpp"

# With NVIDIA GPU (CUDA)
cargo build --features "llamacpp-cuda"

# With Apple Silicon GPU (Metal)
cargo build --features "llamacpp-metal"

# With Vulkan GPU
cargo build --features "llamacpp-vulkan"
```

### 2. Download a GGUF model

Choose a model from Hugging Face. Recommended options:

#### Small models (good for testing, under 4 GB RAM)

```bash
# Llama 3.2 1B (Fastest, minimal resources)
wget https://huggingface.co/bartowski/Llama-3.2-1B-Instruct-GGUF/resolve/main/Llama-3.2-1B-Instruct-Q4_K_M.gguf

# Phi-3 Mini 3.8B (High quality for size)
wget https://huggingface.co/bartowski/Phi-3-mini-4k-instruct-GGUF/resolve/main/Phi-3-mini-4k-instruct-Q4_K_M.gguf

# Qwen 2.5 1.5B (Multilingual)
wget https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf
```

#### Medium models (8-16 GB RAM)

```bash
# Llama 3.2 3B (Great balance)
wget https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf

# Mistral 7B (Excellent performance)
wget https://huggingface.co/TheBloke/Mistral-7B-Instruct-v0.2-GGUF/resolve/main/mistral-7b-instruct-v0.2.Q4_K_M.gguf

# Llama 3.1 8B (Latest, best quality)
wget https://huggingface.co/bartowski/Meta-Llama-3.1-8B-Instruct-GGUF/resolve/main/Meta-Llama-3.1-8B-Instruct-Q4_K_M.gguf
```

#### Large models (32 GB+ RAM or GPU)

```bash
# Llama 3.1 70B (Highest quality)
wget https://huggingface.co/bartowski/Meta-Llama-3.1-70B-Instruct-GGUF/resolve/main/Meta-Llama-3.1-70B-Instruct-IQ3_M.gguf

# Qwen 2.5 72B (Strong multilingual)
wget https://huggingface.co/Qwen/Qwen2.5-72B-Instruct-GGUF/resolve/main/qwen2.5-72b-instruct-q4_k_m.gguf
```

### 3. Configure environment

Set the model path in your `.env` file:

```bash
# LlamaCpp takes priority over other providers when set
LLAMACPP_MODEL_PATH=/path/to/your/model.gguf

# Optional: Customize context size (default: 4096)
LLAMACPP_N_CTX=8192

# Optional: Number of CPU threads (default: 4)
LLAMACPP_N_THREADS=8

# Optional: Max tokens to generate (default: 512)
LLAMACPP_MAX_TOKENS=1024
```

### 4. Run A.R.E.S

```bash
cargo run --features "llamacpp"
```

The server selects the LlamaCpp provider automatically when `LLAMACPP_MODEL_PATH` is set.

## Quantization formats

GGUF models come in different quantization levels. The levels mean:

| Format | Size | Quality | Speed | Use Case |
|--------|------|---------|-------|----------|
| Q2_K | Smallest | Low | Fastest | Testing only |
| Q3_K_S | Very Small | Fair | Very Fast | Resource-constrained |
| Q4_0 | Small | Good | Fast | Balanced (recommended) |
| Q4_K_M | Small | Good+ | Fast | **Best for most users** |
| Q5_K_M | Medium | Very Good | Medium | Better quality |
| Q6_K | Large | Excellent | Slower | Near full quality |
| Q8_0 | Very Large | Excellent+ | Slow | Maximum quality |
| F16 | Huge | Perfect | Slowest | Original quality |

Start with `Q4_K_M`. It offers the best balance of quality, speed, and size.

## Hardware requirements

### CPU inference

| Model Size | RAM Required | CPU Threads | Tokens/sec (approx) |
|------------|--------------|-------------|---------------------|
| 1B (Q4) | 2-3 GB | 4 | 40-60 |
| 3B (Q4) | 4-6 GB | 4-8 | 20-30 |
| 7B (Q4) | 6-8 GB | 8 | 10-15 |
| 13B (Q4) | 10-12 GB | 8-16 | 5-8 |
| 70B (Q4) | 40-50 GB | 16+ | 1-3 |

### GPU acceleration

GPU acceleration greatly improves performance:

```bash
# CUDA (NVIDIA)
cargo build --features "llamacpp-cuda"

# Metal (Apple Silicon)
cargo build --features "llamacpp-metal"

# Vulkan (Cross-platform)
cargo build --features "llamacpp-vulkan"
```

Typical gains on modern GPUs:
- 7B model: 50-100 tokens/sec
- 13B model: 30-60 tokens/sec
- 70B model: 10-20 tokens/sec (needs 48 GB+ VRAM)

## Programmatic usage

### Basic generation

```rust
use ares_llm::{LLMClient, Provider};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create provider
    let provider = Provider::LlamaCpp {
        model_path: "/path/to/model.gguf".to_string(),
    };

    // Create client
    let client = provider.create_client().await?;

    // Generate response
    let response = client.generate("What is Rust?").await?;
    println!("Response: {}", response);
    
    Ok(())
}
```

### Streaming generation

```rust
use ares_llm::{LLMClient, Provider};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = Provider::LlamaCpp {
        model_path: "/path/to/model.gguf".to_string(),
    };
    
    let client = provider.create_client().await?;
    
    // Stream response token by token
    let mut stream = client.stream("Explain quantum computing").await?;
    
    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(text) => print!("{}", text),
            Err(e) => eprintln!("Error: {}", e),
        }
    }
    
    Ok(())
}
```

### With system prompt

```rust
let response = client
    .generate_with_system(
        "You are a helpful Rust programming assistant.",
        "How do I create a HashMap?",
    )
    .await?;
```

### With conversation history

```rust
let history = vec![
    ("user".to_string(), "What is 2+2?".to_string()),
    ("assistant".to_string(), "2+2 equals 4.".to_string()),
    ("user".to_string(), "What about 3+3?".to_string()),
];

let response = client.generate_with_history(&history).await?;
```

### Custom parameters

```rust
use ares_llm::LlamaCppClient;

// Create client with custom parameters
let client = LlamaCppClient::with_config_params(
    "/path/to/model.gguf".to_string(),
    8192,  // context size
    8,     // threads
    1024,  // max tokens
    0.7,   // temperature
    0.9,   // top_p
)?;
```

## Tool calling with GGUF models

Tool calling requires models trained for function calling (for example Llama 3.1+ or Mistral tool models).

The LlamaCpp client has basic tool calling support today. Ollama has more mature tool calling. Prefer it for production workloads.

### Basic tool support

```rust
use ares_llm::{LLMClient, Provider};
use ares_types::types::ToolDefinition;
use serde_json::json;

let provider = Provider::LlamaCpp {
    model_path: "/path/to/qwen3-vl-2b.gguf".to_string(),
};

let client = provider.create_client().await?;

let tools = vec![
    ToolDefinition {
        name: "calculator".to_string(),
        description: "Performs arithmetic operations".to_string(),
        parameters: json!({
            "type": "object",
            "properties": {
                "operation": {"type": "string"},
                "a": {"type": "number"},
                "b": {"type": "number"}
            },
            "required": ["operation", "a", "b"]
        }),
    }
];

let response = client
    .generate_with_tools("What is 5 + 3?", &tools)
    .await?;

if !response.tool_calls.is_empty() {
    println!("Tool called: {}", response.tool_calls[0].name);
    println!("Arguments: {}", response.tool_calls[0].arguments);
}
```

## Performance optimization

### 1. Adjust Context size

A larger context needs more memory and runs slower:

```bash
# Reduce for faster inference
LLAMACPP_N_CTX=2048

# Increase for longer conversations
LLAMACPP_N_CTX=8192
```

### 2. Thread count

Match the thread count to your CPU cores:

```bash
# Check cores
lscpu | grep "^CPU(s):"

# Set threads (leave 1-2 cores for system)
LLAMACPP_N_THREADS=6
```

### 3. Batch size

Adjust batch processing in code for production workloads:

```rust
// Larger batches = faster throughput, more memory
let mut client = LlamaCppClient::with_config_params(
    model_path,
    4096,  // ctx
    8,     // threads
    512,   // max_tokens
    0.7,   // temperature
    0.9,   // top_p
)?;
```

### 4. Model selection

Choose the quantization level:
- Development: `Q4_K_M`
- Production with quality focus: `Q5_K_M` or `Q6_K`
- Production with speed focus: `Q4_0` or `Q3_K_M`

## Troubleshooting

### Error: "failed to load model"

Check the file path and make sure that the GGUF file is valid:

```bash
file /path/to/model.gguf
# Should show: "GGUF model file"
```

### Error: "out of memory"

Fixes:
1. Use a smaller model (1B or 3B)
2. Choose stronger quantization (`Q3_K` or `Q4_0`)
3. Reduce context size: `LLAMACPP_N_CTX=2048`
4. Close other applications

### Slow inference

Fixes:
1. Increase threads: `LLAMACPP_N_THREADS=8`
2. Enable GPU acceleration (CUDA, Metal, or Vulkan)
3. Use a smaller model
4. Choose stronger quantization
5. Reduce max tokens: `LLAMACPP_MAX_TOKENS=256`

### Model ignores instructions

The model output drifts from the request. Fixes:
1. Use instruction-tuned models (the `-Instruct` variants)
2. Choose higher-quality quantization (`Q5_K_M` or `Q6_K`)
3. Adjust the system prompt
4. Try a different model architecture

## Best practices

### 1. Model selection
- Chat: `-Instruct` or `-Chat` models
- Code: CodeLlama or Qwen-Coder models
- Speed: 1B-3B models
- Quality: 7B-13B models

### 2. Memory management
- Load the model once and reuse the client
- Track RAM use with `htop` or Task Manager
- Avoid loading multiple large models at the same time

### 3. Context window
- Do not spend context on repetitive content
- Summarize long conversations at intervals
- Select a context size that fits the workload

### 4. Production deployment
- Pre-download models during the container build
- Balance quality and size with `Q4_K_M` or `Q5_K_M`
- Enable GPU acceleration where available
- Set token limits to prevent abuse

## Recommended models by use case

### General chat
- Llama 3.2 3B Instruct (best for most cases)
- Mistral 7B Instruct (high quality)
- Phi-3 Mini (efficient)

### Code generation
- CodeLlama 7B Instruct
- Qwen 2.5 Coder 7B
- DeepSeek Coder 6.7B

### Multilingual
- Qwen 2.5 (any size)
- Llama 3.1 (8B+)

### Creative writing
- Llama 3.1 70B (if resources allow)
- Mistral 7B
- Llama 3.2 3B

### Fast responses
- Llama 3.2 1B
- Phi-3 Mini
- TinyLlama 1.1B

## Resources

- [Hugging Face GGUF Models](https://huggingface.co/models?library=gguf)
- [llama.cpp GitHub](https://github.com/ggerganov/llama.cpp)
- [GGUF Spec](https://github.com/ggerganov/ggml/blob/master/docs/gguf.md)
- [Quantization Guide](https://github.com/ggerganov/llama.cpp/blob/master/examples/quantize/README.md)

## Example: complete setup

This complete example starts you with a 3B model:

```bash
# 1. Download model
cd models/
wget https://huggingface.co/bartowski/Llama-3.2-3B-Instruct-GGUF/resolve/main/Llama-3.2-3B-Instruct-Q4_K_M.gguf

# 2. Configure
cat > .env << EOF
LLAMACPP_MODEL_PATH=./models/Llama-3.2-3B-Instruct-Q4_K_M.gguf
LLAMACPP_N_CTX=4096
LLAMACPP_N_THREADS=4
LLAMACPP_MAX_TOKENS=512
EOF

# 3. Build and run
cargo build --release --features "llamacpp"
cargo run --release --features "llamacpp"
```

The A.R.E.S server now runs fully local, offline LLM inference.