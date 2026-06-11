# Client Generation

## Overview

The Rust client for this program is generated with [Codama](https://github.com/codama-idl/codama) from the Anchor IDL. Codama produces typed instruction builders, CPI helpers, argument structs (borsh), error types, and a program ID constant.

The generated client is a low-level crate (`anchor_program_client`). Game code should not depend on it directly — instead, use the `deform_program` trait interface with the `deform_program_anchor` implementation. This allows swapping the on-chain program framework (e.g. Anchor to Pinocchio) without changing game code.

## Architecture

```
Game code
    |
    v
deform_program          (trait: DeformProgramClient)
    ^
    |
deform_program_anchor   (impl + Codama-generated code)
```

- `deform_program` — trait crate in `../crates/deform_program/`. Defines `DeformProgramClient<I, G>` with methods for each instruction, PDA derivation, and lobby deserialization. Framework-agnostic.
- `deform_program_anchor` — implementation crate in `../crates/deform_program_anchor/`. Contains:
  - `src/generated/` — Codama-generated instruction builders, borsh arg structs, discriminators, error types. **Do not edit** these files; they are overwritten by `yarn generate`.
  - `src/lib.rs` — `AnchorClient` struct implementing the trait, using the generated builders and wincode for lobby deserialization.

## How to regenerate

1. Build the program to produce a fresh IDL:

```sh
anchor build
```

2. Run the generation script:

```sh
yarn generate
```

This reads `target/idl/anchor_program.json` and writes the Rust client to `clients/rust/`.

The generated files land in `../crates/deform_program_anchor/src/generated/`. Codama does not generate a `Cargo.toml` — the dependencies live in the workspace. You may want to run `rustfmt` on the output.

## What Codama generates

All generated code lives in `../crates/deform_program_anchor/src/generated/`:

- **Instructions** (`instructions/`) — for each instruction (`create_lobby`, `join_lobby`, `ready`, `write_and_close`):
  - Account struct with an `.instruction()` method that builds a `solana_instruction::Instruction`
  - Builder pattern struct (e.g. `CreateLobbyBuilder`)
  - CPI struct and CPI builder for on-chain cross-program invocation
  - Instruction args struct with borsh serialization
  - Discriminator constant
- **Types** (`types/`) — borsh-serializable structs for instruction arg types like `PlayerScore`.
- **Errors** (`errors/`) — error enum matching the on-chain `ErrorCode`.
- **Program** (`programs.rs`) — the program ID constant.

## What Codama does NOT generate

### Lobby account deserialization

`LobbyAccount` uses wincode for serialization, not borsh. Because it is not annotated with `#[account]`, it does not appear in the Anchor IDL. This means:

- Codama has **no account struct or decoder** for `LobbyAccount`.
- You must **deserialize lobby account data manually** using wincode.
- The same applies to **PDA derivation** — the lobby PDA uses seeds `["lobby", id.to_le_bytes()]` and must be computed manually since Codama has no seeds metadata for it.

Both of these are handled by `deform_program_anchor`, so game code using the `DeformProgramClient` trait gets them for free.

### On-chain layout (for manual deserialization)

The account data is wincode-encoded with this layout:
- `account_type: u64` (discriminant — `0` for Lobby)
- `bump: u8`
- `lobby: Lobby<Inputs, GameState>` (wincode-encoded)

## Files

| File | Purpose |
|---|---|
| `generate-client.mjs` | Codama generation script |
| `target/idl/anchor_program.json` | Anchor IDL (input) |
| `../crates/deform_program/` | Framework-agnostic trait interface |
| `../crates/deform_program_anchor/src/generated/` | Codama output (do not edit) |
| `../crates/deform_program_anchor/src/lib.rs` | `AnchorClient` implementation |
