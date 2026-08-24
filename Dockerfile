# syntax=docker/dockerfile:1.7

FROM rust:1.98-bookworm AS builder

WORKDIR /app

ARG FEATURES="openai,postgres,mcp"
ARG EXTRA_CARGO_ARGS="--no-default-features"

COPY Cargo.toml Cargo.lock build.rs ./
COPY crates/ ./crates/
COPY src/ ./src/
COPY vendor/ ./vendor/
COPY ares.example.toml ./ares.example.toml

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release ${EXTRA_CARGO_ARGS} --features "${FEATURES}" --bin ares-server && \
    cp /app/target/release/ares-server /tmp/ares-server

FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/* && \
    useradd --create-home --uid 1000 --shell /usr/sbin/nologin ares

WORKDIR /app

COPY --from=builder /tmp/ares-server /usr/local/bin/ares-server
COPY ares.example.toml /app/ares.example.toml

RUN mkdir -p /app/data /app/config && chown -R ares:ares /app

USER ares

ENV RUST_LOG=info
ENV ARES_CONFIG=/app/ares.toml

EXPOSE 3000

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3000/health || exit 1

CMD ["ares-server"]
