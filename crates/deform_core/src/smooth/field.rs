use std::ops::{Add, Sub};

use super::{validate_ratio, Smooth, SmoothParams, SmoothableField};

/// Interpolation and rollback state for one arithmetic value.
#[derive(Clone)]
pub struct FieldSmoother<T> {
    offset: T,
    params: SmoothParams,
    frame_params: SmoothParams,
    ratio: f32,
    discarded: u64,
}

impl<T: Default> Default for FieldSmoother<T> {
    fn default() -> Self {
        Self {
            offset: T::default(),
            params: SmoothParams::default(),
            frame_params: SmoothParams::default(),
            ratio: 1.0,
            discarded: 0,
        }
    }
}

impl<T> FieldSmoother<T>
where
    T: SmoothableField + Default,
{
    fn scale_offset(&mut self, factor: f32) {
        self.offset = T::default().lerp_toward(&self.offset, factor);
    }

    fn cap_offset(&mut self, limit: f32) {
        let magnitude_sq = self.offset.magnitude_sq();
        if magnitude_sq > limit * limit {
            self.scale_offset(limit / magnitude_sq.sqrt());
        }
    }
}

impl<T> Smooth<T> for FieldSmoother<T>
where
    T: SmoothableField + Default + Clone + Send + Add<Output = T> + Sub<Output = T>,
{
    fn reset(&mut self) {
        self.offset = T::default();
    }

    fn on_rollback(&mut self, pre: &T, post: &T) {
        self.offset = self.offset.clone() + (pre.clone() - post.clone());
        if self.offset.magnitude_sq() > self.params.max_offset_sq {
            self.reset();
            self.discarded += 1;
        }
    }

    fn apply(&mut self, prev: &T, current: &mut T, t: f32) {
        let movement_sq = (current.clone() - prev.clone()).magnitude_sq();
        if movement_sq > self.params.max_offset_sq {
            self.reset();
            return;
        }

        let target = prev.lerp_toward(current, t);
        let params = self.frame_params;
        self.scale_offset(params.decay);
        if params.max_correction.is_finite() {
            let magnitude = self.offset.magnitude_sq().sqrt();
            if magnitude > 0.0 {
                self.scale_offset((magnitude - params.max_correction).max(0.0) / magnitude);
            }
        }
        if params.motion_ratio.is_finite() {
            self.cap_offset(movement_sq.sqrt() * params.motion_ratio);
        }
        if self.offset.magnitude_sq() < params.min_offset_sq {
            self.reset();
        }
        *current = target + self.offset.clone();
    }

    fn scale_decay(&mut self, ratio: f32) {
        validate_ratio(ratio);
        self.ratio = ratio;
        self.frame_params = self.params.scaled(ratio);
    }

    fn set_params(&mut self, params: SmoothParams) {
        params.validate();
        self.params = params;
        self.frame_params = params.scaled(self.ratio);
    }

    fn correction_magnitude_sq(&self) -> f32 {
        self.offset.magnitude_sq()
    }

    fn corrections_discarded(&self) -> u64 {
        self.discarded
    }
}
