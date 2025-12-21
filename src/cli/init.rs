//! Init command implementation
//!
//! Scaffolds a new A.R.E.S project with all necessary configuration files.

use super::output::Output;
use std::fs;
use std::path::Path;

/// Result of the init operation
pub enum InitResult {
    /// Initialization completed successfully
    Success,
    /// Project already exists (ares.toml found)
    AlreadyExists,
    /// An error occurred during initialization
    Error(String),
}

/// Configuration for the init command
pub struct InitConfig {
    /// Directory to initialize
    pub path: std::path::PathBuf,
    /// Overwrite existing files
    pub force: bool,
    /// Create minimal configuration
    pub minimal: bool,
    /// Skip creating example TOON files
    pub no_examples: bool,
    /// LLM provider to configure (ollama, openai, or both)
    pub provider: String,
    /// Host address for the server
    pub host: String,
    /// Port for the server
    pub port: u16,
}

/// Run the init command
pub fn run(config: InitConfig, output: &Output) -> InitResult {
    output.banner();
    output.header("Initializing A.R.E.S Project");

    let base_path = &config.path;

    // Check if ares.toml already exists
    let config_path = base_path.join("ares.toml");
    if config_path.exists() && !config.force {
        output.warning("ares.toml already exists!");
        output.hint("Use --force to overwrite existing files");
        return InitResult::AlreadyExists;
    }

    // Create directories
    output.subheader("Creating directories");

    let directories = [
        "data",
        "config",
        "config/agents",
        "config/models",
        "config/tools",
        "config/workflows",
        "config/mcps",
    ];

    for dir in &directories {
        let dir_path = base_path.join(dir);
        if !dir_path.exists() {
            if let Err(e) = fs::create_dir_all(&dir_path) {
                output.error(&format!("Failed to create {}: {}", dir, e));
                return InitResult::Error(e.to_string());
            }
            output.created_dir(dir);
        } else {
            output.skipped(dir, "already exists");
        }
    }

    // Create ares.toml
    output.subheader("Creating configuration files");

    let toml_content = generate_ares_toml(&config);
    if let Err(e) = write_file(&config_path, &toml_content, config.force) {
        output.error(&format!("Failed to create ares.toml: {}", e));
        return InitResult::Error(e.to_string());
    }
    output.created("config", "ares.toml");

    // Create .env.example
    let env_example_path = base_path.join(".env.example");
    let env_content = generate_env_example();
    if let Err(e) = write_file(&env_example_path, &env_content, config.force) {
        output.error(&format!("Failed to create .env.example: {}", e));
        return InitResult::Error(e.to_string());
    }
    output.created("env", ".env.example");

    // Create TOON files if not --no-examples
    if !config.no_examples {
        output.subheader("Creating example configurations");

        // Models
        create_model_files(base_path, &config, output);

        // Agents
        create_agent_files(base_path, &config, output);

        // Tools
        create_tool_files(base_path, output);

        // Workflows
        create_workflow_files(base_path, output);
    }

    // Create .gitignore if it doesn't exist
    let gitignore_path = base_path.join(".gitignore");
    if !gitignore_path.exists() {
        let gitignore_content = generate_gitignore();
        if let Err(e) = write_file(&gitignore_path, &gitignore_content, false) {
            output.warning(&format!("Failed to create .gitignore: {}", e));
        } else {
            output.created("file", ".gitignore");
        }
    }

    // Print completion message and next steps
    output.complete("A.R.E.S project initialized successfully!");

    output.header("Next Steps");
    output.newline();
    output.info("1. Set up environment variables:");
    output.command("cp .env.example .env");
    output.command("# Edit .env and set JWT_SECRET (min 32 chars) and API_KEY");
    output.newline();

    if config.provider == "ollama" {
        output.info("2. Start Ollama (if not running):");
        output.command("ollama serve");
        output.command("ollama pull ministral-3:3b  # or your preferred model");
        output.newline();
    }

    output.info("3. Start the server:");
    output.command("ares-server");
    output.newline();

    output.hint(&format!(
        "Server will be available at http://{}:{}",
        config.host, config.port
    ));
    output.hint("API docs available at /swagger-ui/ (requires 'swagger-ui' feature)");
    output.hint("Build with: cargo build --features swagger-ui");

    InitResult::Success
}

fn write_file(path: &Path, content: &str, force: bool) -> std::io::Result<()> {
    if path.exists() && !force {
        return Ok(()); // Skip existing files unless force is true
    }
    fs::write(path, content)
}

fn generate_ares_toml(config: &InitConfig) -> String {
    let provider_section = if config.provider == "openai" {
        r#"# OpenAI API (set OPENAI_API_KEY in .env)
[providers.openai]
type = "openai"
api_key_env = "OPENAI_API_KEY"
api_base = "https://api.openai.com/v1"
default_model = "gpt-4o-mini"
"#
    } else if config.provider == "both" {
        r#"# Ollama - Local inference (default)
[providers.ollama-local]
type = "ollama"
base_url = "http://localhost:11434"
default_model = "ministral-3:3b"

# OpenAI API (set OPENAI_API_KEY in .env)
[providers.openai]
type = "openai"
api_key_env = "OPENAI_API_KEY"
api_base = "https://api.openai.com/v1"
default_model = "gpt-4o-mini"
"#
    } else {
        // Default to ollama
        r#"# Ollama - Local inference (no API key required)
[providers.ollama-local]
type = "ollama"
base_url = "http://localhost:11434"
default_model = "ministral-3:3b"
"#
    };

    let model_provider = if config.provider == "openai" {
        "openai"
    } else {
        "ollama-local"
    };

    let model_name = if config.provider == "openai" {
        "gpt-4o-mini"
    } else {
        "ministral-3:3b"
    };

    format!(
        r#"# A.R.E.S Configuration
# =====================
# Generated by: ares-server init
#
# REQUIRED: Set these environment variables before starting:
#   - JWT_SECRET: A secret key for JWT signing (min 32 characters)
#   - API_KEY: API key for service-to-service authentication
#
# Hot Reloading: Changes to this file are automatically detected and applied
# without restarting the server.

# =============================================================================
# Server Configuration
# =============================================================================
[server]
host = "{host}"
port = {port}
log_level = "info"

# =============================================================================
# Authentication Configuration
# =============================================================================
[auth]
jwt_secret_env = "JWT_SECRET"
jwt_access_expiry = 900
jwt_refresh_expiry = 604800
api_key_env = "API_KEY"

# =============================================================================
# Database Configuration
# =============================================================================
[database]
url = "./data/ares.db"

# =============================================================================
# LLM Providers
# =============================================================================
{provider_section}
# =============================================================================
# Model Configurations
# =============================================================================
[models.fast]
provider = "{model_provider}"
model = "{model_name}"
temperature = 0.7
max_tokens = 256

[models.balanced]
provider = "{model_provider}"
model = "{model_name}"
temperature = 0.7
max_tokens = 512

[models.powerful]
provider = "{model_provider}"
model = "{model_name}"
temperature = 0.5
max_tokens = 1024

# =============================================================================
# Tools Configuration
# =============================================================================
[tools.calculator]
enabled = true
description = "Performs basic arithmetic operations (+, -, *, /)"
timeout_secs = 10

[tools.web_search]
enabled = true
description = "Search the web using DuckDuckGo (no API key required)"
timeout_secs = 30

# =============================================================================
# Agent Configurations
# =============================================================================
[agents.router]
model = "fast"
tools = []
max_tool_iterations = 1
parallel_tools = false
system_prompt = """
You are a routing agent that classifies user queries.

Available agents:
- orchestrator: General purpose agent for complex queries

Respond with ONLY the agent name (one word, lowercase).
"""

[agents.orchestrator]
model = "powerful"
tools = ["calculator", "web_search"]
max_tool_iterations = 10
parallel_tools = false
system_prompt = """
You are an orchestrator agent for complex queries.

Capabilities:
- Break down complex requests
- Perform web searches
- Execute calculations
- Provide comprehensive answers

Be helpful, accurate, and thorough.
"""

# =============================================================================
# Workflow Configurations
# =============================================================================
[workflows.default]
entry_agent = "router"
fallback_agent = "orchestrator"
max_depth = 3
max_iterations = 5

# =============================================================================
# RAG Configuration
# =============================================================================
[rag]
embedding_model = "BAAI/bge-small-en-v1.5"
chunk_size = 1000
chunk_overlap = 200

# =============================================================================
# Dynamic Configuration Paths (TOON Files)
# =============================================================================
[config]
agents_dir = "config/agents"
workflows_dir = "config/workflows"
models_dir = "config/models"
tools_dir = "config/tools"
mcps_dir = "config/mcps"
hot_reload = true
watch_interval_ms = 1000
"#,
        host = config.host,
        port = config.port,
        provider_section = provider_section,
        model_provider = model_provider,
        model_name = model_name,
    )
}

fn generate_env_example() -> String {
    r#"# A.R.E.S Environment Variables
# =============================
# Copy this file to .env and fill in the values.

# REQUIRED: JWT secret for authentication (minimum 32 characters)
# Generate with: openssl rand -base64 32
JWT_SECRET=change-me-in-production-use-at-least-32-characters

# REQUIRED: API key for service-to-service authentication
API_KEY=your-api-key-here

# Optional: Logging level (trace, debug, info, warn, error)
RUST_LOG=info,ares=debug

# Optional: OpenAI API key (if using OpenAI provider)
# OPENAI_API_KEY=sk-...

# Optional: Turso cloud database (if using remote database)
# TURSO_URL=libsql://your-db.turso.io
# TURSO_AUTH_TOKEN=your-token

# Optional: Qdrant vector database
# QDRANT_URL=http://localhost:6334
# QDRANT_API_KEY=your-key
"#
    .to_string()
}

fn generate_gitignore() -> String {
    r#"# A.R.E.S Generated Files
/data/
*.db
*.db-journal

# Environment
.env
.env.local
.env.*.local

# Rust
/target/
Cargo.lock

# IDE
.idea/
.vscode/
*.swp
*.swo
*~

# OS
.DS_Store
Thumbs.db
"#
    .to_string()
}

fn create_model_files(base_path: &Path, config: &InitConfig, output: &Output) {
    let model_provider = if config.provider == "openai" {
        "openai"
    } else {
        "ollama-local"
    };

    let model_name = if config.provider == "openai" {
        "gpt-4o-mini"
    } else {
        "ministral-3:3b"
    };

    let models = [
        (
            "fast.toon",
            format!(
                r#"name: fast
provider: {provider}
model: {model}
temperature: 0.7
max_tokens: 256
"#,
                provider = model_provider,
                model = model_name
            ),
        ),
        (
            "balanced.toon",
            format!(
                r#"name: balanced
provider: {provider}
model: {model}
temperature: 0.7
max_tokens: 512
"#,
                provider = model_provider,
                model = model_name
            ),
        ),
        (
            "powerful.toon",
            format!(
                r#"name: powerful
provider: {provider}
model: {model}
temperature: 0.5
max_tokens: 1024
"#,
                provider = model_provider,
                model = model_name
            ),
        ),
    ];

    for (filename, content) in &models {
        let path = base_path.join("config/models").join(filename);
        if let Err(e) = write_file(&path, content, config.force) {
            output.warning(&format!("Failed to create {}: {}", filename, e));
        } else {
            output.created("model", &format!("config/models/{}", filename));
        }
    }
}

fn create_agent_files(base_path: &Path, config: &InitConfig, output: &Output) {
    let agents = [
        (
            "router.toon",
            r#"name: router
model: fast
max_tool_iterations: 1
parallel_tools: false
tools[0]:
system_prompt: "You are a routing agent that classifies user queries and routes them to the appropriate specialized agent.\n\nAvailable agents:\n- orchestrator: Complex queries requiring multiple steps or research\n\nAnalyze the user's query and respond with ONLY the agent name (lowercase, one word)."
"#.to_string(),
        ),
        (
            "orchestrator.toon",
            r#"name: orchestrator
model: powerful
max_tool_iterations: 10
parallel_tools: false
tools[0]: calculator
tools[1]: web_search
system_prompt: "You are an orchestrator agent for complex queries.\n\nCapabilities:\n- Break down complex requests\n- Perform web searches\n- Execute calculations\n- Synthesize information\n\nProvide comprehensive, well-structured answers."
"#.to_string(),
        ),
    ];

    for (filename, content) in &agents {
        let path = base_path.join("config/agents").join(filename);
        if let Err(e) = write_file(&path, content, config.force) {
            output.warning(&format!("Failed to create {}: {}", filename, e));
        } else {
            output.created("agent", &format!("config/agents/{}", filename));
        }
    }
}

fn create_tool_files(base_path: &Path, output: &Output) {
    let tools = [
        (
            "calculator.toon",
            r#"name: calculator
enabled: true
description: Performs basic arithmetic operations (+, -, *, /)
timeout_secs: 10
"#,
        ),
        (
            "web_search.toon",
            r#"name: web_search
enabled: true
description: Search the web using DuckDuckGo (no API key required)
timeout_secs: 30
"#,
        ),
    ];

    for (filename, content) in &tools {
        let path = base_path.join("config/tools").join(filename);
        if let Err(e) = write_file(&path, content, false) {
            output.warning(&format!("Failed to create {}: {}", filename, e));
        } else {
            output.created("tool", &format!("config/tools/{}", filename));
        }
    }
}

fn create_workflow_files(base_path: &Path, output: &Output) {
    let workflows = [
        (
            "default.toon",
            r#"name: default
entry_agent: router
fallback_agent: orchestrator
max_depth: 3
max_iterations: 5
parallel_subagents: false
"#,
        ),
        (
            "research.toon",
            r#"name: research
entry_agent: orchestrator
max_depth: 3
max_iterations: 10
parallel_subagents: true
"#,
        ),
    ];

    for (filename, content) in &workflows {
        let path = base_path.join("config/workflows").join(filename);
        if let Err(e) = write_file(&path, content, false) {
            output.warning(&format!("Failed to create {}: {}", filename, e));
        } else {
            output.created("workflow", &format!("config/workflows/{}", filename));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_config(temp_dir: &TempDir) -> InitConfig {
        InitConfig {
            path: temp_dir.path().to_path_buf(),
            force: false,
            minimal: false,
            no_examples: false,
            provider: "ollama".to_string(),
            host: "127.0.0.1".to_string(),
            port: 3000,
        }
    }

    #[test]
    fn test_init_config_creation() {
        let config = InitConfig {
            path: std::path::PathBuf::from("/tmp/test"),
            force: false,
            minimal: false,
            no_examples: false,
            provider: "ollama".to_string(),
            host: "127.0.0.1".to_string(),
            port: 3000,
        };

        assert_eq!(config.path, std::path::PathBuf::from("/tmp/test"));
        assert!(!config.force);
        assert!(!config.minimal);
        assert_eq!(config.provider, "ollama");
        assert_eq!(config.port, 3000);
    }

    #[test]
    fn test_init_result_variants() {
        // Verify enum variants exist and can be matched
        let success = InitResult::Success;
        let exists = InitResult::AlreadyExists;
        let error = InitResult::Error("test error".to_string());

        match success {
            InitResult::Success => (),
            _ => panic!("Expected Success"),
        }

        match exists {
            InitResult::AlreadyExists => (),
            _ => panic!("Expected AlreadyExists"),
        }

        match error {
            InitResult::Error(msg) => assert_eq!(msg, "test error"),
            _ => panic!("Expected Error"),
        }
    }

    #[test]
    fn test_generate_ares_toml_ollama() {
        let config = InitConfig {
            path: std::path::PathBuf::from("/tmp"),
            force: false,
            minimal: false,
            no_examples: false,
            provider: "ollama".to_string(),
            host: "127.0.0.1".to_string(),
            port: 3000,
        };

        let content = generate_ares_toml(&config);

        assert!(content.contains("[server]"));
        assert!(content.contains("host = \"127.0.0.1\""));
        assert!(content.contains("port = 3000"));
        assert!(content.contains("[providers.ollama-local]"));
        assert!(content.contains("type = \"ollama\""));
    }

    #[test]
    fn test_generate_ares_toml_openai() {
        let config = InitConfig {
            path: std::path::PathBuf::from("/tmp"),
            force: false,
            minimal: false,
            no_examples: false,
            provider: "openai".to_string(),
            host: "0.0.0.0".to_string(),
            port: 8080,
        };

        let content = generate_ares_toml(&config);

        assert!(content.contains("host = \"0.0.0.0\""));
        assert!(content.contains("port = 8080"));
        assert!(content.contains("[providers.openai]"));
        assert!(content.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn test_generate_ares_toml_both() {
        let config = InitConfig {
            path: std::path::PathBuf::from("/tmp"),
            force: false,
            minimal: false,
            no_examples: false,
            provider: "both".to_string(),
            host: "127.0.0.1".to_string(),
            port: 3000,
        };

        let content = generate_ares_toml(&config);

        assert!(content.contains("[providers.ollama-local]"));
        assert!(content.contains("[providers.openai]"));
    }

    #[test]
    fn test_generate_env_example() {
        let content = generate_env_example();

        assert!(content.contains("JWT_SECRET"));
        assert!(content.contains("API_KEY"));
        assert!(content.contains("RUST_LOG"));
        assert!(content.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn test_generate_gitignore() {
        let content = generate_gitignore();

        assert!(content.contains("/data/"));
        assert!(content.contains(".env"));
        assert!(content.contains("/target/"));
        assert!(content.contains(".DS_Store"));
    }

    #[test]
    fn test_write_file_creates_new() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = temp_dir.path().join("test.txt");

        let result = write_file(&file_path, "test content", false);
        assert!(result.is_ok());
        assert!(file_path.exists());

        let content = fs::read_to_string(&file_path).expect("Failed to read file");
        assert_eq!(content, "test content");
    }

    #[test]
    fn test_write_file_skips_existing_without_force() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = temp_dir.path().join("test.txt");

        // Create initial file
        fs::write(&file_path, "original").expect("Failed to write");

        // Try to write without force
        let result = write_file(&file_path, "new content", false);
        assert!(result.is_ok());

        // Content should remain original
        let content = fs::read_to_string(&file_path).expect("Failed to read file");
        assert_eq!(content, "original");
    }

    #[test]
    fn test_write_file_overwrites_with_force() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = temp_dir.path().join("test.txt");

        // Create initial file
        fs::write(&file_path, "original").expect("Failed to write");

        // Write with force
        let result = write_file(&file_path, "new content", true);
        assert!(result.is_ok());

        // Content should be new
        let content = fs::read_to_string(&file_path).expect("Failed to read file");
        assert_eq!(content, "new content");
    }

    #[test]
    fn test_run_creates_all_files() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config = create_test_config(&temp_dir);
        let output = Output::no_color();

        let result = run(config, &output);

        match result {
            InitResult::Success => (),
            _ => panic!("Expected Success"),
        }

        // Check all expected files exist
        assert!(temp_dir.path().join("ares.toml").exists());
        assert!(temp_dir.path().join(".env.example").exists());
        assert!(temp_dir.path().join(".gitignore").exists());
        assert!(temp_dir.path().join("data").is_dir());
        assert!(temp_dir.path().join("config/agents").is_dir());
        assert!(temp_dir.path().join("config/models").is_dir());
        assert!(temp_dir.path().join("config/tools").is_dir());
        assert!(temp_dir.path().join("config/workflows").is_dir());
    }

    #[test]
    fn test_run_no_examples_skips_toon_files() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let config = InitConfig {
            path: temp_dir.path().to_path_buf(),
            force: false,
            minimal: false,
            no_examples: true,
            provider: "ollama".to_string(),
            host: "127.0.0.1".to_string(),
            port: 3000,
        };
        let output = Output::no_color();

        let result = run(config, &output);

        match result {
            InitResult::Success => (),
            _ => panic!("Expected Success"),
        }

        // ares.toml should exist
        assert!(temp_dir.path().join("ares.toml").exists());

        // TOON files should not exist
        assert!(!temp_dir.path().join("config/models/fast.toon").exists());
        assert!(!temp_dir.path().join("config/agents/router.toon").exists());
    }

    #[test]
    fn test_run_already_exists_without_force() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create initial ares.toml
        fs::write(temp_dir.path().join("ares.toml"), "existing").expect("Failed to write");

        let config = create_test_config(&temp_dir);
        let output = Output::no_color();

        let result = run(config, &output);

        match result {
            InitResult::AlreadyExists => (),
            _ => panic!("Expected AlreadyExists"),
        }
    }

    #[test]
    fn test_run_force_overwrites() {
        let temp_dir = TempDir::new().expect("Failed to create temp dir");

        // Create initial ares.toml
        fs::write(temp_dir.path().join("ares.toml"), "existing").expect("Failed to write");

        let config = InitConfig {
            path: temp_dir.path().to_path_buf(),
            force: true,
            minimal: false,
            no_examples: true,
            provider: "ollama".to_string(),
            host: "127.0.0.1".to_string(),
            port: 3000,
        };
        let output = Output::no_color();

        let result = run(config, &output);

        match result {
            InitResult::Success => (),
            _ => panic!("Expected Success"),
        }

        // ares.toml should be overwritten
        let content =
            fs::read_to_string(temp_dir.path().join("ares.toml")).expect("Failed to read");
        assert!(content.contains("[server]"));
        assert!(!content.contains("existing"));
    }
}
