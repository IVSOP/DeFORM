# Devnet account cleanup

Run from this directory:

```bash
./clear-devnet-accounts.sh --list
./clear-devnet-accounts.sh --lobby-id 2 --dry-run
./clear-devnet-accounts.sh --lobby-id 2
./clear-devnet-accounts.sh --account <ADDRESS>
./clear-devnet-accounts.sh
./clear-devnet-accounts.sh --lobby-id 0 --recover-local --dry-run
./clear-devnet-accounts.sh --lobby-id 0 --recover-local
```

The listing includes both undelegated accounts owned by soccer and delegated
lobby/input accounts discovered through MagicBlock's delegation metadata. It
shows addresses, delegation status, lobby IDs and validator identities. Listing
does not require an admin keypair and sends no transactions.

Closing uses the repository admin keypair, or `--admin PATH` / `KEYPAIR_PATH`.
For delegated accounts it calls the existing admin-only `undelegate` instruction
on the validator recorded in the delegation record, waits for every account in
the lobby to return to soccer ownership on devnet, then force-closes the selected
accounts and refunds their rent to the admin. Selecting one delegated account
undelegates its entire lobby, but only closes the selected account.

`--timeout-secs 120` controls the wait per lobby. If cleanup times out, inspect
`--list` before retrying: undelegation may still complete asynchronously. A failed
run may have closed earlier accounts; rerunning discovers the remaining ones.

Use `--er-rpc-url URL` for a custom validator endpoint. Its identity must match the
delegation record. The usual local stack backed by Surfpool cannot recover devnet
accounts assigned to the Localhost identity.

For those disposable accounts, `--recover-local` uses the matching validator key
from the repository's `magicblock/config.toml`. It signs directly on **devnet**;
no ER process or endpoint is needed. Each account is emptied, undelegated, and
force-closed in one atomic transaction signed by the validator and soccer admin.
This discards game state and returns rent. It is restricted to devnet and the
repository's local validator identity, and refuses accounts with pending commits
or existing undelegation requests. `--dry-run --recover-local` simulates these
transactions with signature verification; it requires the admin keypair but sends
no transaction. The temporary validator keypair file has restricted permissions
and is deleted when the script exits.

The normal ER cleanup path requires the deployed program to support `undelegate`
and `process_undelegation`. Orphan inputs, partially delegated lobbies and old
delegated account layouts that the program cannot deserialize require separate
recovery; cleanup reports an error. Direct local-validator recovery only requires
`force_close`, because it empties the account before returning ownership.
Running cranks are not cancelled by this script; it does not possess their
scheduler authority. Their writes cannot proceed once accounts are undelegated.

The underlying CLI also provides `fetch-accounts --include-delegated` and
`clear-accounts` with the same selection and dry-run options. Always supply the
appropriate global `--rpc-url` when using those commands directly.
