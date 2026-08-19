/// Parameters controlling smoothing behavior.
#[derive(Clone, Copy, Debug)]
pub struct SmoothParams {
    pub decay: f32,
    pub max_offset_sq: f32,
    pub min_offset_sq: f32,
    /// Hard ceiling on how fast an offset is pulled toward zero, applied on top of
    /// `decay`.
    ///
    /// `f32::INFINITY` disables it, leaving pure exponential decay.
    pub max_correction: f32,
    /// Caps the offset at this multiple of the distance the field actually moved
    /// during the current tick.
    ///
    /// Dimensionless, so unlike `max_correction` it is not rescaled per frame.
    /// `f32::INFINITY` disables it.
    pub motion_ratio: f32,
}

/// Trait for interpolating between game states and absorbing rollback corrections.
///
/// `apply` lerps between `prev` and `current` using `t`, then adds a decaying
/// offset that absorbs discontinuities from rollbacks.
///
/// Use `#[derive(Smooth)]` on your game state to generate an implementation,
/// or [`NoopSmoother`] to disable smoothing.
pub trait Smooth<G>: Default + Send + Clone {
    /// Resets smoothing offsets to zero. Used on hard resets (e.g., fast-forward).
    fn reset(&mut self);

    /// Captures the offset from a rollback correction so the visual position
    /// eases into the corrected state instead of teleporting.
    fn on_rollback(&mut self, pre: &G, post: &G);

    /// Interpolates between `prev` and `current` using `t` (0=prev, 1=current),
    /// then applies and decays any residual offset from rollback corrections.
    fn apply(&mut self, prev: &G, current: &mut G, t: f32);

    /// Adjusts the decay factor for a visual tick rate that differs from the simulation tick rate.
    /// `ratio` is `visual_tick_micros / sim_tick_micros`.
    fn scale_decay(&mut self, ratio: f32);

    /// Override the smoothing parameters. Used by `#[smooth(map)]` to inherit the parent's config.
    fn set_params(&mut self, params: SmoothParams);

    /// Squared magnitude of the offsets currently being absorbed. Read right after
    /// [`Smooth::on_rollback`] to see how far the world jumped.
    fn correction_magnitude_sq(&self) -> f32 {
        0.0
    }

    /// Cumulative count of offsets dropped for exceeding `max_offset` — corrections the
    /// smoother gave up on, so the player saw a snap. Diff it across a rollback.
    fn corrections_discarded(&self) -> u64 {
        0
    }
}

/// Trait linking a type to its derived smoother.
/// Automatically implemented by `#[derive(Smooth)]`.
pub trait Smoothable: Sized {
    type Smoother: Smooth<Self> + Clone;
}

/// A no-op smoother implementation
#[derive(Default, Clone)]
pub struct NoopSmoother;

impl<G> Smooth<G> for NoopSmoother {
    fn reset(&mut self) {}
    fn on_rollback(&mut self, _pre: &G, _post: &G) {}
    fn apply(&mut self, _prev: &G, _current: &mut G, _t: f32) {}
    fn scale_decay(&mut self, _ratio: f32) {}
    fn set_params(&mut self, _params: SmoothParams) {}
}

/// Types usable with `#[smooth]` in a `#[derive(Smooth)]` struct.
pub trait SmoothableField {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self;
    fn magnitude_sq(&self) -> f32;
}

impl SmoothableField for f32 {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self {
        self + (target - self) * t
    }
    fn magnitude_sq(&self) -> f32 {
        self * self
    }
}

impl SmoothableField for f64 {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self {
        self + (target - self) * t as f64
    }
    fn magnitude_sq(&self) -> f32 {
        (*self * *self) as f32
    }
}

impl SmoothableField for glam::Vec2 {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self {
        self.lerp(*target, t)
    }
    fn magnitude_sq(&self) -> f32 {
        self.length_squared()
    }
}

impl SmoothableField for glam::Vec3 {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self {
        self.lerp(*target, t)
    }
    fn magnitude_sq(&self) -> f32 {
        self.length_squared()
    }
}
