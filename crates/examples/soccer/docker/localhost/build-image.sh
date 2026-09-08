#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATES_DIR="$(cd "$SCRIPT_DIR/../../../.." && pwd)"

docker build -f "$SCRIPT_DIR/Dockerfile" -t soccer-server:latest "$CRATES_DIR" "$@"
