#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

# Anchor's IDL generator otherwise selects a floating stable toolchain.
export RUSTUP_TOOLCHAIN="$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)"
# Validate the committed resolution before Anchor invokes cargo build-sbf.
# --locked cannot be passed directly to build-sbf, and an extra -- breaks IDL args.
cargo metadata --locked --format-version 1 > /dev/null
anchor build -- --no-default-features --features soccer
yarn install --frozen-lockfile
yarn generate ../crates/examples/soccer
