# --- Build stage ---
# Network is a compile-time feature: --build-arg FEATURES=testnet|mainnet
FROM rust:1-bookworm AS builder
ARG FEATURES
# protobuf-compiler: seer-sync's build script compiles the lightwalletd gRPC schema.
RUN apt-get update && apt-get install -y protobuf-compiler && rm -rf /var/lib/apt/lists/*
RUN test -n "$FEATURES" || (echo "FEATURES required (testnet or mainnet)" && exit 1)
WORKDIR /build

# Dependency cache layer: manifests + lockfile first, fetch everything
# (registry + git deps + the orchard patch), build against a stub main.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo fetch
RUN cargo build --release --no-default-features --features "$FEATURES"

# Real sources; only the crate itself rebuilds.
COPY src/ src/
RUN touch src/main.rs && cargo build --release --no-default-features --features "$FEATURES"

# --- Runtime stage ---
FROM debian:bookworm-slim
# ca-certificates: TLS to the bundled lightwalletd endpoints.
# curl: swarm/healthcheck probes against the JSON-RPC status method.
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
WORKDIR /data
COPY --from=builder /build/target/release/zns-resolver /usr/local/bin/zns-resolver
# RPC_ADDR is 127.0.0.1:8080 by design; run with --network host and let the
# host nginx (or nothing) front it. DB lands in /data — mount a volume there.
EXPOSE 8080
ENTRYPOINT ["zns-resolver"]
