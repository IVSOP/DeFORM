/// Parameters controlling smoothing behavior.
#[derive(Clone, Copy, Debug)]
pub struct SmoothParams {
    pub decay: f32,
    pub max_offset_sq: f32,
    pub min_offset_sq: f32,
}

/// Trait defining how the [`DeformUserLogic::GameState`] should be smoothed.
/// Use `#[derive(Smooth)]` on your game state to generate an implementation, or [`NoopSmoother`] to disable smoothing.
pub trait Smooth<G>: Default + Send {
    /// Resets smoothing offsets to zero. Used on hard resets (e.g. fast-forward) when no prior visual state is meaningful.
    fn reset(&mut self);

    /// Recomputes offsets based on a rollback.
    /// `pre_state` is the state at local_tick *before* the rollback (what was being displayed).
    /// `post_state` is the state at the same local_tick *after* resimulation (the corrected truth).
    fn on_rollback(&mut self, pre_state: &G, post_state: &G);

    /// Decays offsets toward zero then applies them to the game state for rendering.
    fn apply(&mut self, game_state: &mut G);

    /// Override the smoothing parameters. Used by `#[smooth(map)]` to inherit the parent's config.
    fn set_params(&mut self, _params: SmoothParams) {}
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
    fn on_rollback(&mut self, _: &G, _: &G) {}
    fn apply(&mut self, _: &mut G) {}
}

/// Types usable with `#[smooth]` in a `#[derive(Smooth)]` struct.
/// Arithmetic is handled through standard operators (`-`, `+=`, `*= f32`).
/// This trait only adds squared magnitude, needed for offset thresholds.
pub trait SmoothableField {
    fn magnitude_sq(&self) -> f32;
}

impl SmoothableField for f32 {
    fn magnitude_sq(&self) -> f32 {
        self * self
    }
}

impl SmoothableField for f64 {
    fn magnitude_sq(&self) -> f32 {
        (*self * *self) as f32
    }
}

impl SmoothableField for glam::Vec2 {
    fn magnitude_sq(&self) -> f32 {
        self.length_squared()
    }
}

impl SmoothableField for glam::Vec3 {
    fn magnitude_sq(&self) -> f32 {
        self.length_squared()
    }
}
