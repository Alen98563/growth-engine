# ── Build Stage ──
FROM rust:1.77-slim-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo build --release

# ── Runtime Stage ──
FROM python:3.11-slim-bookworm
RUN pip install --no-cache-dir hyperliquid-python-sdk eth-account requests
COPY --from=builder /app/target/release/growth-engine /usr/local/bin/
COPY scripts/ /app/scripts/
WORKDIR /app
ENTRYPOINT ["growth-engine"]
