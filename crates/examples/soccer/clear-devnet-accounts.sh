#!/usr/bin/env bash
# List or close soccer accounts on devnet, undelegating whole lobbies as needed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADMIN_PATH="${KEYPAIR_PATH:-$SCRIPT_DIR/../../../anchor_program/PRIVATE_DO_NOT_PUBLISH_THIS/admin.json}"
DRY_RUN=false
EXTRA_ARGS=()
RECOVER_LOCAL=false

usage() {
    cat <<'EOF'
Usage: clear-devnet-accounts.sh [--list|--dry-run] [--account ADDRESS|--lobby-id ID]
                                [--admin PATH]
                                [--er-rpc-url URL] [--timeout-secs SECONDS]
                                [--recover-local]

Lists both delegated and undelegated accounts. Without --list/--dry-run, returns
delegated lobbies and their inputs to devnet, waits for ownership to return, then
closes the selected accounts and refunds rent to the admin.

  --list, --dry-run       List addresses, status, lobby IDs and validators only.
  --account ADDRESS      Close only this account (default: all soccer accounts).
                         If delegated, its whole lobby is undelegated first.
  --lobby-id ID          Select all accounts belonging to this lobby.
  --admin PATH           Admin keypair (KEYPAIR_PATH or the repository admin).
  --er-rpc-url URL        Override ER endpoint; its validator identity is checked.
  --timeout-secs SECONDS  Wait for base-chain undelegation (default: 120 per lobby).
  --recover-local        Use the repository local validator key to recover its
                         devnet accounts directly, discarding ER state. No ER
                         process is required. --dry-run simulates the transaction.
  --help                 Show this help.

Unrecoverable/orphan accounts cause an error; they are never silently skipped.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run|--list) DRY_RUN=true; shift ;;
        --recover-local) RECOVER_LOCAL=true; shift ;;
        --account|--lobby-id|--er-rpc-url|--timeout-secs)
            if [[ $# -lt 2 || -z "$2" ]]; then
                echo "$1 requires a value" >&2
                exit 2
            fi
            EXTRA_ARGS+=("$1" "$2")
            shift 2
            ;;
        --admin)
            if [[ $# -lt 2 || -z "$2" ]]; then
                echo "--admin requires a keypair path" >&2
                exit 2
            fi
            ADMIN_PATH="$2"
            shift 2
            ;;
        --help|-h) usage; exit 0 ;;
        *) echo "Unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ ( "$DRY_RUN" == false || "$RECOVER_LOCAL" == true ) && ! -f "$ADMIN_PATH" ]]; then
    echo "Admin keypair not found: $ADMIN_PATH" >&2
    exit 1
fi

# An explicit RPC overrides any RPC_URL inherited from the caller.
SOCCER=(cargo run --quiet --locked --manifest-path "$SCRIPT_DIR/Cargo.toml"
    -p soccer --no-default-features --features bin,anchor,20hz --
    --rpc-url https://api.devnet.solana.com)

if [[ "$DRY_RUN" == true ]]; then EXTRA_ARGS+=(--dry-run); fi
if [[ "$RECOVER_LOCAL" == true ]]; then
    # Keep the validator secret out of process arguments, logs and the repository.
    RECOVERY_KEYPAIR="$(mktemp "${TMPDIR:-/tmp}/soccer-validator-recovery.XXXXXX")"
    trap 'rm -f -- "$RECOVERY_KEYPAIR"' EXIT
    python3 - "$SCRIPT_DIR/../../../magicblock/config.toml" "$RECOVERY_KEYPAIR" <<'PY'
import json, pathlib, sys, tomllib
config = tomllib.loads(pathlib.Path(sys.argv[1]).read_text())
value = config['validator']['keypair']
alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
number = 0
for char in value:
    number = number * 58 + alphabet.index(char)
raw = number.to_bytes((number.bit_length() + 7) // 8, 'big')
raw = b'\0' * (len(value) - len(value.lstrip('1'))) + raw
if len(raw) != 64:
    raise SystemExit('Validator configuration must contain a 64-byte keypair')
pathlib.Path(sys.argv[2]).write_text(json.dumps(list(raw)))
PY
    EXTRA_ARGS+=(--validator-keypair "$RECOVERY_KEYPAIR")
    "${SOCCER[@]}" clear-accounts --admin "$ADMIN_PATH" "${EXTRA_ARGS[@]}"
    exit
fi
exec "${SOCCER[@]}" clear-accounts --admin "$ADMIN_PATH" "${EXTRA_ARGS[@]}"
