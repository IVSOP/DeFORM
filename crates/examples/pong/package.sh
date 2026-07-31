#!/usr/bin/env bash
# Package the pong QUIC server into one archive you can scp to a server.
#
#   ./package.sh                 -> dist/pong-server.tar.zst
#   ./package.sh --gzip          -> dist/pong-server.tar.gz  (target has no zstd)
#   ./package.sh --no-keypair    -> bundle without admin.json (copy it over yourself)
#   PLATFORM=linux/arm64 ./package.sh
#   CARGO_BUILD_FEATURES="server,anchor,60hz" ./package.sh
#
# On the target:
#   tar --zstd -xf pong-server.tar.zst && cd pong-server && ./run.sh
#
# The bundle carries only the devnet stack -- no surfpool, no ephemeral validator.
# The image itself is network-agnostic (RPC_URL/PORT/KEYPAIR_PATH are read from the
# environment), so it is built from the same Dockerfile the localhost stack uses.
set -euo pipefail

GAME="pong"
IMAGE="${GAME}-server:latest"
PLATFORM="${PLATFORM:-linux/amd64}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATES_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"        # docker build context (the workspace)
REPO_DIR="$(cd "$CRATES_DIR/.." && pwd)"
DOCKERFILE="$SCRIPT_DIR/docker/localhost/Dockerfile"
DEVNET_DIR="$SCRIPT_DIR/docker/devnet"
ADMIN_KEYPAIR="$REPO_DIR/anchor_program/PRIVATE_DO_NOT_PUBLISH_THIS/admin.json"

DIST_DIR="$SCRIPT_DIR/dist"
STAGE="$DIST_DIR/${GAME}-server"

compress="zstd"
include_keypair=1
for arg in "$@"; do
    case "$arg" in
        --gzip) compress="gzip" ;;
        --no-keypair) include_keypair=0 ;;
        -h | --help)
            awk 'NR>1 && /^#/ {sub(/^# ?/, ""); print; next} NR>1 {exit}' "${BASH_SOURCE[0]}"
            exit 0
            ;;
        *)
            echo "unknown argument: $arg" >&2
            exit 1
            ;;
    esac
done

if [[ $compress == "zstd" ]] && ! command -v zstd > /dev/null; then
    echo "zstd not found -- falling back to gzip" >&2
    compress="gzip"
fi

echo "==> building $IMAGE for $PLATFORM"
# Only override the Dockerfile's feature default when the caller asked for it, so
# the Dockerfile stays the single source of truth for the server feature set.
build_args=()
if [[ -n ${CARGO_BUILD_FEATURES:-} ]]; then
    build_args+=(--build-arg "CARGO_BUILD_FEATURES=$CARGO_BUILD_FEATURES")
fi
docker build --platform "$PLATFORM" -f "$DOCKERFILE" -t "$IMAGE" "${build_args[@]}" "$CRATES_DIR"

echo "==> staging bundle"
rm -rf "$STAGE"
mkdir -p "$STAGE"

# Saved uncompressed: the whole bundle gets compressed once at the end.
docker save "$IMAGE" -o "$STAGE/image.tar"
cp "$DEVNET_DIR/docker-compose.yml" "$STAGE/"
install -m 755 "$DEVNET_DIR/run.sh" "$STAGE/run.sh"

cat > "$STAGE/.env" << 'EOF'
# Base-layer RPC the server talks to. Public devnet is heavily rate limited --
# point this at a Helius/Triton devnet endpoint for anything beyond a smoke test.
RPC_URL=https://api.devnet.solana.com
# Host-side UDP port for the QUIC server (clients connect to <host>:<PORT>).
PORT=4433
RUST_LOG=info
EOF

cat > "$STAGE/README.md" << 'EOF'
# pong server (devnet)

    tar --zstd -xf pong-server.tar.zst   # or: tar -xzf pong-server.tar.gz
    cd pong-server
    $EDITOR .env                          # set RPC_URL, PORT
    ./run.sh

Needs docker with the compose plugin, and UDP `PORT` (default 4433) open --
the transport is QUIC, so a TCP-only firewall rule will not do.

`admin.json` is the admin keypair the server signs settlement transactions with.
It must be funded on devnet, and the anchor program must already be deployed
there. Clients then connect to `<host>:<PORT>` with the network set to Devnet.

    docker compose logs -f     # follow
    docker compose down        # stop
EOF

if ((include_keypair)); then
    if [[ ! -f $ADMIN_KEYPAIR ]]; then
        echo "error: admin keypair not found at $ADMIN_KEYPAIR (use --no-keypair to skip)" >&2
        exit 1
    fi
    install -m 600 "$ADMIN_KEYPAIR" "$STAGE/admin.json"
    keypair_note="includes admin.json -- treat the archive as a secret"
else
    keypair_note="no admin.json -- copy it into the extracted directory yourself"
fi

echo "==> compressing"
if [[ $compress == "zstd" ]]; then
    ARCHIVE="$DIST_DIR/${GAME}-server.tar.zst"
    tar -C "$DIST_DIR" -cf - "${GAME}-server" | zstd -T0 -9 -f -o "$ARCHIVE" -q
else
    ARCHIVE="$DIST_DIR/${GAME}-server.tar.gz"
    tar -C "$DIST_DIR" -czf "$ARCHIVE" "${GAME}-server"
fi
chmod 600 "$ARCHIVE"
rm -rf "$STAGE"

echo
echo "$ARCHIVE ($(du -h "$ARCHIVE" | cut -f1))"
echo "  $keypair_note"
echo
echo "  scp $ARCHIVE user@host:~/"
echo "  ssh user@host 'tar $([[ $compress == zstd ]] && echo --zstd || echo -z) -xf $(basename "$ARCHIVE") && cd ${GAME}-server && ./run.sh'"
