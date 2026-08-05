FROM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
WORKDIR /app
COPY rust-toolchain.toml .
RUN rustup show

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
ARG CARGO_BUILD_FEATURES="server,anchor,20hz"
RUN cargo chef cook --release --recipe-path recipe.json \
    --no-default-features --features="${CARGO_BUILD_FEATURES}" -p soccer
COPY . .
RUN cargo build -p soccer --release --no-default-features \
      --features="${CARGO_BUILD_FEATURES}" && \
    cp target/release/soccer /soccer

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /soccer /app/soccer
CMD ["/app/soccer", "serve"]
