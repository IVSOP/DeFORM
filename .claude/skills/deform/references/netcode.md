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

The lead is not targeted directly. What the client steers is the authority's **input
buffer**: how many of the client's inputs are queued for ticks the authority has not run
yet. Every state update reports it — explicitly in the QUIC packet
(`player_input_buffer_len`), and in FoC as the length of the player's inputs account,
which the `tick` instruction drains one entry at a time.

A buffer of 0 means the next authoritative tick has nothing to apply and will `predict()`,
which costs a rollback. So the setpoint is **1, plus `JITTER_SLACK`** — one is the failure
threshold, not a place to sit. Whatever lead that setpoint implies is whatever the network
happens to need; nothing measures RTT to derive it.

The client never sends an input for the tick still in progress: `commit_inputs` ships only
ticks strictly below `local_tick`, because the authority merges first-write-wins and a
half-finished sample would be locked in while newer ones are still being merged. The newest
input in flight is always for `local_tick - 1`, and the buffer controller absorbs that
automatically rather than needing a `+1` in a formula.

RTT is still measured, but only for two things: the size of the catch-up burst at match
start (dilation can hold a lead, not create one), and `stats.ping_ms`. QUIC reads quinn's estimate; FoC times a commit from send to when it
appears in the inputs account.

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

The tick interval is stretched or squeezed so the buffer converges on its setpoint. Both
flanks of the response are `tanh`, so the correction saturates instead of overshooting:

```
target = TARGET_BUFFER + JITTER_SLACK + rollback_panic     // 1.0 + 0.5 + [0, 2]
behind = max(target - buffer_estimate, 0)
ahead  = max(buffer_estimate - target - 1, 0)
rate   = 1 + TIME_DILATION * tanh(behind / 1)
           - TIME_DILATION * 0.5 * tanh(ahead / 3)
interval = TICK_RATE_MICROS / rate
```

`TARGET_BUFFER` is fixed by how the authority consumes inputs, so it is a private const.
`JITTER_SLACK` is the margin on top and is an associated const on `DeformQuicLogic` /
`DeformFocLogic`, overridable per game.

Everything about it is deliberately lopsided, because sitting too far ahead only costs input
lag while falling behind costs rollbacks: speedup is capped at twice the slowdown, it reaches
most of its range within a single tick of error where slowdown takes three, and only the
slowdown side has a dead zone.

`buffer_estimate` is where the pessimism lives. It is an EWMA that falls fast (0.60) and
rises slowly (0.05), so it tracks the low end of the reports rather than their average — a dip
means the authority nearly starved, while one high report proves nothing about the next tick.

That asymmetry also handles variance on its own, which is why the slack above it is a plain
constant. An EWMA with unequal gains settles where the pull from each side balances
(`0.05 * E[above] = 0.60 * E[below]`), and that resting point sits below the mean by an amount
proportional to the spread: a noisier link drags the estimate further down without anything
having to measure the noise. An earlier version added a second, jitter-derived slack on top
and was double-counting.

A rollback caused by *our own* inputs mismatching is direct evidence the buffer ran dry, more
sharply than any report shows. It adds 1 to `rollback_panic` (capped at 2), which raises the
target — dead zone and all — and decays with a half-life of ~15 updates, about as long as a
10 % speedup needs to win back a tick of buffer. The extra lead is then shed gently through
the slowdown flank instead of snapping back.

Past `MAX_PREDICTION_TICKS` the simulation stops entirely until the authority catches up.
That is not part of the control loop and is only reachable when updates dry up altogether:
with no reports arriving, dilation is steering on a frozen estimate, so the constant bounds
both the eventual fast-forward and the memory held by `info_per_tick`.

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
