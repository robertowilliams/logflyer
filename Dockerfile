# ─── Stage 1: dependency cache ───────────────────────────────────────────────
# Build a throwaway binary using only Cargo manifests so Docker can cache the
# full dependency compile layer separately from application source changes.
FROM rust:1.83-bookworm AS deps

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config libssl-dev libssh2-1-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./

# Stub out lib + main so cargo can resolve and compile all dependencies.
RUN mkdir -p src \
    && echo 'pub fn main() {}' > src/lib.rs \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release \
    && rm -rf src

# ─── Stage 2: application build ──────────────────────────────────────────────
FROM deps AS builder

COPY src ./src

# Touch main.rs so cargo knows the real source changed.
RUN touch src/main.rs \
    && cargo build --release

# ─── Stage 3: minimal runtime image ──────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates libssl3 libssh2-1 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/logflayer /usr/local/bin/logflayer

# Log output lands here; mount a volume in production for persistence.
RUN mkdir -p /app/logs

ENV RUST_LOG=info \
    LOG_DIRECTORY=/app/logs \
    LOG_FILE_BASE_NAME=logflayer \
    RUN_MODE=periodic \
    POLL_INTERVAL_SECS=300 \
    API_PORT=8080

EXPOSE 8080 9090

HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

CMD ["logflayer"]
