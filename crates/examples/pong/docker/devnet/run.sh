#!/usr/bin/env bash
# Target-side entrypoint: shipped inside the bundle produced by ../../package.sh.
# Loads the saved image and brings the stack up. Safe to re-run -- `docker load`
# is idempotent and compose recreates the container in place.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

if [[ ! -f admin.json ]]; then
    echo "error: admin.json is missing. The bundle was built with --no-keypair;" >&2
    echo "       copy the admin keypair next to this script before running." >&2
    exit 1
fi

echo "==> loading pong-server image"
docker load -i image.tar

echo "==> starting stack"
docker compose up -d "$@"

echo
docker compose ps
echo
echo "logs:  docker compose logs -f"
echo "stop:  docker compose down"
