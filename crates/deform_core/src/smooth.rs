/// Trait for interpolating between two game states.
///
/// The smoother is stateless — it only knows how to lerp between a previous
/// and current state given a `t` in `[0, 1]`. The backend is responsible for
/// tracking the previous state and computing `t` from elapsed time.
///
/// Use `#[derive(Smooth)]` on your game state to generate an implementation,
/// or [`NoopSmoother`] to disable interpolation.
pub trait Smooth<G> {
    /// Interpolates `current` toward `prev` based on `t`.
    /// `t = 0.0` → shows `prev`, `t = 1.0` → shows `current` (no change).
    fn apply(prev: &G, current: &mut G, t: f32);
}

/// Trait linking a type to its derived smoother.
/// Automatically implemented by `#[derive(Smooth)]`.
pub trait Smoothable: Sized {
    type Smoother: Smooth<Self>;
}

/// A no-op smoother implementation
#[derive(Default, Clone)]
pub struct NoopSmoother;

impl<G> Smooth<G> for NoopSmoother {
    fn apply(_prev: &G, _current: &mut G, _t: f32) {}
}

/// Types usable with `#[smooth]` in a `#[derive(Smooth)]` struct.
pub trait SmoothableField {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self;
}

impl SmoothableField for f32 {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self {
        self + (target - self) * t
    }
}

impl SmoothableField for f64 {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self {
        self + (target - self) * t as f64
    }
}

impl SmoothableField for glam::Vec2 {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self {
        self.lerp(*target, t)
    }
}

impl SmoothableField for glam::Vec3 {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self {
        self.lerp(*target, t)
    }
}
