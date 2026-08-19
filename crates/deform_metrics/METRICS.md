# Metrics

Every row also has `t_us` (wall clock) and `tick`.

## Plots — one number, sampled over time

| Name | What it is |
|---|---|
| `local_input_delay` | µs from you pressing a key to the tick that used it being simulated |
| `input_to_commit` | µs from you pressing a key to those inputs being sent |
| `commit_batch_ticks` | how many ticks of input went out in one send |
| `RTT` | ms round trip to the server (QUIC: quinn's estimate. FoC: whichever probe is built in) |
| `min ticks ahead` / `max ticks ahead` | how far ahead of the server we aim to run / the hard limit |
| `sleep_time` | ms the loop decided to wait before the next tick (bigger = slowing down on purpose) |
| `advance_sim` | the local tick number, each time we simulate one |
| `last_tick_slot` | the server's tick number, each time an update arrives |
| `remote_tick (clean)` | same, but only after old/repeat updates are filtered out |
| `commit_inputs` | newest tick number in the batch being sent |
| `current_vs_remote_adv` | how many ticks ahead of the server we are, measured when we simulate |
| `current_vs_remote_reception` | same, measured when an update arrives |
| `visual_t` | how far between two sim states the picture is drawn (0 = old state, 1 = new one) |
| `datagram_fragments` | how many pieces one message was split into |
| `datagram_body_bytes` | size of a message after compression |
| `compression_ratio` | compressed size ÷ original size |

## Spans — how long a piece of code took (µs, named `<name>_us` in the CSV)

| Name | What it is |
|---|---|
| `sim_compute` | running the game's `advance_frame` once |
| `advance_local_simulation` | one whole tick: the above plus input bookkeeping (QUIC only) |
| `commit_inputs` | building and sending one input batch (QUIC only) |
| `process_server_update` | handling one update from the server (QUIC only) |
| `update_ticks_ahead` | recomputing the target lead from RTT (QUIC only) |
| `compute_dilated_tick_interval` | picking the next tick's length (QUIC only) |

## Events — things that happened, with details

| Name | What it is |
|---|---|
| `rollback` | server inputs didn't match our guess. `depth` = ticks thrown away and redone, `magnitude` = how far the world jumped in game units, `corrections_discarded` = how many jumps were too big to smooth, so the player saw a snap |
| `gap` | server tick numbers skipped. `missed` = how many never arrived (packet loss) |
| `fast_forward` | server got ahead of us. `jump` = by how many ticks. Worse than a rollback: smoothing is reset, so the world teleports |

## Metrics that go together

- **`local_input_delay` vs `input_to_commit`** — the first is what you feel locally, the second is when the network hears about it. In QUIC they're close. In FoC the gap is the batching wait.
- **`input_to_commit` vs `commit_batch_ticks`** — a bigger batch means inputs waited longer before being sent.
- **`sleep_time` vs `visual_t`** — when `sleep_time` grows, the game slows down and `visual_t` should still sweep 0→1 smoothly. If it sits at 1, the picture is frozen.
- **`sim_compute` vs `advance_local_simulation`** — the difference is framework overhead per tick, not your game code.
- **`rollback.depth` vs `sim_compute`** — multiply them to estimate how long a rollback blocked the loop.
- **`rollback.magnitude` vs `corrections_discarded`** — big jumps that get discarded are the ones the player actually sees.
- **`RTT` → `min ticks ahead` → `sleep_time`** — a chain: latency sets the target lead, the lead decides whether the loop slows down.
- **`current_vs_remote_adv` vs `min/max ticks ahead`** — where we actually are versus where we want to be. Hitting `max` means the sim froze; dropping below `min` leads to a `fast_forward`.
- **`gap` vs `rollback`** — a gap always forces a rollback, so gaps explain rollbacks that aren't misprediction.
- **`datagram_fragments` / `datagram_body_bytes` / `compression_ratio`** — all about message size. Fragments above 1 means the state is too big for one packet.
