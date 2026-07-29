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

pub struct SmoothParams { pub decay: f32, pub max_offset_sq: f32, pub min_offset_sq: f32 }
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
#[smooth(decay = 0.9, max_offset = 200.0, min_offset_sq = 4.0)]
struct GameState { /* … */ }
```

| Parameter | Default | Meaning |
| --- | --- | --- |
| `decay` | `0.9` | offset multiplier per visual frame — lower snaps harder |
| `max_offset` | `200.0` | corrections larger than this are discarded (teleport instead of a long visible slide) |
| `min_offset_sq` | `4.0` | squared magnitude below which the offset is zeroed, so it doesn't crawl forever |

Omitting the attribute uses the defaults *and* marks the smoother as "no custom params", so a
parent smoother's `set_params` will override it. Specifying `#[smooth(...)]` pins your values
and makes them win over any parent. That is the mechanism behind `#[smooth(map)]` inheritance.

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
#[smooth(decay = 0.9, max_offset = 200.0, min_offset_sq = 4.0)]
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
- **Don't smooth what teleports on purpose**: a respawn or a round reset is a legitimate jump.
  `max_offset` is the blunt guard (a correction bigger than that is discarded rather than
  slid through), but if you have explicit teleports, consider `NoopSmoother` on that
  sub-struct or a custom impl that checks a flag.

## Tuning

- Corrections visibly lag → lower `decay` (e.g. 0.8) or lower `max_offset`.
- Objects visibly "swim" or overshoot → raise `min_offset_sq` so small offsets get dropped.
- Long-distance corrections slide across the screen → lower `max_offset` so they snap.
- `decay` is per **visual** frame; the backend calls `scale_decay(visual_tick / sim_tick)` at
  construction so the same value behaves identically at 60 and 144 Hz. Don't compensate by hand.
