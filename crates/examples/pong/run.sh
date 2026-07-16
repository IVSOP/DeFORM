#!/usr/bin/env bash
# Launch two pong clients (each with its own wallet) plus the local docker stack.
#
# Ctrl+C tears down all three: the two cargo clients run in the background and are
# killed by the trap, while `docker compose` (via up.sh) runs in the foreground so
# it receives every Ctrl+C directly -- it wants several to force a hard shutdown.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATES_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)" # workspace root, holds the CLi*.json wallets
DOCKER_DIR="$SCRIPT_DIR/docker/localhost"

# Resolve the two wallets by prefix so the exact base58 suffix doesn't matter.
WALLET1="$(ls "$CRATES_DIR"/CLi1*.json 2>/dev/null | head -n1 || true)"
WALLET2="$(ls "$CRATES_DIR"/CLi2*.json 2>/dev/null | head -n1 || true)"

pids=()
cleanup() {
    for pid in "${pids[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
}
trap cleanup INT TERM EXIT

# cwd = CRATES_DIR so the in-app keypair dropdown (which scans ".") also finds the wallets.
cd "$CRATES_DIR"
# cargo run --release -p pong -- run ${WALLET1:+--wallet="$WALLET1"} &
cargo run --release -p pong --no-default-features --features="client,server,anchor,foc-ping,deform_foc/tracy" -- run ${WALLET1:+--wallet="$WALLET1"} &
pids+=($!)
cargo run --release -p pong -- run ${WALLET2:+--wallet="$WALLET2"} &
pids+=($!)

# Foreground so repeated Ctrl+C reaches docker compose directly.
cd "$DOCKER_DIR"
./up.sh
