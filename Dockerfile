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

# Warm the dependency cache with the real sources (stub main to avoid
# compiling the full binary twice).
RUN mkdir -p bot/src \
    && printf 'fn main() {}\n' > bot/src/main.rs \
    && cd bot && cargo build --release 2>/dev/null; true

# Now the real source — touch to force cargo to rebuild the binary itself.
COPY bot ./bot
RUN cd bot && touch src/main.rs && cargo build --release

# ---------- runtime stage ----------
FROM debian:bookworm-slim
# curl for the compose healthcheck (probes the WebUI /api/status).
RUN apt-get update && apt-get install -y --no-install-recommends curl \
    && rm -rf /var/lib/apt/lists/*

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

# WebUI: dashboard (/) + forecast (/forecast) + start/stop API.
EXPOSE 8090

ENTRYPOINT ["crypto-bot"]
# Default: WebUI, which manages the trading loop as a child process via the
# dashboard's start/stop. Override per deploy (e.g. "live --confirm ..." for
# loop-only without the web server).
CMD ["webui", "--port", "8090", "--autostart"]
