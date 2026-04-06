# --- Build stage ---
FROM rust:1.85-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock* askama.toml ./
COPY src/ src/

# Build release binary deterministically from the committed lockfile.
RUN cargo build --release --locked

# --- Runtime stage ---
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Create app user
RUN useradd -m -s /bin/bash symlinkarr

COPY --from=builder /app/target/release/symlinkarr /usr/local/bin/symlinkarr
COPY --from=builder /app/src/web/static /usr/local/share/symlinkarr/static

# Default config and data directories
RUN mkdir -p /app/config /app/data && chown -R symlinkarr:symlinkarr /app

USER symlinkarr
WORKDIR /app

EXPOSE 8726
HEALTHCHECK --interval=60s --timeout=10s --start-period=20s --retries=3 CMD symlinkarr status --output json >/dev/null || exit 1

# Default: run as daemon
ENTRYPOINT ["symlinkarr"]
CMD ["daemon"]
