# A.R.E.S - Multi-stage Docker Build
# =============================================================================

# Build stage
FROM rust:1.91 as builder

WORKDIR /app
COPY . .

# Build with release optimizations
ARG FEATURES=""
RUN if [ -z "$FEATURES" ]; then \
        cargo build --release; \
    else \
        cargo build --release --features "$FEATURES"; \
    fi

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies and just command runner
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

# Install just (https://just.systems)
# Using prebuilt binary for faster builds
RUN curl --proto '=https' --tlsv1.2 -sSf https://just.systems/install.sh | bash -s -- --to /usr/local/bin

# Copy the built binary
COPY --from=builder /app/target/release/agentic-chatbot-server /usr/local/bin/ares

# Copy justfile for container-based workflows
COPY --from=builder /app/justfile /app/justfile
COPY --from=builder /app/hurl /app/hurl

WORKDIR /app

# Create data directory
RUN mkdir -p /app/data

# Set environment variables
ENV RUST_LOG=info
ENV HOST=0.0.0.0
ENV PORT=3000

EXPOSE 3000

# Health check
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:3000/health || exit 1

CMD ["ares"]
