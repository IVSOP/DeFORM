#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
./build-image.sh

exec docker compose up --build "$@"
