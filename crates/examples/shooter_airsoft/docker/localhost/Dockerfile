FROM lukemathwalker/cargo-chef:latest-rust-1-bookworm AS chef
WORKDIR /app
# The workspace's floating nightly is overridden locally by rustup, but Docker
# does not inherit that override. Keep the compiler that Bevy 0.19.0 was tested
# with in every stage, including cargo-chef's generated skeleton workspace.
ARG RUST_TOOLCHAIN=nightly-2026-04-03
ENV RUSTUP_TOOLCHAIN=${RUST_TOOLCHAIN}
RUN rustup toolchain install "$RUSTUP_TOOLCHAIN" --profile minimal && rustc --version

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# The server runs headlessly. The Tnua adapter currently enables Avian debug
# rendering transitively, so bevy_render is still compiled (no window/audio app).
ARG CARGO_BUILD_FEATURES="server,anchor"
RUN cargo chef cook --locked --release --recipe-path recipe.json \
    --no-default-features --features="${CARGO_BUILD_FEATURES}" -p shooter_airsoft
COPY . .
RUN cargo build --locked -p shooter_airsoft --release --no-default-features \
      --features="${CARGO_BUILD_FEATURES}" && \
    cp target/release/shooter_airsoft /shooter_airsoft

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /shooter_airsoft /app/shooter_airsoft
CMD ["/app/shooter_airsoft", "serve"]
