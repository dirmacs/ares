# Self-Hosting

Run your own ARES instance on your infrastructure. This guide covers local development setup, production deployment, and configuration options.

---

## Prerequisites

| Requirement | Minimum Version | Notes |
|---|---|---|
| Rust | 1.91+ | Install via [rustup](https://rustup.rs/) |
| PostgreSQL | 15+ | Used for tenants, agents, usage tracking |
| Git | 2.x | For cloning the repository |

Optional, depending on your provider configuration:

| Requirement | When Needed |
|---|---|
| Groq API key | Using Groq as an LLM provider |
| Anthropic API key | Using Anthropic as an LLM provider |
| NVIDIA API key | Using NVIDIA-hosted DeepSeek models |
| Ollama | Running local models |

---

## Quick Start

### 1. Clone the Repository

```bash
git clone https://github.com/dirmacs/ares
cd ares
```

### 2. Set Up the Database

Create a PostgreSQL database for ARES:

```bash
createdb ares
```

ARES runs migrations automatically on startup. No manual schema setup is required.

### 3. Create Configuration

Copy the example config and customize it:

```bash
cp ares.example.toml ares.toml
```

Edit `ares.toml` to configure your providers and models. At minimum, you need one LLM provider:

```toml
[server]
port = 3000

[database]
url = "postgres://localhost/ares"

[[providers]]
name = "groq"
type = "openai"
base_url = "https://api.groq.com/openai/v1"
api_key_env = "GROQ_API_KEY"

[[providers.models]]
id = "llama-3.3-70b-versatile"
name = "llama-3.3-70b"
context_length = 131072
```

### 4. Set Environment Variables

```bash
export DATABASE_URL="postgres://localhost/ares"
export JWT_SECRET="your-secret-key-at-least-32-characters-long"
export API_KEY="your-admin-api-secret"
export GROQ_API_KEY="gsk_..."
```

| Variable | Required | Description |
|---|---|---|
| `DATABASE_URL` | Yes | PostgreSQL connection string |
| `JWT_SECRET` | Yes | Secret for signing JWT tokens (32+ characters) |
| `API_KEY` | Yes | Admin secret for `/api/admin/*` endpoints |
| `GROQ_API_KEY` | If using Groq | Groq API key |
| `ANTHROPIC_API_KEY` | If using Anthropic | Anthropic API key |
| `NVIDIA_API_KEY` | If using NVIDIA | NVIDIA API key |

### 5. Build

```bash
cargo build --release --features openai,postgres,mcp
```

See [Feature Flags](#feature-flags) for all available options.

### 6. Run

```bash
./target/release/ares-server
```

### 7. Verify

```bash
curl http://localhost:3000/health
```

You should receive a `200 OK` response. ARES is running.

---

## Feature Flags

ARES uses Cargo feature flags to control which capabilities are compiled into the binary. This keeps the binary lean — only include what you need.

| Feature | Default | Description |
|---|---|---|
| `openai` | Yes | OpenAI-compatible provider support (also used for Groq, NVIDIA) |
| `anthropic` | No | Anthropic Claude provider support |
| `ollama` | No | Local Ollama model support |
| `postgres` | Yes | PostgreSQL database backend |
| `mcp` | No | Model Context Protocol support for external tool servers |
| `ares-vector` | No | Vector storage and semantic search |

### Build Examples

**Minimal build (Groq only):**

```bash
cargo build --release --no-default-features --features openai,postgres
```

**Full build (all providers):**

```bash
cargo build --release --features openai,anthropic,ollama,postgres,mcp,ares-vector
```

**Production build (recommended for VPS deployment):**

```bash
cargo build --release --no-default-features --features openai,postgres,mcp
```

---

## Production Deployment

### systemd Service

Create a systemd unit file at `/etc/systemd/system/ares.service`:

```ini
[Unit]
Description=ARES AI Agent Platform
After=network.target postgresql.service
Wants=postgresql.service

[Service]
Type=simple
User=ares
Group=ares
WorkingDirectory=/opt/ares
ExecStart=/opt/ares/target/release/ares-server
Restart=on-failure
RestartSec=5
Environment=DATABASE_URL=postgres://dirmacs:password@localhost/ares
Environment=JWT_SECRET=your-production-jwt-secret
Environment=API_KEY=your-admin-secret
Environment=GROQ_API_KEY=gsk_...
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo systemctl enable ares
sudo systemctl start ares
sudo systemctl status ares
```

View logs:

```bash
journalctl -u ares -f
```

### Caddy Reverse Proxy

[Caddy](https://caddyserver.com/) provides automatic HTTPS with Let's Encrypt. Create a `Caddyfile`:

```
api.ares.yourdomain.com {
    reverse_proxy localhost:3000
}
```

Start Caddy:

```bash
sudo systemctl enable caddy
sudo systemctl start caddy
```

Caddy automatically provisions and renews TLS certificates. No manual certificate management is needed.

### PostgreSQL Setup

For production, create a dedicated database user:

```sql
CREATE USER ares WITH PASSWORD 'strong-password-here';
CREATE DATABASE ares OWNER ares;
```

Update your `DATABASE_URL` accordingly:

```
DATABASE_URL=postgres://ares:strong-password-here@localhost/ares
```

---

## Configuration Reference

The `ares.toml` file is the primary configuration file. It controls server settings, providers, models, and agent definitions.

### Server Section

```toml
[server]
port = 3000          # HTTP port (overrides PORT env var)
host = "0.0.0.0"     # Bind address
```

### Database Section

```toml
[database]
url = "postgres://ares:password@localhost/ares"
max_connections = 10
```

### Provider Section

Each provider is defined as a `[[providers]]` entry:

```toml
[[providers]]
name = "groq"
type = "openai"
base_url = "https://api.groq.com/openai/v1"
api_key_env = "GROQ_API_KEY"

[[providers.models]]
id = "llama-3.3-70b-versatile"
name = "llama-3.3-70b"
context_length = 131072

[[providers.models]]
id = "llama-3.1-8b-instant"
name = "llama-3.1-8b"
context_length = 131072

[[providers]]
name = "anthropic"
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[[providers.models]]
id = "claude-3-5-sonnet-20241022"
name = "claude-3.5-sonnet"
context_length = 200000

[[providers]]
name = "local"
type = "ollama"
base_url = "http://localhost:11434"

[[providers.models]]
id = "mistral"
name = "mistral-7b"
context_length = 32768
```

### Agent Section

Static agents can be defined in the config file:

```toml
[[agents]]
name = "general-assistant"
model = "llama-3.3-70b"
system_prompt = "You are a helpful assistant."
tools = ["calculator", "web_search"]
max_tokens = 4096
```

For tenant-specific agents, use the Admin API instead of config file definitions.

---

## Updating

To update a running ARES instance:

```bash
cd /opt/ares
git pull origin main
cargo build --release --no-default-features --features openai,postgres,mcp
sudo systemctl restart ares
```

Database migrations run automatically on startup. No manual migration steps are needed.

---

## Troubleshooting

**Port already in use:**
```
Error: Address already in use (os error 98)
```
Another process is using port 3000. Either stop it or change the port in `ares.toml`.

**Database connection failed:**
```
Error: error communicating with database
```
Verify PostgreSQL is running and your `DATABASE_URL` is correct. Check that the database user has permissions on the database.

**Provider API key missing:**
```
Error: Environment variable GROQ_API_KEY not set
```
Set the required API key environment variable, or remove the provider from `ares.toml` if you do not need it.

**JWT secret too short:**
```
Error: JWT_SECRET must be at least 32 characters
```
Use a longer secret. Generate one with: `openssl rand -hex 32`
