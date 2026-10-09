# crypto-bot — multi-stage Docker build
#
# Builds the Rust live-trading bot and ships a slim runtime image.
# State lives in /app/data (mount a volume so it survives restarts).
# Secrets come from the environment (.env via docker compose), never baked in.
#
# The service-level compose file lives in the nuniesmith/oryx repo, which
# builds this Dockerfile straight from this repo's git URL.

# ---------- build stage ----------
FROM rust:1.96-bookworm AS builder

WORKDIR /src

# Copy manifests first so dependency compilation is cached in its own layer.
COPY bot/Cargo.toml bot/Cargo.lock ./bot/

# Warm the dependency cache with a stub binary (rebuilt for real below).
RUN mkdir -p bot/src \
    && printf 'fn main() {}\n' > bot/src/main.rs \
    && cd bot && cargo build --release 2>/dev/null; rm -rf bot/src

# Now the real source.
COPY bot ./bot
RUN cd bot && cargo build --release

# ---------- runtime stage ----------
FROM debian:bookworm-slim

# TLS roots for Kraken/Discord HTTPS (bot uses rustls; no OpenSSL needed).
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/bot/target/release/crypto-bot /usr/local/bin/crypto-bot

WORKDIR /app
RUN mkdir -p /app/data/paper

# Where the bot keeps state.json and reads .env-adjacent config.
ENV CRYPTO_BOT_ROOT=/app

# Persist state across container restarts/upgrades.
VOLUME ["/app/data"]

ENTRYPOINT ["crypto-bot"]
# Default: live loop. Override per deploy (e.g. "paper" for a dry run).
CMD ["live", "--confirm", "I_UNDERSTAND_REAL_MONEY", "--loop"]
