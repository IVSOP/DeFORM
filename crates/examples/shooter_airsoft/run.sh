#!/usr/bin/env bash
# Launch two shooter_airsoft clients (each with its own wallet) plus the local docker stack
# (surfpool base layer + the QUIC game server — no ephemeral validator: the shooter_airsoft
# is Web2-only).
#
# Before the first run: ../../anchor_program/build_airsoft_shooter.sh, so the program
# surfpool deploys is the one built with the `shooter_airsoft` feature.
#
# In-game flow: both clients Connect (Localhost) -> Create/Join Lobby -> Ready ->
# Read Lobby -> Play Online (web2).
#
# Build before launching anything, and stop the whole session when any client
# or Compose exits. Job control gives each child its own process group, so the
# cleanup trap also stops the game process launched underneath `cargo run`.
set -euo pipefail
set -m

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATES_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)" # workspace root, holds the CLi*.json wallets
DOCKER_DIR="$SCRIPT_DIR/docker/localhost"

# Resolve the two wallets by prefix so the exact base58 suffix doesn't matter.
WALLET1="$(ls "$CRATES_DIR"/CLi1*.json 2>/dev/null | head -n1 || true)"
WALLET2="$(ls "$CRATES_DIR"/CLi2*.json 2>/dev/null | head -n1 || true)"

pids=()
cleanup() {
    local status=$?
    trap - EXIT INT TERM
    for pid in "${pids[@]}"; do
        kill -TERM -- "-$pid" 2>/dev/null || true
    done
    wait "${pids[@]}" 2>/dev/null || true
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# A failed image build must not start clients or try to pull a missing image.
"$DOCKER_DIR/build-image.sh"

# cwd = CRATES_DIR so the in-app keypair dropdown (which scans ".") also finds the wallets.
cd "$CRATES_DIR"
cargo run --release -p shooter_airsoft --features="metrics" -- run ${WALLET1:+--wallet="$WALLET1"} &
pids+=($!)
cargo run --release -p shooter_airsoft -- run ${WALLET2:+--wallet="$WALLET2"} &
pids+=($!)

# The image is already built. Monitor Compose alongside both clients so a
# background compilation/connection failure cannot leave the rest running.
# Compose is a background job: terminal menus/progress can suspend it with
# SIGTTOU before containers start. Keep its input and output non-interactive.
# --build also builds Compose-managed services such as the ephemeral validator.
(cd "$DOCKER_DIR" && exec docker compose --ansi never --progress plain up --build --menu=false </dev/null) &
pids+=($!)
# Collect the status even if a command exited before monitoring began. `wait -n`
# can skip already-finished jobs; checking the known PIDs avoids that race.
while :; do
    for pid in "${pids[@]}"; do
        if ! kill -0 "$pid" 2>/dev/null; then
            wait "$pid"
            exit 0
        fi
    done
    sleep 0.1
done
