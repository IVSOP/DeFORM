# DeFORM netcode reference

A record of how the algorithm works, written for thesis use. Every claim about behavior
cites the file and line that produces it. Numbers derived by hand are marked as such.

Line numbers refer to commit `0493bf1`. Function names are given alongside them, since line
numbers drift and names do not.

---

## 1. The model

DeFORM runs **client-side prediction with rollback**. The client simulates ahead of the
authority. When the authority disagrees, the client rewinds and re-simulates.

The authority is a QUIC server (`deform_quic/src/server/matches.rs`) or a MagicBlock
ephemeral rollup running the `tick` instruction
(`anchor_program/programs/anchor_program/src/instructions/tick.rs`). Both behave the same
way. This document calls either one the **server**.

### Relation to GGPO

The model belongs to the GGPO family but differs in two ways.

GGPO runs peer to peer and sends only inputs. No server exists. Every peer runs the same
deterministic simulation and every peer rewinds.

DeFORM has a server, and that server sends the **full game state** each tick. Only the
client rewinds. So DeFORM combines GGPO-style prediction with authoritative state sync.

---

## 2. Vocabulary

One name per concept, used throughout.

| Term | Meaning |
| --- | --- |
| tick | one fixed step of the simulation, `TICK_RATE_MICROS` long |
| server tick | the tick the authority has simulated up to |
| local tick | the tick the client has simulated up to |
| lead | `local_tick - server_tick` |
| snapshot | an authoritative state the server sent |
| extrapolate | compute a tick using guessed inputs |
| interpolate | compute a state between two states already held |
| rewind | return the simulation to an earlier tick |
| rollback | a rewind followed by re-simulation forward |

---

## 3. Inputs are bound to ticks

An input belongs to a specific tick. The server must run that tick with that input.
An input that arrives after the server ran its tick is not late. It is **wrong**, because
the server already computed that tick from something else.

### Why the binding exists

The chain re-executes. Every validator must reach the same state from the same stored
inputs, and an auditor must reproduce it later.

"Apply the input when it arrives" depends on network arrival order. That order differs per
node and nothing records it. The result stops being reproducible.

A tick number is the only clock every node shares. Inputs accounts are keyed by tick, and
`advance_tick` reads the entry for `current_tick` (`tick.rs:131-137`).

The crank makes this stricter. It fires irregularly, so `tick` derives how many ticks to run
from the slot delta. Wall-clock arrival is not observable on chain in any reproducible way.

**This is the root constraint.** Section 12 traces the rest of the design back to it.

---

## 4. Running ahead

The client simulates ahead so its inputs arrive before the server needs them. It does not
target a lead directly. It targets the **server's input buffer**: how many of this client's
inputs are queued for ticks the server has not run yet.

The server reports it on every state update. In QUIC that is
`StateUpdatePacket::player_input_buffer_len`, measured in the match loop right after the
tick consumed its input and dropped everything at or below it
(`deform_quic/src/server/matches.rs:232`). In FoC it is the length of the player's inputs
account, which the `tick` instruction drains one entry at a time.

A buffer of 0 means the next server tick has nothing to apply and calls `predict()`, which
costs a rollback. One is therefore the failure threshold, not an operating point:

```rust
// deform_quic/src/client.rs, compute_dilated_tick_interval
target = TARGET_BUFFER + JITTER_SLACK + rollback_panic;   // 1.0 + 0.5 + [0, 2]
```

`TARGET_BUFFER` is fixed by the server's consume-one-per-tick behaviour and is a private
const. `JITTER_SLACK` is the margin on top, an associated const on `DeformQuicLogic` /
`DeformFocLogic` so a game can raise it.

Whatever lead this implies is whatever the network happens to need. Nothing measures RTT to
derive it.

### The withheld tick

`commit_inputs` only ships ticks strictly below `local_tick`, because the server merges
first-write-wins and a half-finished sample for the in-progress tick would be locked in
while the client is still merging newer ones.

So the newest input ever sent is for `local_tick - 1`. Under the old RTT-derived formula
this had to be repaid explicitly with a `+1` term. The buffer controller absorbs it for
free: a withheld tick simply shows up as one less input in the server's queue, and the
target is expressed in that same unit.

At 20 Hz the withheld tick still costs 50 ms of prediction horizon. Removing it means
changing the server's merge semantics for the current tick, not tuning a number.

### What RTT is still for

Two things, neither of them pacing:

- the size of the catch-up burst at match start, before any buffer report exists
- `stats.ping_ms`, for display

QUIC reads quinn's estimate every `RTT_SAMPLE_INTERVAL_MS`. FoC times a commit from send to
when it appears in the inputs account, which includes inclusion latency.

### The horizon in steady state

The lead is an outcome rather than a target: it settles wherever keeping ~1.5 inputs queued
requires. On localhost that is small; on a 100 ms link it is proportionally larger, without
anything computing it. `MAX_PREDICTION_TICKS` is a ceiling that steady state never touches.

This governs how wrong a remote entity can be. At `PADDLE_SPEED = 480`, three ticks of
extrapolation is about 72 units on a 1000-unit field.

### Time dilation

The tick interval is stretched or squeezed so the buffer estimate converges on the target
(`compute_dilated_tick_interval`). Both flanks are `tanh`, so the correction saturates
instead of overshooting:

```
behind = max(target - buffer_estimate, 0)
ahead  = max(buffer_estimate - target - SLOWDOWN_DEADZONE, 0)
rate   = 1 + TIME_DILATION * tanh(behind / SPEEDUP_SOFTNESS)
           - TIME_DILATION * SLOWDOWN_RATIO * tanh(ahead / SLOWDOWN_SOFTNESS)
interval = TICK_RATE_MICROS / rate
```

The two `max` clamps make the branches mutually exclusive, so between `target` and
`target + SLOWDOWN_DEADZONE` both terms are zero and the interval is exactly the base rate.

Everything is deliberately lopsided, because sitting too far ahead only costs input lag
while falling behind costs rollbacks. Speedup is capped at twice the slowdown, reaches most
of its range within one tick of error where slowdown takes three, and only the slowdown side
has a dead zone.

`buffer_estimate` is where the pessimism lives: an EWMA that falls fast (`BUFFER_FALL`,
0.60) and rises slowly (`BUFFER_RISE`, 0.05), so it tracks the low end of the reports rather
than their average. A dip means the server nearly starved; one high report proves nothing
about the next tick. That asymmetry also absorbs variance on its own — an EWMA with unequal
gains rests below the mean by an amount proportional to the spread — which is why the slack
above it is a plain constant rather than a measured one.

A rollback whose mismatching inputs were *ours* is direct evidence the buffer ran dry. It
adds `ROLLBACK_KICK` to `rollback_panic` (capped at `PANIC_MAX`), raising the target — dead
zone and all — then decays at `PANIC_DECAY` per update, so the extra lead is shed through
the gentle slowdown flank instead of snapping back.

Past `MAX_PREDICTION_TICKS` (30) the simulation stops entirely until the server catches up.
Only reachable when the server goes quiet: with no reports arriving there is nothing for
dilation to steer on, so the constant bounds both the eventual fast-forward and the memory
held by `info_per_tick`.

A lag spike therefore shows up as a brief slow-motion moment, not a freeze followed by a
large rollback.

---

## 5. Prediction

To advance a tick the client needs every player's input. It has only its own.

```rust
// deform_quic/src/client.rs:743-753
*inputs = if *player == self.player {
    if let Some(provided) = self.inputs.get(&current_tick) { provided.clone() }
    else { inputs.predict() }
} else {
    inputs.predict()
}
```

`predict` chains off its own output, once per extrapolated tick.

### The server extrapolates with the same function

When no input arrives for a tick, both authorities call `predict()` on the last applied
input:

- QUIC: `server/matches.rs`, the `last_applied_inputs` branch
- chain: `tick.rs`, the `else` arm of `advance_tick`

Both sides must use the same function. If they differ, a merely missing input desyncs every
client from the server and forces a rollback the game never caused.

### Do not damp a held axis

Damping looks attractive. Because `predict` chains, a multiplier bounds the extrapolated
travel instead of letting it grow with the horizon.

It backfires. The server uses the **real** input whenever one arrives, which is the normal
case. A damped guess therefore disagrees with the server on every tick an input is held,
which is the most common state in play. That is a rollback per tick.

`PongInputs` keeps the default clone for this reason. Fix the visual artifact with
`motion_ratio` instead (section 8).

### What `predict` is for

Zeroing edge-triggered fields. A `jump: bool` or `fire: bool` predicted as `true` every tick
makes the extrapolated player fly or fire forever.

**Open item:** `ShooterInputs` does not override `predict`, so its `fire` and `jump` fields
have this bug. The cooldown and tnua's `allow_in_air: false` bound the damage.

---

## 6. Reconciliation

An arriving authoritative state is classified in order (`ReceivedScenario`,
`deform_quic/src/client.rs`):

| Scenario | Condition | Action |
| --- | --- | --- |
| Old | `new_remote <= old_remote` | drop, stale or duplicate |
| FastForward | `new_remote > local_sim` | adopt wholesale, clear history, `on_fast_forward` |
| Gap | `new_remote > old_remote + 1` | missing ticks are not recomputed, new state is truth, `on_gap` |
| Rollback | received inputs differ from predicted | rewind and re-simulate, `on_rollback` |
| Default | `new_remote == old_remote + 1`, inputs matched | prune history only |

### What travels on a rollback

**One state.** The server sends its current authoritative state per tick. It never sends a
range.

`handle_rollback` (`client.rs:863`) then:

1. Removes the pre-rollback state at `previous_local_tick`.
2. Inserts the authoritative state at `conflicting_tick`.
3. Re-simulates `conflicting_tick .. previous_local_tick`, reusing stored inputs.
4. Hands the pre/post pair to the smoother.
5. Calls `user_logic.on_rollback`, which owns the old state because that timeline is gone.

The client derives everything after the conflict itself. This is why `advance_frame` must be
pure and deterministic.

### Who rolls back, and when

Derived from the code, and not symmetric.

Let P be a player and O an observer.

| Situation | Server uses | O predicted | Result |
| --- | --- | --- | --- |
| P's input arrives, equals prediction | the real input | same value | nobody rolls back |
| P's input arrives, differs | the real input | stale value | **O rolls back** |
| P's input is late or lost | `predict()` of last applied | `predict()` of last applied | **P rolls back**, O does not |

So a **late** input rolls back the sender. A **changed** input rolls back the watchers. Row
two is the common case, and it is the source of visible correction on remote entities.

Row three only holds because the server now extrapolates with `predict()`. Before that
change it repeated the last input verbatim, and any override of `predict` broke the match.

---

## 7. The visual layer

Two mechanisms run on every visual tick. They are distinct and easily confused.

| Mechanism | Endpoints | What it hides |
| --- | --- | --- |
| frame interpolation | two of the client's own predicted ticks | the gap between sim rate and render rate |
| rollback smoothing | a decaying offset captured on rollback | the jump when a correction lands |

Both live in the generated `apply` (`deform_derive/src/lib.rs`):

```rust
let target = lerp_toward(&prev.field, &current.field, t);   // frame interpolation
self.field *= decay;                                         // rollback smoothing
current.field = target + self.field;
```

### The endpoints

```rust
// deform_quic/src/client.rs:493-506
let prev_tick = self.local_tick.saturating_sub(1);
let t = (elapsed / TICK_RATE_MICROS).clamp(0.0, 1.0);
smoother.apply(&prev.game_state, &mut visual_state.game_state, t);
```

`prev` and `current` are `info_per_tick[local_tick - 1]` and `info_per_tick[local_tick]`.
Both are predicted states. Neither is a snapshot.

**Nothing anywhere interpolates between two authoritative states.** That absence is what
section 10 is about.

### The render sits one tick behind the compute front

The render position is `local_tick - 1 + t`. At `t = 0` it shows `local_tick - 1`. At
`t = 1` it shows `local_tick`.

This is forced. Interpolation needs a destination, and holding the destination means having
already computed it.

Consequence: up to **one tick of visual latency on the player's own input**. At 20 Hz that is
up to 50 ms. The keypress lands in `inputs[T-1]`, produces state T, and the render only
reaches T one tick period later.

lightyear pays exactly the same cost and documents it as such:

> To solve this, we visually display the state of the game with 1 tick of delay.
> — `crates/core/frame_interpolation/src/lib.rs`

It offers no extrapolating alternative. Grepping that repository for `Extrapolat` returns
nothing. The difference is default and tick rate: lightyear makes frame interpolation opt-in
and runs at 64 Hz, where one tick is 16 ms.

`local_tick` still leads the server by whatever lead keeps the input buffer at its target, so
the render remains ahead of the server even one tick back.

### Which fields get which treatment

`visual_state` starts as a clone of `current`, so unannotated fields sit at `local_tick`
exactly. Only annotated fields are pulled back and offset.

| Pong field | Treatment |
| --- | --- |
| `ball_pos`, `paddle_y` | interpolated and smoothed |
| `ball_vel` | verbatim at `local_tick` |
| `score`, `creator` | verbatim at `local_tick` |

Smoothing a velocity fights the integrator. Smoothing a score is meaningless.

### Anchor invariant

`last_sim_instant` anchors `t`, so it may only move when the simulation actually advanced.
Assigning it unconditionally restarts `t` at 0 over an unchanged tick pair, and the render
sweeps the same tick repeatedly while frozen at `MAX_PREDICTION_TICKS`.

---

## 8. Smoother parameters

Set with `#[smooth(...)]`. Generated by `deform_derive`.

| Parameter | Default | Unit | Meaning |
| --- | --- | --- | --- |
| `decay` | `0.9` | per **simulation tick** | offset multiplier |
| `max_offset` | `200.0` | distance | discontinuity threshold |
| `min_offset_sq` | `4.0` | distance squared | zero the offset below this |
| `max_correction` | unset | distance per **simulation tick** | hard step toward zero |
| `motion_ratio` | unset | dimensionless | cap the offset at N times this tick's travel |

`decay` and `max_correction` are authored per simulation tick. `scale_decay(visual / sim)`
converts them to per-frame values at construction, so the same numbers behave identically at
60 Hz and 144 Hz. `motion_ratio` is dimensionless and is not rescaled.

### Comparing decay across tick rates

`decay` alone is a trap, because its meaning depends on tick length. Compare time constants:

```
tau = tick_ms / ln(1 / decay)
```

| Configuration | tau |
| --- | --- |
| `decay = 0.9` at 20 Hz | 475 ms |
| `decay = 0.9` at 60 Hz | 158 ms |
| `decay = 0.5` at 20 Hz | 72 ms |
| `decay = 0.8` at 60 Hz | 75 ms |

The last two rows are near-identical in wall-clock terms, despite looking unrelated.

### Decay alone never finishes

Rollbacks arrive every tick, and each re-injects the current prediction error `e`. The offset
does not converge to zero. It settles at:

```
offset_steady = e / (1 - decay)
```

| `decay` | steady offset |
| --- | --- |
| `0.9` | `10 e` |
| `0.5` | `2 e` |

While `e` keeps arriving, no decay value ends the correction. This is why the two bounding
parameters exist.

### `max_correction`

Subtracts a fixed step per tick on top of `decay`. Bounds any correction to
`magnitude / max_correction` ticks, whatever re-injects it.

Starting point: `worst_case_offset / ticks_you_are_willing_to_spend`.

### `motion_ratio`

Caps the offset at a multiple of the distance the field actually moved this tick.

A correction is invisible while the object genuinely moves. Once the true state comes to
rest, any residual offset becomes the only motion on screen. With a finite ratio the
allowance falls to zero as the object stops, so a halted remote entity snaps rather than
glides.

This is the parameter that addresses "the other player keeps sliding after they stop".

### `max_offset` does two jobs

1. `on_rollback` discards an offset larger than it.
2. `apply` compares the **single-tick jump** against it and snaps rather than sweeping.

Job two makes a round reset or a respawn teleport instead of streaking across the screen. It
requires `max_offset` to sit above the fastest normal per-tick motion and below the smallest
real teleport. If those overlap, use `NoopSmoother` on that sub-struct.

### Pong worked example

20 Hz, `dt = 50 ms`, field 1000 units, paddle 120 tall.

| Quantity | Value |
| --- | --- |
| paddle travel per tick | 24 units |
| ball travel per tick | 52.5 units |
| `reset_round` ball jump | about 850 units |
| worst extrapolation error | 3 ticks x 24 = 72 units |

Chosen: `decay = 0.5`, `max_offset = 200`, `min_offset_sq = 9`, `max_correction = 40`,
`motion_ratio = 2.0`.

`max_offset` sits above 52.5 and below 850. `motion_ratio = 2.0` allows 48 units of
correction inside a moving paddle and zero inside a stopped one.

---

## 9. Parameter inheritance

A struct-level `#[smooth(...)]` applies to that type **and its whole subtree**. Any
descendant that authors its own overrides for **itself and everything below it**.

This holds for `#[smooth(nested)]` fields and `#[smooth(map)]` entries alike, including map
entries created long after the root was built and scaled.

Implementation: the smoother keeps authored params separate from scaled ones.

| Field | Meaning |
| --- | --- |
| `__params` | authored or inherited, always per simulation tick, never scaled in place |
| `__scale` | `visual_tick / sim_tick` |
| `__scaled` | derived per-frame view, read by the hot path |

Keeping `__params` unscaled makes the scale replayable, which is what allows a map entry
created after `scale_decay` ran to be scaled correctly. `scale_decay` assigns rather than
accumulates, so replaying it is idempotent.

Covered by `deform_core/tests/smooth_hierarchy.rs`. Extend it when touching the derive.

---

## 10. Comparison with Source and lightyear

### One timeline against two

Every networked game answers one question per entity: at which tick do I draw this now?

**DeFORM answers with one tick.** The client draws everything at its predicted tick. Remote
entities are therefore extrapolated, which is why corrections exist at all.

**Source and lightyear answer with two.** The local player is predicted and drawn ahead.
Remote entities are interpolated between two received snapshots and drawn behind, by
`cl_interp` (100 ms default in Source). A remote entity guesses nothing, so it cannot
mispredict, so it never rubber-bands.

| | DeFORM, GGPO | Source, lightyear |
| --- | --- | --- |
| timelines the client draws | 1 | 2 |
| remote entities | extrapolated | interpolated |
| remote entities can be wrong | yes | no |
| who rewinds the simulation | client only | client only |
| server re-simulates | no | no |
| server stores position history | no | yes |
| needs lag compensation | no | yes |
| determinism required | yes | no |
| what the server sends | full state | delta-compressed snapshots |
| cost per extra player | one more entity to extrapolate | near zero |

**Neither server rewinds the simulation.** Both run forward on a fixed schedule. A server
serves many clients at different latencies, so re-simulating for one late input would
invalidate state already sent to everyone else.

### What lag compensation actually is

Two timelines create a seam. The crosshair sits on a player drawn 150 ms in the past while
the server runs in the present.

Source closes the seam for hit tests only:

1. Store each player's hitbox positions, about one second deep.
2. Receive a fire command.
3. Compute the time that client was viewing: `server_time - latency - interp_delay`.
4. Move the other players' hitboxes back to that time.
5. Run the hit test.
6. Restore the hitboxes to the present.

Step 4 moves **hitboxes only**. The server does not rewind the world and does not
re-simulate. The cost lands on the target, who takes a hit after reaching cover, because the
shooter's rewound view still showed them exposed. The design accepts this and favors the
shooter.

### The trade has no free corner

| | needs lag compensation | remote extrapolation artifacts |
| --- | --- | --- |
| one timeline | no | **yes** |
| two timelines | **yes** | no |

No configuration avoids both.

### Why DeFORM does not need it today

Lag compensation requires three things at once: weapons that resolve instantly, remote
entities drawn stale, and a shooter aiming at that stale picture.

Neither example has any of them. Pong couples the ball to both paddles every tick, so no
discrete moment exists to rewind to. The shooter fires physics projectiles with real flight
time, so no instant trace exists to compensate.

### How lightyear implements it, if ever needed

Read from the cloned source.

The client computes the delay by subtracting its own two timelines
(`crates/inputs/inputs/src/client.rs`):

```rust
let mut delay = input_timeline.now() - interpolation_timeline.now();
if delay.is_negative() { delay = TickDelta::from(Tick(0)); }
```

It attaches that to the input message, only when `InputConfig::lag_compensation` is enabled.
The server stores it verbatim with no clamp and no cross-check
(`crates/inputs/inputs/src/server.rs`):

```rust
if let Some(interpolation_delay) = message.interpolation_delay {
    commands.entity(client_entity).insert(interpolation_delay);
}
```

The only bound is history depth, `max_collider_history_ticks`, default 35, about 500 ms at
64 Hz. A claim past that finds no history entry and the shot misses. That is fail-closed,
not a clamp.

Two implementation details worth keeping:

- **The rewind lands between ticks.** `tick_and_overstep` returns a tick plus a fraction, and
  the query lerps position and slerps rotation between two history entries. The client was
  itself drawing a lerp, so a whole-tick rewind would reconstruct a pose nobody saw.
- **The query is two-phase.** Broad phase casts against an AABB envelope covering the whole
  history. Only on a hit does it look up the historical pose and cast against the real
  collider.

### If DeFORM ever adds it

The rewind is measured from the input's **own tick**, not from arrival time. With lead `A`
and interpolation delay `B`, a shot tagged for tick `T` must resolve against tick
`T - A - B`, which is `A + B` ticks in the server's past. The server has already simulated
it, so there is something to rewind to.

Sketch of what would change:

1. The client keeps confirmed snapshots. `info_per_tick` holds predicted states only.
2. The input carries the tick the client was drawing, with a quantized sub-tick fraction.
   `DeformInputs` requires `Eq`, so no `f32`. Follow the `yaw_q` and `pitch_q` pattern.
3. That field must be excluded from the equality check, or every observer rolls back every
   tick trying to predict a value nobody can predict.
4. `advance_frame` gains a history parameter. It stays pure, so rollback still works.
5. The server stores collision-relevant fields for `A + B + margin` ticks.

On chain, points 4 and 5 are the expensive ones. History must live on chain and every
validator must replay the rewind, inside consensus.

### Trust

The rewind target splits into terms with different trust properties:

```
interp_delay = lead + transit + buffer
```

- `lead` is derivable. The server knows the input's tick and its own tick at arrival.
- `transit` is server-measurable off chain. It is **not** measurable on chain, because a
  per-node RTT is not consensus data.
- `buffer` should be a protocol constant. Then no client reports it and no client can lie
  about it.

Two of those three scale with client latency, so a single global constant cannot replace
them. Rewind too far and victims die behind cover more than they should. Rewind too little
and a correctly-aimed shot misses.

Per-tick keying of whatever the client does send defends against **jitter and replay
divergence**, not against lying. Only a clamp defends against lying. Keep the two purposes
separate.

---

## 11. Cheating surface

Clients send inputs, not state. A client cannot set its own position. Cheating enters
elsewhere.

**The server tells you too much.** Full state reaches every client, so wallhack-class cheats
just render what already arrived. On chain this is absolute: the state is public by
construction, so reading it is not even cheating.

**The client automates aiming.** An aimbot uses legitimately received data. The server sees a
well-aimed input and cannot distinguish a human from a script.

**The client lies about timing metadata.** Only relevant if lag compensation is added. Source
clamps both interp ratio and total unlag because the values are unverifiable. A lag switch
stays undetectable from the packet alone, since the packet is honest and only the network
behavior is not.

---

## 12. Everything traces back to one constraint

The chain must recompute the game from stored inputs. Therefore:

1. Inputs bind to ticks, because arrival order is not reproducible.
2. A shared tick number makes one timeline natural.
3. One timeline forces remote entities to be extrapolated.
4. Extrapolation produces corrections.
5. The smoother exists to hide those corrections.

Snapshot interpolation would relax determinism. That buys nothing here, because the chain
already requires it.

---

## 13. Open items

| Item | Status |
| --- | --- |
| `ShooterInputs` does not override `predict`, so `fire` and `jump` extrapolate as true forever | open |
| `advance_frame` history parameter for lag compensation | not started, needed only if hitscan is added |
| Client retention of confirmed snapshots | not started, needed for interpolated entities |
| Self-input render lag of up to one tick | accepted, see section 7 |
| `commit_inputs` withholding the in-progress tick, costing one tick of horizon | accepted, structural |
