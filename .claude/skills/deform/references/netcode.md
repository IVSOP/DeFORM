# How the netcode works

You do not have to read this to use DeFORM — the backends implement all of it and your game
only sees `advance_frame` and `read_state`. It is worth knowing because it explains *why* the
traits are shaped the way they are, and what "deterministic" actually costs you.

The model is **client-side prediction with rollback**, the same family as GGPO/rollback
fighting games, adapted so the authority can be a Solana ephemeral rollup.

## The loop

Each client keeps:

- `local_tick` — how far its own simulation has run
- `info_per_tick: HashMap<u64, TickInfo<T>>` — the history it can rewind into
- `inputs: HashMap<u64, T::Inputs>` — its own inputs, keyed by the tick they apply to
- `remote_lobby` — the last authoritative state received

Two independent timers run:

- **simulation tick** every `TICK_RATE_MICROS` — advance one tick
- **visual tick** every `visual_tick_micros` — interpolate and publish to the shared state

Plus a commit timer that ships accumulated inputs to the authority (a QUIC datagram, or a
`set_inputs` transaction).

## Running ahead

The client deliberately simulates **ahead** of the authority, so that by the time its inputs
arrive, the authority is at the tick those inputs were meant for.

```rust
min_ticks_ahead = ceil((rtt_micros + 3000) / TICK_RATE_MICROS) + 1
max_ticks_ahead = max(3 * min_ticks_ahead, 5)
```

A **full** RTT, not half: the authoritative state you just received is already RTT/2 old, and
your inputs need another RTT/2 to get back — so the authority advances a full RTT between the
state you saw and the state your input lands in. The `+3000 µs` and `+1` absorb commit-timer
jitter. Recomputed on every RTT sample (500 ms for FoC; QUIC uses quinn's own RTT estimate).

For FoC there is an extra floor: transaction inclusion can't beat the slot time, so the slot
duration is folded into the target.

## Prediction

To advance a tick, the client needs everyone's inputs, but only has its own:

- **own inputs** — taken from `self.inputs[tick]` if present, else `predict()`ed from the
  previous tick
- **other players' inputs** — always `predict()`ed from their last known inputs

This is why `DeformInputs::predict()` exists as an override point. The default clone is right
for continuous state (a held direction), wrong for edge-triggered actions — a `jump: bool`
predicted as `true` every tick makes the predicted player fly. Zero those fields in `predict`.

Prediction is also why `advance_frame` must be pure and deterministic: it is going to be
re-run over the same ticks with corrected inputs, and its output must match what the
authority computed byte for byte.

## Reconciliation

When an authoritative state arrives, the client classifies it (see `ReceivedScenario` in
`deform_quic/src/client.rs`, evaluated in order):

| Scenario | Condition | Action |
| --- | --- | --- |
| **Old** | `new_remote <= old_remote` | drop it (stale or duplicate datagram) |
| **FastForward** | `new_remote > local_sim` | the authority is ahead of us; take its state wholesale, clear history, `on_fast_forward` |
| **Gap** | `new_remote > old_remote + 1` | authoritative ticks were skipped; the missing ones are *not* recomputed — the new state is truth. Also triggers a rollback, and `on_gap` |
| **Rollback** | received inputs ≠ predicted inputs | rewind and re-simulate, `on_rollback` |
| **Default** | `new_remote == old_remote + 1`, inputs matched | just prune history; nothing to fix |

A rollback:

1. adopts the authoritative `TickInfo` at tick N as the new truth,
2. replays ticks N+1 … `local_tick` through `advance_frame`, using the real inputs where
   known and predictions elsewhere,
3. hands the smoother the pre/post state pair so the visual correction is absorbed as a
   decaying offset instead of a teleport,
4. calls `on_rollback(old_info, &new_info)` — `old_info` is owned, because that timeline no
   longer exists.

**This is the reason `DeformUserLogic` is a separate object from `GameState`.** Everything in
`GameState` is rewound and recomputed; `&mut self` on `advance_frame` is not. Data that must
survive corrections (counters, per-match config, IDs) goes in the logic type.

Gaps are benign over a reliable transport (a WebSocket feed of account updates rarely skips)
and expected over UDP datagrams. Since a rollback is always emitted alongside, `on_gap` can
be safely ignored.

## Time dilation

If the client drifts too far ahead of the authority (the server hitched, the ER fell behind),
it doesn't hard-stop — it *stretches* its tick interval so the authority catches up smoothly.

The percentage overshoot past `min_ticks_ahead` scales the sleep between ticks. Example: with
a target of 10 ticks ahead, being at 10 is 0 % dilation, being at 20 is a full step of
dilation. Past `max_ticks_ahead` (3× the target, minimum 5) the simulation stops entirely
until the authority catches up. Below the target it fast-forwards.

The result is that a lag spike shows up as a slightly slow-motion moment rather than a freeze
followed by a huge rollback.

## Visual state vs simulation state

The simulation runs at a fixed `TICK_RATE_MICROS`. Rendering does not. Every visual tick the
backend:

1. clones the current tick's `game_state`,
2. computes `t = elapsed_since_last_sim_tick / TICK_RATE_MICROS`, clamped to `0..1`,
3. calls `smoother.apply(&previous_state, &mut visual_state, t)` — lerp toward the current
   tick, plus whatever rollback offset is still decaying,
4. writes that into `DeformSharedBackendState.lobby`.

So `client.read_state()` always gives you a presentation-ready state; there is no need to
interpolate in your renderer. The smoother's decay is rescaled by
`visual_tick_micros / TICK_RATE_MICROS` at construction so a 144 Hz client and a 60 Hz client
converge at the same real-world speed.

## Authority-side differences

- **QUIC server** (`deform_quic/src/server/matches.rs`): one `match_loop` task per match.
  It applies whatever inputs arrived for the current tick (defaulting to the last known
  inputs), runs `advance_frame`, broadcasts the new `LobbyState` to every connected client as
  a datagram, and on `has_ended()` runs `on_match_end` and drops the match.
- **On-chain** (`anchor_program/.../instructions/tick.rs`): a scheduled crank calls `tick`.
  Because it can be called irregularly, it derives how many ticks to run from elapsed slots:
  `num_ticks = max(1, slot_delta * micros_per_slot / TICK_RATE_MICROS)`. It reads each
  player's inputs account, applies inputs for the current tick, prunes anything older, runs
  the frame, and writes the lobby and inputs accounts back.

Both consume `inputs` keyed by tick number, which is why clients batch-resend recent inputs:
a lost datagram or dropped transaction is recovered by the next commit, with no
retransmission logic.
