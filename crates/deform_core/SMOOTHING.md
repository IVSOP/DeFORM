# Recursive smoothing

`#[derive(Smooth)]` creates a `TypeSmoother` and connects it to the state through
`Smoothable::Smoother`. Each `#[smooth]` field participates; unmarked fields always
keep their current simulation value.

```rust
use std::collections::HashMap;
use deform_core::{Smooth, Smoothable};

#[derive(Clone, Smooth)]
struct Player {
    #[smooth]
    position: glam::Vec2,
    score: u32,
}

#[derive(Clone, Smooth)]
#[smooth(decay = 0.8, max_offset = 10.0, min_offset_sq = 0.0004)]
struct World {
    #[smooth]
    players: HashMap<u32, Player>,
    round: u32,
}
```

The same field annotation works for `f32`, `f64`, `glam::Vec2`, `glam::Vec3`,
derived structs, and `HashMap` values. Maps may contain other maps or scalar
values, and type aliases and custom hashers work. Derived structs support type,
lifetime, and const parameters. Only selected fields need to be smoothable.

Selection is explicit at every level: marking `players` opts into traversing the
map, then `Player` selects `position`. Neither `score` nor `round` interpolates.
If `players` were unmarked, its positions would not interpolate either.

## Inheritance

Parameters are authored in simulation-tick units. Each child inherits its parent's
settings and replaces only the parameters declared on its own type. For example,
a child with `#[smooth(decay = 0.5)]` inside the world above uses `decay = 0.5`,
`max_offset = 10.0`, and `min_offset_sq = 0.0004`. Those effective settings also
reach the child's descendants, including map entries created later.

| Parameter | Root default | Behavior |
| --- | --- | --- |
| `decay` | `0.9` | Fraction of rollback offset retained each simulation tick; range `[0, 1]` |
| `max_offset` | `200.0` | Corrections and single-tick movements above this distance snap |
| `min_offset_sq` | `4.0` | Residual offsets below this squared distance clear |
| `max_correction` | disabled | Extra distance removed each tick after exponential decay |
| `motion_ratio` | disabled | Caps the offset to this multiple of the distance moved this tick |

`max_correction` retains its historical name for compatibility. It is an
additional linear reduction, **not a maximum correction speed**. Zero disables
this reduction. `motion_ratio` is dimensionless; when enabled, a stationary field
loses its entire residual offset. The runtime represents disabled optional
settings with `f32::INFINITY`.

## Driving a smoother

1. Construct `WorldSmoother::default()` (or use the associated smoother type).
2. Call `scale_decay(visual_tick_micros / sim_tick_micros)` using floating-point
   division. This sets the ratio; repeated calls do not compound it. Decay becomes
   `decay.powf(ratio)` and the linear correction step becomes `step * ratio`.
3. On a rollback, call `on_rollback(&before, &after)` with simulation snapshots.
4. Each visual frame, clone the current simulation snapshot and call
   `apply(&previous_snapshot, &mut visual_clone, t)`, where `t` runs from zero to
   one over the simulation interval. Never feed the visual result back into the
   simulation or use it as an interpolation endpoint.
5. Call `reset()` for a hard reset or fast-forward. Configuration and frame ratio
   remain in effect.

A field normally renders as the interpolation between its snapshots plus its
decayed rollback offset. A movement above `max_offset` instead keeps the current
value and clears the offset. Rollback differences accumulate with the residual
offset; totals above the threshold are discarded.

Maps match entities by key. An entity absent from the previous snapshot appears
at its current value. Removed entities release their smoother during both visual
updates and rollbacks. Keys must identify a continuous entity lifetime: use a new
key for a replacement entity when both snapshots still contain the old key.

`correction_magnitude_sq()` sums the remaining squared offsets.
`corrections_discarded()` counts oversized rollback corrections cumulatively;
removing map entries or resetting offsets does not reduce it.

## Implementation and migration

The derive consists of attribute parsing (`deform_derive/src/parse.rs`) and
struct delegation (`deform_derive/src/expand.rs`). Numerical behavior lives in
`deform_core/src/smooth/field.rs`, and map lifecycle handling lives in `map.rs`.
There is no numerical algorithm emitted by the derive.

Existing `#[smooth(nested)]` and `#[smooth(map)]` spellings remain aliases for
`#[smooth]`, and the backend `Smooth` interface and generated `TypeSmoother` names
are unchanged. There are two intentional changes for code using internals:

- A partial type configuration now inherits unspecified settings; previously,
  declaring any parameter reset all other settings to global defaults.
- Generated smoother fields are private implementation details. Use `Smooth`
  methods for configuration and metrics instead of accessing offsets directly.

For a custom arithmetic leaf, implement `SmoothableField` plus
`Clone + Default + Send + Add<Output = Self> + Sub<Output = Self>` (`Default` must
be zero), and implement `Smoothable` with
`type Smoother = deform_core::smooth::FieldSmoother<Self>`. The default leaf
smoother scales offsets by interpolating from zero, so a custom field's lerp must
be linear. Custom behavior can instead implement `Smooth` and use that smoother
as the associated type.
