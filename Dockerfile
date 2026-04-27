FROM rust:1.83-bookworm AS builder

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev libssh2-1-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml ./
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 libssh2-1 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/logflayer /usr/local/bin/logflayer
COPY .env.example /app/.env.example

ENV RUST_LOG=info

CMD ["logflayer"]
