# Interpolation and rollback smoothing (`deform_derive`)

A rollback rewrites the recent past. Without help, a mispredicted paddle *teleports* to its
corrected position the frame the correction arrives. The smoother solves two problems at
once:

1. **Interpolation** — the simulation runs at a fixed tick rate (often 20 Hz on-chain) while
   rendering runs at whatever your display does. The smoother lerps between the previous and
   current tick so motion is continuous.
2. **Rollback absorption** — when a correction lands, the visual/simulated delta is captured
   as an *offset* that is added to the rendered state and decays exponentially. The object
   keeps rendering where it was and slides into the corrected position over a few frames.

Both happen inside the backend. Your renderer just reads the already-smoothed state.

## The trait

```rust
pub trait Smooth<G>: Default + Send + Clone {
    fn reset(&mut self);                                   // hard reset (fast-forward)
    fn on_rollback(&mut self, pre: &G, post: &G);          // capture the correction offset
    fn apply(&mut self, prev: &G, current: &mut G, t: f32);// lerp + decay, in place
    fn scale_decay(&mut self, ratio: f32);                 // visual_tick / sim_tick
    fn set_params(&mut self, params: SmoothParams);
}

pub struct SmoothParams {
    pub decay: f32,
    pub max_offset_sq: f32,
    pub min_offset_sq: f32,
    pub max_correction: f32,   // f32::INFINITY = unbounded (pure exponential decay)
    pub motion_ratio: f32,     // f32::INFINITY = offset never capped by how fast the field moves
}
```

`type Smoother` on `DeformUserLogic` must be a `Smooth<Self::GameState>`. Three options:

- `#[derive(Smooth)]` on your game state → use the generated `FooSmoother`
- `NoopSmoother` → no interpolation, no absorption (state snaps to the simulation tick)
- your own impl → total control

## `#[derive(Smooth)]`

For a struct `Foo`, generates `FooSmoother`, `impl Smooth<Foo> for FooSmoother`, and
`impl Smoothable for Foo` (which is how nested/map fields find a child smoother). Named
structs only.

### Struct-level parameters

```rust
#[derive(Smooth)]
#[smooth(decay = 0.5, max_offset = 200.0, min_offset_sq = 9.0,
         max_correction = 40.0, motion_ratio = 2.0)]
struct GameState { /* … */ }
```

| Parameter | Default | Meaning |
| --- | --- | --- |
| `decay` | `0.9` | offset multiplier per **simulation tick** — lower snaps harder |
| `max_offset` | `200.0` | discontinuity threshold: rollback offsets above it are dropped, **and** single-tick jumps above it snap instead of interpolating |
| `min_offset_sq` | `4.0` | squared magnitude below which the offset is zeroed, so it doesn't crawl forever |
| `max_correction` | unset | max distance the offset is pulled toward zero per **simulation tick**, on top of `decay` |
| `motion_ratio` | unset | caps the offset at this multiple of the distance the field moved this tick |

`decay` and `max_correction` are authored **per simulation tick**; the backend calls
`scale_decay(visual_tick / sim_tick)` at construction to convert them to per-frame values, so
the same numbers behave identically at 60 and 144 Hz. Never compensate by hand. `motion_ratio`
is dimensionless and is not rescaled.

### Choosing them

`decay` alone is asymptotic, and rollbacks arrive *every tick*. For a per-tick prediction
error `e`, the offset does not converge to zero — it settles at `e / (1 - decay)`:

| `decay` | steady-state offset | at 20 Hz, time constant |
| --- | --- | --- |
| `0.9` | `10 × e` | 475 ms |
| `0.5` | `2 × e` | 72 ms |

So a `decay` chosen at 60 Hz means something very different at 20 Hz: `tick_ms / ln(1/decay)`
is the number to compare across tick rates, not `decay` itself.

- `max_correction` bounds how long *any* correction can last, however it is being re-fed.
  Start at `worst_case_offset / ticks_you_are_willing_to_spend`.
- `motion_ratio` targets the case the eye is most sensitive to: an offset that outlives the
  motion it was hiding inside. A correction is invisible while the object is genuinely
  moving, but once the true state comes to rest any residual offset *is* the only motion on
  screen. With a finite ratio the allowance falls to zero as the object stops, so a halted
  remote entity snaps instead of gliding. Reach for this first when "the other player keeps
  sliding after they stop".
- `max_offset` must sit above the largest distance a field covers in one tick during normal
  play, and below the smallest genuine teleport.

### Inheritance

Omitting the attribute uses the defaults *and* marks the smoother as "no custom params", so
the containing smoother's values flow into it. Specifying `#[smooth(...)]` pins your values.
The contract, in both directions:

- a struct-level `#[smooth(...)]` applies to that type **and its whole subtree**
- any descendant that authors its own `#[smooth(...)]` overrides for **itself and everything
  below it**, and is not bypassed in favour of a grandparent's values
- this holds for `#[smooth(nested)]` fields and `#[smooth(map)]` entries alike, including map
  entries created long after the root was constructed and scaled

Covered by `deform_core/tests/smooth_hierarchy.rs`; extend it if you touch the derive.

### Field attributes

Only annotated fields are smoothed. Everything else is copied through from the simulation
state verbatim — which is what you want for scores, flags, IDs, and anything discrete.

| Attribute | Field type | Behaviour |
| --- | --- | --- |
| `#[smooth]` | implements `SmoothableField` + `-`, `+=`, `*= f32` | lerp between ticks, plus offset decay |
| `#[smooth(nested)]` | a struct that derives `Smooth` | delegates to that type's smoother |
| `#[smooth(map)]` | `HashMap<K, V>` where `V: Smoothable` | one independent smoother per entry, created on demand and dropped when the key disappears |

```rust
#[derive(Default, Clone, Debug, serde::Serialize, SchemaRead, SchemaWrite, Smooth)]
pub struct PlayerState {
    #[smooth]
    pub paddle_y: f32,
    pub score: u32,        // discrete — never interpolate this
}

#[derive(Default, Clone, Debug, serde::Serialize, SchemaRead, SchemaWrite, Smooth)]
#[smooth(decay = 0.5, max_offset = 200.0, min_offset_sq = 9.0, max_correction = 40.0, motion_ratio = 2.0)]
pub struct PongGameState {
    #[smooth]
    #[wincode(with = "PodVec2")]
    pub ball_pos: Vec2,
    #[wincode(with = "PodVec2")]
    pub ball_vel: Vec2,    // not smoothed: it's the derivative, smoothing it looks wrong
    pub creator: Pubkey,
    #[smooth(map)]
    pub players: HashMap<Pubkey, PlayerState>,
}
```

The generated smoother is `PongGameStateSmoother`; wire it up with
`type Smoother = PongGameStateSmoother;`.

## `SmoothableField`

```rust
pub trait SmoothableField {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self;
    fn magnitude_sq(&self) -> f32;
}
```

Implemented in core for `f32`, `f64`, `glam::Vec2`, `glam::Vec3`. Implement it for your own
types (quaternions, fixed-point, a custom `Angle` that wraps at 2π) to use them with
`#[smooth]`. Your type also needs `Sub`, `AddAssign`, and `MulAssign<f32>` — the generated
code does offset arithmetic directly.

## What to smooth

- **Smooth**: positions, rotations, camera targets, health bars — continuous quantities a
  player perceives as motion.
- **Don't smooth**: velocities (smoothing the derivative fights the integrator), scores,
  ammo counts, booleans, enum states, anything a game rule reads. Remember the smoothed state
  is *presentation only* — but if your renderer feeds it back into any decision, discrete
  fields must be exact.
- **Teleports are handled for you, if `max_offset` is set sanely**: `apply` compares the
  single-tick jump against `max_offset` and snaps rather than sweeping the object across the
  gap, so a respawn or round reset does not streak across the screen. This only works if
  `max_offset` sits between "fastest normal per-tick motion" and "smallest real teleport"; if
  those overlap, use `NoopSmoother` on that sub-struct or a custom impl that checks a flag.

## Tuning

| Symptom | Reach for |
| --- | --- |
| a remote entity keeps gliding after it stops | `motion_ratio` (2.0 is a reasonable start) |
| corrections drag on for seconds | `decay` is too high for your tick rate — compare `tick_ms / ln(1/decay)`, not `decay`; then bound it with `max_correction` |
| objects "swim" or overshoot | raise `min_offset_sq` so small offsets are dropped |
| a teleport streaks across the screen | lower `max_offset` below the jump distance |
| a fast object pops instead of smoothing | raise `max_offset` above its per-tick travel |

**Smoothing cannot fix a state that is persistently wrong.** If corrections keep arriving,
the offset is being re-injected faster than it decays, and no smoothing parameter removes
that — it only chooses between a visible slide and a visible pop. The fix is upstream: a
better `DeformInputs::predict()` (see `netcode.md`), or a shorter prediction horizon.
