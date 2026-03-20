# --- Build stage ---
FROM rust:1.85-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock* askama.toml ./
COPY src/ src/

# Build release binary
RUN cargo build --release

# --- Runtime stage ---
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Create app user
RUN useradd -m -s /bin/bash symlinkarr

COPY --from=builder /app/target/release/symlinkarr /usr/local/bin/symlinkarr
COPY --from=builder /app/src/web/static /usr/local/bin/static

# Default config and data directories
RUN mkdir -p /app/config /app/data && chown -R symlinkarr:symlinkarr /app

USER symlinkarr
WORKDIR /app

# Default: run as daemon
ENTRYPOINT ["symlinkarr"]
CMD ["daemon"]
