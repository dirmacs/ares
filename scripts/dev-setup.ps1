# Development Setup Script for A.R.E.S (Windows PowerShell)
# This script helps set up a local development environment with Ollama models

$ErrorActionPreference = "Stop"

# Colors for output
function Write-Info {
    param([string]$Message)
    Write-Host "ℹ $Message" -ForegroundColor Blue
}

function Write-Success {
    param([string]$Message)
    Write-Host "✓ $Message" -ForegroundColor Green
}

function Write-Warning {
    param([string]$Message)
    Write-Host "⚠ $Message" -ForegroundColor Yellow
}

function Write-Error-Message {
    param([string]$Message)
    Write-Host "✗ $Message" -ForegroundColor Red
    exit 1
}

# Check if Docker Compose is available
function Test-DockerCompose {
    try {
        $null = docker compose version
        Write-Success "Docker Compose is available"
        return $true
    } catch {
        try {
            $null = docker-compose --version
            Write-Success "Docker Compose is available"
            return $true
        } catch {
            Write-Error-Message "Docker Compose is not installed. Please install Docker Desktop first."
            return $false
        }
    }
}

# Check if Ollama is running
function Test-Ollama {
    $ollamaUrl = if ($env:OLLAMA_BASE_URL) { $env:OLLAMA_BASE_URL } else { "http://localhost:11434" }

    try {
        $response = Invoke-WebRequest -Uri "$ollamaUrl/api/tags" -Method Get -TimeoutSec 5 -UseBasicParsing
        if ($response.StatusCode -eq 200) {
            Write-Success "Ollama is running at $ollamaUrl"
            return $true
        }
    } catch {
        Write-Warning "Ollama is not running"
        return $false
    }
}

# Start Docker Compose services
function Start-Services {
    Write-Info "Starting Docker Compose services..."

    if (Test-Path "docker-compose.dev.yml") {
        docker compose -f docker-compose.dev.yml up -d ollama qdrant
        Write-Success "Docker services started"

        # Wait for Ollama to be ready
        Write-Info "Waiting for Ollama to be ready..."
        $maxAttempts = 30
        $attempt = 0

        while ($attempt -lt $maxAttempts) {
            if (Test-Ollama) {
                break
            }
            Write-Host "." -NoNewline
            Start-Sleep -Seconds 2
            $attempt++
        }
        Write-Host ""

        if ($attempt -eq $maxAttempts) {
            Write-Warning "Ollama did not start in time. Check logs with: docker compose -f docker-compose.dev.yml logs ollama"
        }
    } else {
        Write-Error-Message "docker-compose.dev.yml not found"
    }
}

# Pull an Ollama model
function Get-OllamaModel {
    param([string]$ModelName)

    Write-Info "Pulling model: $ModelName"

    # Check if ollama CLI is available
    try {
        $null = Get-Command ollama -ErrorAction Stop
        ollama pull $ModelName
    } catch {
        # Use Docker Compose exec
        docker compose -f docker-compose.dev.yml exec ollama ollama pull $ModelName
    }

    if ($LASTEXITCODE -eq 0) {
        Write-Success "Model $ModelName pulled successfully"
    } else {
        Write-Warning "Failed to pull model $ModelName"
    }
}

# List available models
function Show-Models {
    Write-Info "Available models:"

    try {
        $null = Get-Command ollama -ErrorAction Stop
        ollama list
    } catch {
        docker compose -f docker-compose.dev.yml exec ollama ollama list
    }
}

# Create .env file if it doesn't exist
function Initialize-Environment {
    if (-not (Test-Path ".env")) {
        Write-Info "Creating .env file..."

        # Generate random secrets
        $jwtSecret = [Convert]::ToBase64String([System.Security.Cryptography.RandomNumberGenerator]::GetBytes(32))
        $apiKey = -join ((48..57) + (97..102) | Get-Random -Count 32 | ForEach-Object {[char]$_})

        $envContent = @"
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
# LLAMACPP_MODEL_PATH=C:\path\to\model.gguf
# LLAMACPP_N_CTX=4096
# LLAMACPP_N_THREADS=4
# LLAMACPP_MAX_TOKENS=512

# Optional: Qdrant (vector database)
# QDRANT_URL=http://localhost:6334
# QDRANT_API_KEY=

# Authentication
JWT_SECRET=$jwtSecret
API_KEY=$apiKey

# Logging
RUST_LOG=info,ares=debug
"@

        $envContent | Out-File -FilePath ".env" -Encoding UTF8
        Write-Success ".env file created"
    } else {
        Write-Warning ".env file already exists, skipping creation"
    }
}

# Create models directory
function Initialize-ModelsDirectory {
    if (-not (Test-Path "models")) {
        Write-Info "Creating models directory for GGUF files..."
        New-Item -ItemType Directory -Path "models" | Out-Null
        Write-Success "models\ directory created"
    }
}

# Interactive model selection
function Start-InteractiveSetup {
    Write-Host ""
    Write-Info "A.R.E.S Development Setup"
    Write-Host ""

    Write-Host "Select models to pull (you can select multiple separated by space):"
    Write-Host "1) ministral-3:3b (3B) - Excellent general purpose"
    Write-Host "2) qwen3-vl:2b - Vision model with multimodal support"
    Write-Host "3) granite4:tiny-h (4B) - Recommended for development"
    Write-Host "4) phi3 - Efficient 3.8B model"
    Write-Host "5) qwen2.5:1.5b - Fast multilingual"
    Write-Host "6) Custom model name"
    Write-Host "7) Skip model download"
    Write-Host ""

    $choices = Read-Host "Enter your choices (e.g., 1 3 4)"
    $choiceArray = $choices -split '\s+'

    foreach ($choice in $choiceArray) {
        switch ($choice) {
            "1" { Get-OllamaModel "ministral-3:3b" }
            "2" { Get-OllamaModel "qwen3-vl:2b" }
            "3" { Get-OllamaModel "granite4:tiny-h" }
            "4" { Get-OllamaModel "phi3" }
            "5" { Get-OllamaModel "qwen2.5:1.5b" }
            "6" {
                $customModel = Read-Host "Enter model name"
                Get-OllamaModel $customModel
            }
            "7" { Write-Info "Skipping model download" }
            default { Write-Warning "Invalid choice: $choice" }
        }
    }
}

# Main setup workflow
function Start-Setup {
    Write-Host ""
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor Cyan
    Write-Host "  A.R.E.S Development Environment Setup" -ForegroundColor Cyan
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor Cyan
    Write-Host ""

    # Check prerequisites
    Test-DockerCompose | Out-Null

    # Setup environment
    Initialize-Environment
    Initialize-ModelsDirectory

    # Ask user what they want to do
    Write-Host ""
    Write-Host "Setup options:"
    Write-Host "1) Full setup (start services + pull models)"
    Write-Host "2) Start services only"
    Write-Host "3) Pull models only (services must be running)"
    Write-Host "4) List current models"
    Write-Host ""

    $setupChoice = Read-Host "Choose an option (1-4)"

    switch ($setupChoice) {
        "1" {
            Start-Services
            Start-InteractiveSetup
            Show-Models
        }
        "2" {
            Start-Services
            Write-Success "Services started. Run this script again to pull models."
        }
        "3" {
            if (Test-Ollama) {
                Start-InteractiveSetup
                Show-Models
            } else {
                Write-Error-Message "Ollama is not running. Start services first (option 2)."
            }
        }
        "4" {
            if (Test-Ollama) {
                Show-Models
            } else {
                Write-Error-Message "Ollama is not running."
            }
        }
        default {
            Write-Error-Message "Invalid option"
        }
    }

    Write-Host ""
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor Cyan
    Write-Success "Setup complete!"
    Write-Host ""
    Write-Info "Next steps:"
    Write-Host "  1. Build and run A.R.E.S:"
    Write-Host "     cargo build --features ollama"
    Write-Host "     cargo run --features ollama"
    Write-Host ""
    Write-Host "  2. Or use Docker Compose:"
    Write-Host "     docker compose -f docker-compose.dev.yml up ares"
    Write-Host ""
    Write-Host "  3. Access the API:"
    Write-Host "     http://localhost:3000"
    Write-Host "     http://localhost:3000/swagger-ui/"
    Write-Host ""
    Write-Host "  4. Ollama Web UI:"
    Write-Host "     http://localhost:11434"
    Write-Host ""
    Write-Host "  5. Qdrant Dashboard:"
    Write-Host "     http://localhost:6333/dashboard"
    Write-Host ""
    Write-Host "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━" -ForegroundColor Cyan
}

# Run main function
Start-Setup
