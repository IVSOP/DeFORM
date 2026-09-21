# Multi-platform base image pinned by digest; see crates/BUILD_VERSIONS.md.
FROM lukemathwalker/cargo-chef@sha256:5a37e174ceb7ccbebd6d0024049e9308f4de399bbcabbc3c6c649c70486880d5 AS chef
WORKDIR /app
# Keep all stages (including cargo-chef's skeleton) on the workspace compiler.
ENV RUSTUP_TOOLCHAIN=nightly-2026-04-03
RUN rustup toolchain install "$RUSTUP_TOOLCHAIN" --profile minimal && rustc --version

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
ARG CARGO_BUILD_FEATURES="server,anchor,20hz"
RUN cargo chef cook --locked --release --recipe-path recipe.json \
    --no-default-features --features="${CARGO_BUILD_FEATURES}" -p soccer
COPY . .
RUN cargo build --locked -p soccer --release --no-default-features \
      --features="${CARGO_BUILD_FEATURES}" && \
    cp target/release/soccer /soccer

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251
# Freeze apt packages as well as the base image. Release signatures stay enabled.
RUN printf '%s\n' \
      'Types: deb' \
      'URIs: http://snapshot.debian.org/archive/debian/20260920T000000Z/' \
      'Suites: bookworm bookworm-updates' 'Components: main' \
      'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' 'Check-Valid-Until: no' \
      '' 'Types: deb' \
      'URIs: http://snapshot.debian.org/archive/debian-security/20260920T000000Z/' \
      'Suites: bookworm-security' 'Components: main' \
      'Signed-By: /usr/share/keyrings/debian-archive-keyring.gpg' 'Check-Valid-Until: no' \
      > /etc/apt/sources.list.d/debian.sources \
    && apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /soccer /app/soccer
CMD ["/app/soccer", "serve"]
