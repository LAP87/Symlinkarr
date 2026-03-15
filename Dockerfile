# --- Build stage ---
FROM --platform=$BUILDPLATFORM rust:1.85-bookworm AS builder

ARG TARGETPLATFORM

RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
    gcc-x86-64-linux-gnu libc6-dev-amd64-cross \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src/ src/

RUN case "$TARGETPLATFORM" in \
      "linux/arm64") \
        export CARGO_TARGET=aarch64-unknown-linux-gnu \
        && export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc ;; \
      *) \
        export CARGO_TARGET=x86_64-unknown-linux-gnu ;; \
    esac \
    && cargo build --release --target $CARGO_TARGET \
    && cp target/$CARGO_TARGET/release/symlinkarr /app/symlinkarr

# --- Runtime stage ---
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash symlinkarr

COPY --from=builder /app/symlinkarr /usr/local/bin/symlinkarr

RUN mkdir -p /app/config /app/data && chown -R symlinkarr:symlinkarr /app

USER symlinkarr
WORKDIR /app

ENTRYPOINT ["symlinkarr"]
CMD ["daemon"]
