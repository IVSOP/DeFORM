use std::{
    collections::HashMap,
    hash::{BuildHasher, Hash, RandomState},
    marker::PhantomData,
};

use super::{validate_ratio, Smooth, SmoothParams, Smoothable};

/// Keeps an independent smoother for each map key present in both snapshots.
/// New values snap into existence, removed values release their smoothing state.
pub struct MapSmoother<K, V: Smoothable, S = RandomState> {
    hasher: PhantomData<fn() -> S>,
    entries: HashMap<K, V::Smoother>,
    params: SmoothParams,
    ratio: f32,
    retired_discarded: u64,
}

// Manual impls avoid requiring the simulation value itself to be Clone/Default.
impl<K: Clone, V: Smoothable, S> Clone for MapSmoother<K, V, S> {
    fn clone(&self) -> Self {
        Self {
            hasher: PhantomData,
            entries: self.entries.clone(),
            params: self.params,
            ratio: self.ratio,
            retired_discarded: self.retired_discarded,
        }
    }
}

impl<K, V: Smoothable, S> Default for MapSmoother<K, V, S> {
    fn default() -> Self {
        Self {
            hasher: PhantomData,
            entries: HashMap::new(),
            params: SmoothParams::default(),
            ratio: 1.0,
            retired_discarded: 0,
        }
    }
}

impl<K: Eq + Hash + Clone, V: Smoothable, S: BuildHasher> MapSmoother<K, V, S> {
    fn entry(&mut self, key: &K) -> &mut V::Smoother {
        let params = self.params;
        let ratio = self.ratio;
        self.entries.entry(key.clone()).or_insert_with(|| {
            let mut smoother = V::Smoother::default();
            smoother.set_params(params);
            smoother.scale_decay(ratio);
            smoother
        })
    }

    fn retain_shared(&mut self, prev: &HashMap<K, V, S>, current: &HashMap<K, V, S>) {
        self.entries.retain(|key, smoother| {
            let keep = prev.contains_key(key) && current.contains_key(key);
            if !keep {
                self.retired_discarded += smoother.corrections_discarded();
            }
            keep
        });
    }
}

impl<K, V, S> Smooth<HashMap<K, V, S>> for MapSmoother<K, V, S>
where
    K: Eq + Hash + Clone + Send,
    V: Smoothable,
    S: BuildHasher,
{
    fn reset(&mut self) {
        self.retired_discarded += self
            .entries
            .values()
            .map(Smooth::corrections_discarded)
            .sum::<u64>();
        self.entries.clear();
    }

    fn on_rollback(&mut self, pre: &HashMap<K, V, S>, post: &HashMap<K, V, S>) {
        self.retain_shared(pre, post);
        for (key, current) in post {
            if let Some(previous) = pre.get(key) {
                self.entry(key).on_rollback(previous, current);
            }
        }
    }

    fn apply(&mut self, prev: &HashMap<K, V, S>, current: &mut HashMap<K, V, S>, t: f32) {
        self.retain_shared(prev, current);
        for (key, value) in current {
            if let Some(previous) = prev.get(key) {
                self.entry(key).apply(previous, value, t);
            }
        }
    }

    fn scale_decay(&mut self, ratio: f32) {
        validate_ratio(ratio);
        self.ratio = ratio;
        for smoother in self.entries.values_mut() {
            smoother.scale_decay(ratio);
        }
    }

    fn set_params(&mut self, params: SmoothParams) {
        params.validate();
        self.params = params;
        for smoother in self.entries.values_mut() {
            smoother.set_params(params);
        }
    }

    fn correction_magnitude_sq(&self) -> f32 {
        self.entries
            .values()
            .map(Smooth::correction_magnitude_sq)
            .sum()
    }

    fn corrections_discarded(&self) -> u64 {
        self.retired_discarded
            + self
                .entries
                .values()
                .map(Smooth::corrections_discarded)
                .sum::<u64>()
    }
}

impl<K, V, S> Smoothable for HashMap<K, V, S>
where
    K: Eq + Hash + Clone + Send,
    V: Smoothable,
    S: BuildHasher,
{
    type Smoother = MapSmoother<K, V, S>;
}
