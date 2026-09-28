//! Recursive, opt-in interpolation and rollback smoothing.
//!
//! Mark each field you want to smooth with `#[smooth]`. Scalars interpolate;
//! derived structs recurse into their selected fields; maps track values by key.
//! Unmarked fields keep the current simulation value.
//!
//! ```
//! use std::collections::HashMap;
//! use deform_core::{Smooth, Smoothable};
//!
//! #[derive(Smooth)]
//! struct Player {
//!     #[smooth]
//!     position: glam::Vec2,
//!     score: u32,
//! }
//!
//! #[derive(Smooth)]
//! #[smooth(decay = 0.8, max_offset = 10.0, min_offset_sq = 0.0004)]
//! struct World {
//!     #[smooth]
//!     players: HashMap<u32, Player>,
//! }
//!
//! let mut smoother = <World as Smoothable>::Smoother::default();
//! smoother.scale_decay(0.5); // two visual frames per simulation tick
//! ```
//!
//! Parameters flow from parent to child. A type's `#[smooth(...)]` overrides only
//! the parameters it names, and passes the resulting settings to its children.
//! The root starts with [`SmoothParams::default`]. `#[smooth(nested)]` and
//! `#[smooth(map)]` remain accepted aliases for `#[smooth]`.

mod field;
mod map;

pub use field::FieldSmoother;
pub use map::MapSmoother;

/// Smoothing settings in simulation-tick units. Distances use the field's units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SmoothParams {
    /// Fraction of rollback offset retained each tick, between zero and one.
    pub decay: f32,
    /// Squared discontinuity threshold. Larger corrections are discarded, and
    /// larger single-tick movements snap instead of interpolating.
    pub max_offset_sq: f32,
    /// Residual offsets below this squared magnitude are cleared.
    pub min_offset_sq: f32,
    /// Additional distance removed from the offset each tick, after exponential
    /// decay. Despite the historical name, this is a linear step, not a speed cap.
    /// Infinity disables this step; zero has the same effect.
    pub max_correction: f32,
    /// Caps the offset to this multiple of the distance moved in one simulation
    /// tick. Infinity disables the cap. This parameter is dimensionless.
    pub motion_ratio: f32,
}

impl Default for SmoothParams {
    fn default() -> Self {
        Self {
            decay: 0.9,
            max_offset_sq: 200.0 * 200.0,
            min_offset_sq: 4.0,
            max_correction: f32::INFINITY,
            motion_ratio: f32::INFINITY,
        }
    }
}

impl SmoothParams {
    /// Panics for NaN, negative distances, or decay outside `[0, 1]`.
    pub fn validate(self) {
        assert!((0.0..=1.0).contains(&self.decay), "decay must be in [0, 1]");
        assert!(
            self.max_offset_sq >= 0.0,
            "max_offset_sq must be nonnegative"
        );
        assert!(
            self.min_offset_sq >= 0.0,
            "min_offset_sq must be nonnegative"
        );
        assert!(
            self.max_correction >= 0.0,
            "max_correction must be nonnegative"
        );
        assert!(self.motion_ratio >= 0.0, "motion_ratio must be nonnegative");
    }

    fn scaled(self, ratio: f32) -> Self {
        Self {
            decay: self.decay.powf(ratio),
            // Avoid infinity * zero: disabled settings stay disabled.
            max_correction: if self.max_correction.is_finite() {
                self.max_correction * ratio
            } else {
                self.max_correction
            },
            ..self
        }
    }
}

fn validate_ratio(ratio: f32) {
    assert!(
        ratio.is_finite() && ratio >= 0.0,
        "frame ratio must be finite and nonnegative"
    );
}

/// Interpolates simulation snapshots and absorbs rollback corrections.
///
/// `apply` writes into a fresh clone of the current simulation state. Never feed
/// its visual output back into the simulation or use it as the next snapshot.
pub trait Smooth<G>: Default + Send + Clone {
    /// Clears offsets, retaining parameters, frame ratio, and cumulative metrics.
    fn reset(&mut self);
    /// Adds the difference between the pre- and post-rollback simulation states
    /// to the remaining visual offset.
    fn on_rollback(&mut self, pre: &G, post: &G);
    /// Interpolates (`t = 0`: previous, `t = 1`: current), then decays and applies
    /// the rollback offset. Unselected fields keep their current values.
    fn apply(&mut self, prev: &G, current: &mut G, t: f32);
    /// Sets `visual_tick_micros / sim_tick_micros`. Repeated calls do not compound.
    /// The ratio must be finite and nonnegative. Also reaches future map entries.
    fn scale_decay(&mut self, ratio: f32);
    /// Supplies inherited, unscaled parameters. Derived types override only the
    /// parameters explicitly authored on that type, then configure their children.
    fn set_params(&mut self, params: SmoothParams);
    /// Sum of squared residual offsets across selected fields.
    fn correction_magnitude_sq(&self) -> f32 {
        0.0
    }
    /// Cumulative rollback offsets discarded for exceeding the threshold.
    /// Monotonic even when map entries disappear or offsets are reset.
    fn corrections_discarded(&self) -> u64 {
        0
    }
}

/// Connects a value to its smoother. Implemented for supported scalar/vector
/// leaves, `HashMap<K, V>` with smoothable values, and `#[derive(Smooth)]` structs.
pub trait Smoothable: Sized {
    type Smoother: Smooth<Self>;
}

/// Disables interpolation and rollback smoothing.
#[derive(Default, Clone)]
pub struct NoopSmoother;

impl<G> Smooth<G> for NoopSmoother {
    fn reset(&mut self) {}
    fn on_rollback(&mut self, _pre: &G, _post: &G) {}
    fn apply(&mut self, _prev: &G, _current: &mut G, _t: f32) {}
    fn scale_decay(&mut self, _ratio: f32) {}
    fn set_params(&mut self, _params: SmoothParams) {}
}

/// Arithmetic for an interpolated leaf. To use a custom leaf with `#[smooth]`,
/// implement [`Smoothable`] with [`FieldSmoother<Self>`] as its associated type.
/// It also needs `Clone + Default + Send + Add + Sub`, with `Default` being zero.
pub trait SmoothableField {
    fn lerp_toward(&self, target: &Self, t: f32) -> Self;
    fn magnitude_sq(&self) -> f32;
}

macro_rules! scalar {
    ($ty:ty) => {
        impl SmoothableField for $ty {
            fn lerp_toward(&self, target: &Self, t: f32) -> Self {
                self + (target - self) * t as $ty
            }
            fn magnitude_sq(&self) -> f32 {
                (self * self) as f32
            }
        }
        impl Smoothable for $ty {
            type Smoother = FieldSmoother<Self>;
        }
    };
}

scalar!(f32);
scalar!(f64);

macro_rules! vector {
    ($ty:ty) => {
        impl SmoothableField for $ty {
            fn lerp_toward(&self, target: &Self, t: f32) -> Self {
                self.lerp(*target, t)
            }
            fn magnitude_sq(&self) -> f32 {
                self.length_squared()
            }
        }
        impl Smoothable for $ty {
            type Smoother = FieldSmoother<Self>;
        }
    };
}

vector!(glam::Vec2);
vector!(glam::Vec3);
