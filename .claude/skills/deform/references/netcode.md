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
state you saw and the state your input lands in. Recomputed on every RTT sample (500 ms for
FoC; QUIC uses quinn's own RTT estimate).

The `+1` is **not** a safety margin — it repays a tick the commit path withholds.
`commit_inputs` only ships ticks strictly below `local_tick`, because the authority merges
first-write-wins and a half-finished sample for the in-progress tick would be locked in while
you are still merging newer ones. So the newest input you ever send is for `local_tick - 1`,
and the requirement works out to `ticks_ahead ≥ RTT/tick + 1`. That makes the formula minimal,
not conservative: on localhost `RTT/tick ≈ 0`, so you sit at **2 ticks ahead**, one for
latency and one for the withheld tick. At 20 Hz that withheld tick costs 50 ms of prediction
horizon — the largest single constant in the budget. Getting to 1 means changing the
authority's merge semantics for the current tick, not tuning a number.

The `+3000 µs` is nearly free: `ceil()` already rounds any partial tick up, so the margin only
changes the result when it crosses an integer boundary.

### What the horizon actually is

`max_ticks_ahead` is a *ceiling*, not the operating point. The catch-up burst pulls you back
to `min_ticks_ahead`, and dilation does not engage until 30 % into the `min..max` window, so
in steady state you sit between `min` and `min + 0.3 × window`. On localhost at 20 Hz that is
~2–3.2 ticks (100–160 ms), not 6. `max_ticks_ahead` only matters during a hitch.

This is the number that governs how wrong a remote entity can be: at `PADDLE_SPEED = 480`,
3 ticks of extrapolation is ~72 units on a 1000-unit field.

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

**The authority extrapolates with the same `predict`.** When no input arrives for a tick, the
QUIC server (`server/matches.rs`) and the on-chain `advance_tick` both call `predict()` on the
last applied input, exactly as clients do for players they have no input for. Both sides must
use the same function. If they differ, a merely missing input desyncs every client from the
authority and forces a rollback the game never caused.

**Do not damp a held axis to shrink the extrapolation.** It looks appealing, because `predict`
chains off its own output once per extrapolated tick, so a multiplier bounds the predicted
travel instead of letting it grow with the horizon. It backfires. The authority uses the *real*
input whenever one arrives, which is the normal case, so a damped guess disagrees with the
authority on every tick the input is held. That is a rollback per tick during the most common
state in the game. Fix the visual artifact with `motion_ratio` instead (see `smoothing.md`).

What `predict` *is* for: zeroing edge-triggered fields. A `jump: bool` or `fire: bool` predicted
as `true` every tick makes the extrapolated player fly or fire forever.

**Reducing prediction error beats smoothing it.** No `#[smooth(...)]` parameter can fix a
state that is persistently wrong; it only chooses between a visible slide and a visible pop.

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

Note what this model is: DeFORM predicts **every** entity and absorbs the correction, the
GGPO/fighting-game approach. It assumes corrections are short — 2–4 frames at 60 Hz. At 20 Hz
the same 2–4 ticks is 100–200 ms, four times the window the model was designed for, which is
why remote-entity smoothing is far more visible in a 20 Hz game.

The other industry model is **snapshot interpolation** (Quake/Source and most FPS games):
predict only the local player, and render every remote entity *in the past*, lerped between
two received authoritative snapshots (Valve's `cl_interp`, default 100 ms). Remote entities
then cannot rubber-band, because they are never extrapolated. It costs nothing in local input
lag — only in how recently you see everyone else. DeFORM does not implement this today; it
would need the backend to expose authoritative history to the renderer, since the smoother is
only ever handed predicted states.

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
