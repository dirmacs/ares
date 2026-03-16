#!/usr/bin/env bash
# Development Setup Script for A.R.E.S
# This script helps set up a local development environment with Ollama models
# and initialize a new A.R.E.S project.

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Helper functions
info() {
    echo -e "${BLUE}ℹ${NC} $1"
}

success() {
    echo -e "${GREEN}✓${NC} $1"
}

warning() {
    echo -e "${YELLOW}⚠${NC} $1"
}

error() {
    echo -e "${RED}✗${NC} $1"
    exit 1
}

# Check if running in Docker Compose environment
check_docker_compose() {
    if ! command -v docker-compose &> /dev/null && ! docker compose version &> /dev/null; then
        error "Docker Compose is not installed. Please install it first."
    fi
    success "Docker Compose is available"
}

# Check for Node.js runtime (npm, bun, or deno)
check_node_runtime() {
    if command -v bun &> /dev/null; then
        success "Bun is available ($(bun --version))"
        NODE_RUNTIME="bun"
        return 0
    elif command -v npm &> /dev/null; then
        success "npm is available ($(npm --version))"
        NODE_RUNTIME="npm"
        return 0
    elif command -v deno &> /dev/null; then
        success "Deno is available ($(deno --version | head -1))"
        NODE_RUNTIME="deno"
        return 0
    else
        warning "No Node.js runtime found (bun, npm, or deno)"
        warning "Node.js runtime is required for UI development"
        NODE_RUNTIME=""
        return 1
    fi
}

# Check if ares-server binary is available
check_ares_binary() {
    if command -v ares-server &> /dev/null; then
        success "ares-server binary found"
        return 0
    elif [ -f "./target/debug/ares-server" ]; then
        success "ares-server binary found (debug build)"
        ARES_BIN="./target/debug/ares-server"
        return 0
    elif [ -f "./target/release/ares-server" ]; then
        success "ares-server binary found (release build)"
        ARES_BIN="./target/release/ares-server"
        return 0
    else
        warning "ares-server binary not found"
        info "Building ares-server..."
        cargo build
        ARES_BIN="./target/debug/ares-server"
        return 0
    fi
}

# Initialize a new A.R.E.S project using the CLI
init_project() {
    info "Initializing A.R.E.S project..."

    if [ -f "ares.toml" ]; then
        warning "ares.toml already exists"
        read -p "Overwrite existing configuration? [y/N]: " overwrite
        if [[ "$overwrite" =~ ^[Yy]$ ]]; then
            ${ARES_BIN:-ares-server} init --force
        else
            info "Skipping initialization"
            return 0
        fi
    else
        ${ARES_BIN:-ares-server} init
    fi

    success "Project initialized"
}

# Check if Ollama is running
check_ollama() {
    local ollama_url="${OLLAMA_BASE_URL:-http://localhost:11434}"

    if curl -s "${ollama_url}/api/tags" > /dev/null 2>&1; then
        success "Ollama is running at ${ollama_url}"
        return 0
    else
        warning "Ollama is not running"
        return 1
    fi
}

# Start Docker Compose services
start_services() {
    info "Starting Docker Compose services..."

    if [ -f "docker-compose.dev.yml" ]; then
        docker compose -f docker-compose.dev.yml up -d ollama qdrant
        success "Docker services started"

        # Wait for Ollama to be ready
        info "Waiting for Ollama to be ready..."
        for i in {1..30}; do
            if check_ollama; then
                break
            fi
            echo -n "."
            sleep 2
        done
        echo ""
    else
        error "docker-compose.dev.yml not found"
    fi
}

# Pull an Ollama model
pull_model() {
    local model=$1
    local ollama_url="${OLLAMA_BASE_URL:-http://localhost:11434}"

    info "Pulling model: ${model}"

    if command -v ollama &> /dev/null; then
        # Use local Ollama CLI if available
        ollama pull "${model}"
    else
        # Use Docker Compose exec
        docker compose -f docker-compose.dev.yml exec ollama ollama pull "${model}"
    fi

    success "Model ${model} pulled successfully"
}

# List available models
list_models() {
    local ollama_url="${OLLAMA_BASE_URL:-http://localhost:11434}"

    info "Available models:"

    if command -v ollama &> /dev/null; then
        ollama list
    else
        docker compose -f docker-compose.dev.yml exec ollama ollama list
    fi
}

# Create .env file if it doesn't exist
setup_env() {
    if [ ! -f ".env" ]; then
        info "Creating .env file..."

        cat > .env << 'EOF'
# Server Configuration
HOST=127.0.0.1
PORT=3000

# Database (local SQLite by default)
TURSO_URL=file:local.db
TURSO_AUTH_TOKEN=

# Ollama Configuration (default provider)
OLLAMA_BASE_URL=http://localhost:11434
OLLAMA_MODEL=ministral-3:3b

# Optional: OpenAI (if you want to use it)
# OPENAI_API_KEY=sk-...
# OPENAI_API_BASE=https://api.openai.com/v1
# OPENAI_MODEL=gpt-4

# Optional: LlamaCpp (for direct GGUF loading)
# LLAMACPP_MODEL_PATH=/path/to/model.gguf
# LLAMACPP_N_CTX=4096
# LLAMACPP_N_THREADS=4
# LLAMACPP_MAX_TOKENS=512

# Optional: Qdrant (vector database)
# QDRANT_URL=http://localhost:6334
# QDRANT_API_KEY=

# Authentication
JWT_SECRET=$(openssl rand -base64 32)
API_KEY=$(openssl rand -hex 16)

# Logging
RUST_LOG=info,ares=debug
EOF

        success ".env file created"
    else
        warning ".env file already exists, skipping creation"
    fi
}

# Create models directory
setup_models_dir() {
    if [ ! -d "models" ]; then
        info "Creating models directory for GGUF files..."
        mkdir -p models
        success "models/ directory created"
    fi
}

# Interactive model selection
interactive_setup() {
    echo ""
    info "A.R.E.S Development Setup"
    echo ""

    echo "Select models to pull (you can select multiple):"
    echo "1) granite4:tiny-h (4B) - Recommended for development"
    echo "2) qwen3-vl:2b - Vision model with multimodal support"
    echo "3) ministral-3 (3B) - Excellent general purpose"
    echo "4) phi3 - Efficient 3.8B model"
    echo "5) qwen2.5:1.5b - Fast multilingual"
    echo "6) Custom model name"
    echo "7) Skip model download"
    echo ""

    read -p "Enter your choices (e.g., 1 3 4): " choices

    for choice in $choices; do
        case $choice in
            1) pull_model "ministral-3:3b" ;;
            2) pull_model "qwen3-vl:2b" ;;
            3) pull_model "granite4:tiny-h" ;;
            4) pull_model "phi3" ;;
            5) pull_model "qwen2.5:1.5b" ;;
            6)
                read -p "Enter model name: " custom_model
                pull_model "${custom_model}"
                ;;
            7)
                info "Skipping model download"
                ;;
            *)
                warning "Invalid choice: $choice"
                ;;
        esac
    done
}

# Main setup workflow
main() {
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  A.R.E.S Development Environment Setup"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo ""

    # Check prerequisites
    check_docker_compose
    check_node_runtime || true
    check_ares_binary || true

    # Setup environment
    setup_env
    setup_models_dir

    # Ask user what they want to do
    echo ""
    echo "Setup options:"
    echo "1) Full setup (init project + start services + pull models)"
    echo "2) Initialize project only (using ares-server init)"
    echo "3) Start services only"
    echo "4) Pull models only (services must be running)"
    echo "5) List current models"
    echo "6) Setup UI development environment"
    echo ""
    read -p "Choose an option (1-6): " setup_choice

    case $setup_choice in
        1)
            init_project
            start_services
            interactive_setup
            list_models
            ;;
        2)
            init_project
            success "Project initialized. Run this script again to start services."
            ;;
        3)
            start_services
            success "Services started. Run this script again to pull models."
            ;;
        4)
            if check_ollama; then
                interactive_setup
                list_models
            else
                error "Ollama is not running. Start services first (option 3)."
            fi
            ;;
        5)
            if check_ollama; then
                list_models
            else
                error "Ollama is not running."
            fi
            ;;
        6)
            setup_ui_dev
            ;;
        *)
            error "Invalid option"
            ;;
    esac

    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    success "Setup complete!"
    echo ""
    info "Next steps:"
    echo "  1. Build and run A.R.E.S:"
    echo "     cargo build --features ollama"
    echo "     cargo run --features ollama"
    echo ""
    echo "  2. Or use Docker Compose:"
    echo "     docker compose -f docker-compose.dev.yml up ares"
    echo ""
    echo "  3. Access the API:"
    echo "     http://localhost:3000"
    echo "     http://localhost:3000/swagger-ui/"
    echo ""
    echo "  4. Ollama Web UI:"
    echo "     http://localhost:11434"
    echo ""
    echo "  5. Qdrant Dashboard:"
    echo "     http://localhost:6333/dashboard"
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
}

# Setup UI development environment
setup_ui_dev() {
    info "Setting up UI development environment..."

    if [ -z "$NODE_RUNTIME" ]; then
        error "No Node.js runtime found. Please install bun, npm, or deno first."
    fi

    # Check for wasm32 target
    if ! rustup target list --installed | grep -q wasm32-unknown-unknown; then
        info "Installing wasm32-unknown-unknown target..."
        rustup target add wasm32-unknown-unknown
    fi
    success "WASM target installed"

    # Check for trunk
    if ! command -v trunk &> /dev/null; then
        info "Installing trunk..."
        cargo install trunk --locked
    fi
    success "Trunk installed"

    # Install UI dependencies
    info "Installing UI dependencies..."
    cd ui
    case $NODE_RUNTIME in
        bun)
            bun install
            ;;
        npm)
            npm install
            ;;
        deno)
            # Deno doesn't need npm install for most cases
            info "Deno detected - dependencies will be fetched on demand"
            ;;
    esac
    cd ..
    success "UI dependencies installed"

    echo ""
    success "UI development environment is ready!"
    echo ""
    info "To start the UI development server:"
    echo "     cd ui && trunk serve --open"
    echo ""
    info "Or use just:"
    echo "     just ui-dev"
    echo ""
}

# Run main function
main "$@"
