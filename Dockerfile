# A.R.E.S - Multi-stage Docker Build
# =============================================================================
# Build with: docker build -t ares-server .
# Build with UI: docker build --build-arg FEATURES="ui" -t ares-server .
# Build with all features: docker build --build-arg FEATURES="full" -t ares-server .

# -----------------------------------------------------------------------------
# Stage 1: UI Build (optional, only if UI feature is enabled)
# -----------------------------------------------------------------------------
FROM node:20-slim AS ui-builder

# Install bun for faster builds
RUN npm install -g bun

WORKDIR /app/ui

# Copy UI source files
COPY ui/package.json ui/bun.lockb* ui/package-lock.json* ./
RUN bun install || npm install

COPY ui/ ./

# Install Rust and trunk for WASM build
RUN apt-get update && apt-get install -y curl build-essential && \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && \
    . "$HOME/.cargo/env" && \
    rustup target add wasm32-unknown-unknown && \
    cargo install trunk --locked

# Build the UI
RUN . "$HOME/.cargo/env" && trunk build --release

# -----------------------------------------------------------------------------
# Stage 2: Rust Build
# -----------------------------------------------------------------------------
FROM rust:1.91 AS builder

WORKDIR /app

# Build argument for features
ARG FEATURES=""
ARG BUILD_UI="false"

# Copy source files
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/

# Copy UI dist if building with UI feature
COPY --from=ui-builder /app/ui/dist/ ./ui/dist/

# Create empty ui/dist if it doesn't exist (for non-UI builds)
RUN mkdir -p ui/dist && touch ui/dist/.keep

# Build with release optimizations
RUN if [ -z "$FEATURES" ]; then \
    cargo build --release; \
    else \
    cargo build --release --features "$FEATURES"; \
    fi

# -----------------------------------------------------------------------------
# Stage 3: Runtime
# -----------------------------------------------------------------------------
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN useradd -m -u 1000 -s /bin/bash ares

# Copy the built binary
COPY --from=builder /app/target/release/ares-server /usr/local/bin/ares-server

# Copy default configuration files for init command reference
COPY ares.example.toml /app/ares.example.toml

WORKDIR /app

# Create data directory with proper permissions
RUN mkdir -p /app/data /app/config && \
    chown -R ares:ares /app

# Switch to non-root user
USER ares

# Set environment variables
ENV RUST_LOG=info
ENV HOST=0.0.0.0
ENV PORT=3000

# Expose the default port
EXPOSE 3000

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:3000/health || exit 1

# Default command - run the server
# Users can override with: docker run ares-server init
CMD ["ares-server"]
