FROM rust:1-bookworm AS builder

WORKDIR /app

COPY . .

ARG CARGO_BUILD_FEATURES="server,anchor"

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build -p pong --release --no-default-features \
      --features="${CARGO_BUILD_FEATURES}" && \
    cp target/release/pong /pong

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /pong /app/pong
CMD ["/app/pong", "serve"]
