#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
anchor build -- --no-default-features --features shooter_airsoft
yarn generate ../crates/examples/shooter_airsoft
