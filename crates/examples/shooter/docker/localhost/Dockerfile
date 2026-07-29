FROM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
WORKDIR /app
COPY rust-toolchain.toml .
RUN rustup show

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# The server build pulls a headless bevy (no windowing/audio features), so no extra
# system packages are needed to compile the avian/tnua simulation.
ARG CARGO_BUILD_FEATURES="server,anchor"
RUN cargo chef cook --release --recipe-path recipe.json \
    --no-default-features --features="${CARGO_BUILD_FEATURES}" -p shooter
COPY . .
RUN cargo build -p shooter --release --no-default-features \
      --features="${CARGO_BUILD_FEATURES}" && \
    cp target/release/shooter /shooter

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /shooter /app/shooter
CMD ["/app/shooter", "serve"]
