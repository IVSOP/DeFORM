//! Parameter inheritance for `#[derive(Smooth)]`.
//!
//! The contract: a struct-level `#[smooth(...)]` applies to that type *and its whole
//! subtree*, and any descendant that authors its own `#[smooth(...)]` overrides for
//! itself and everything below it. Independently, `scale_decay` converts the authored
//! per-simulation-tick rates into per-visual-frame ones, and must reach every smoother
//! including map entries created long after it ran.
//!
//! Everything here is asserted through observable output (the value `apply` writes),
//! never through the generated struct's private fields.

use std::collections::HashMap;

use deform_core::{Smooth, Smoothable};

/// Drives one `on_rollback` that injects an offset of exactly `1.0`, then a single
/// `apply` with `prev == current == 0.0`. The value left behind is therefore the
/// offset after one frame of decay — i.e. the effective `decay` this smoother used.
fn effective_decay<S, G>(smoother: &mut S, mut pre: G, post: G) -> G
where
    S: Smooth<G>,
    G: Clone,
{
    smoother.on_rollback(&pre, &post);
    let mut current = post.clone();
    let prev = post;
    smoother.apply(&prev, &mut current, 1.0);
    pre = current;
    pre
}

// --- fixtures -------------------------------------------------------------------

/// No struct-level attribute: inherits from whatever contains it.
#[derive(Default, Debug, Clone, Smooth)]
struct Inheriting {
    #[smooth]
    pos: f32,
}

/// Authors its own params: overrides for itself and its subtree.
#[derive(Default, Debug, Clone, Smooth)]
#[smooth(decay = 0.25, max_offset = 100.0, min_offset_sq = 0.0000001)]
struct Overriding {
    #[smooth]
    pos: f32,
    #[smooth(nested)]
    leaf: Inheriting,
}

#[derive(Default, Debug, Clone, Smooth)]
#[smooth(decay = 0.5, max_offset = 100.0, min_offset_sq = 0.0000001)]
struct Root {
    #[smooth]
    pos: f32,
    #[smooth(nested)]
    inheriting: Inheriting,
    #[smooth(nested)]
    overriding: Overriding,
    #[smooth(map)]
    inheriting_entries: HashMap<u32, Inheriting>,
    #[smooth(map)]
    overriding_entries: HashMap<u32, Overriding>,
}

fn root_with(pos: f32, entry: f32) -> Root {
    let mut r = Root {
        pos,
        inheriting: Inheriting { pos },
        overriding: Overriding {
            pos,
            leaf: Inheriting { pos },
        },
        inheriting_entries: HashMap::new(),
        overriding_entries: HashMap::new(),
    };
    r.inheriting_entries.insert(0, Inheriting { pos: entry });
    r.overriding_entries.insert(
        0,
        Overriding {
            pos: entry,
            leaf: Inheriting { pos: entry },
        },
    );
    r
}

/// One rollback injecting a 1.0 offset everywhere, then one `apply`. Each field of the
/// result is the offset that survived a single frame, i.e. that field's effective decay.
fn decay_of_each_field(smoother: &mut RootSmoother) -> Root {
    let pre = root_with(1.0, 1.0);
    let post = root_with(0.0, 0.0);
    effective_decay(smoother, pre, post)
}

// --- tests ----------------------------------------------------------------------

#[test]
fn parent_params_reach_nested_children() {
    // The root is only ever built via `default()` + `scale_decay` by the backends;
    // `set_params` is never called on it. A nested child must still inherit, rather
    // than silently keeping the derive defaults (decay 0.9).
    let mut s = <Root as Smoothable>::Smoother::default();
    let out = decay_of_each_field(&mut s);

    assert_eq!(out.pos, 0.5, "root's own field uses its authored decay");
    assert_eq!(
        out.inheriting.pos, 0.5,
        "nested child with no params of its own must inherit the root's 0.5, not the derive default 0.9"
    );
}

#[test]
fn child_params_override_the_parent() {
    let mut s = <Root as Smoothable>::Smoother::default();
    let out = decay_of_each_field(&mut s);

    assert_eq!(
        out.overriding.pos, 0.25,
        "a nested child that authored #[smooth(...)] keeps its own decay"
    );
}

#[test]
fn an_override_trickles_down_to_its_own_subtree() {
    // Root(0.5) -> Overriding(0.25) -> Inheriting(unset).
    // The leaf must land on 0.25: an override applies to everything beneath it, and
    // must not be bypassed in favour of the grandparent's value.
    let mut s = <Root as Smoothable>::Smoother::default();
    let out = decay_of_each_field(&mut s);

    assert_eq!(
        out.overriding.leaf.pos, 0.25,
        "leaf under an overriding parent must take that parent's 0.25, not the root's 0.5"
    );
}

#[test]
fn map_entries_follow_the_same_rules() {
    let mut s = <Root as Smoothable>::Smoother::default();
    let out = decay_of_each_field(&mut s);

    assert_eq!(
        out.inheriting_entries[&0].pos, 0.5,
        "map entry with no params of its own inherits the root's"
    );
    assert_eq!(
        out.overriding_entries[&0].pos, 0.25,
        "map entry that authored its own params keeps them"
    );
    assert_eq!(
        out.overriding_entries[&0].leaf.pos, 0.25,
        "and trickles them down to its own children"
    );
}

#[test]
fn scale_decay_reaches_map_entries_created_afterwards() {
    // `scale_decay` runs once at construction, while the maps are still empty. Entries
    // are created lazily on first use, so the scale has to be replayed for them —
    // including entries whose type authored its own params, which `set_params` skips.
    let ratio = 0.5; // visual tick is half a sim tick
    let mut s = <Root as Smoothable>::Smoother::default();
    s.scale_decay(ratio);
    let out = decay_of_each_field(&mut s);

    let expect_inherited = 0.5f32.powf(ratio);
    let expect_overriding = 0.25f32.powf(ratio);

    assert!((out.pos - expect_inherited).abs() < 1e-6, "got {}", out.pos);
    assert!(
        (out.inheriting_entries[&0].pos - expect_inherited).abs() < 1e-6,
        "inheriting map entry not scaled: got {}",
        out.inheriting_entries[&0].pos
    );
    assert!(
        (out.overriding_entries[&0].pos - expect_overriding).abs() < 1e-6,
        "map entry with its own params was never scaled: got {} want {}",
        out.overriding_entries[&0].pos,
        expect_overriding
    );
}

#[test]
fn scale_decay_is_idempotent() {
    // Scaling is an assignment, not an accumulation, so applying it twice must not
    // compound. This is what makes replaying it onto lazily created children safe.
    let mut once = <Root as Smoothable>::Smoother::default();
    once.scale_decay(0.5);
    let a = decay_of_each_field(&mut once);

    let mut twice = <Root as Smoothable>::Smoother::default();
    twice.scale_decay(0.5);
    twice.scale_decay(0.5);
    let b = decay_of_each_field(&mut twice);

    assert_eq!(a.pos, b.pos);
    assert_eq!(a.overriding.pos, b.overriding.pos);
    assert_eq!(a.overriding_entries[&0].pos, b.overriding_entries[&0].pos);
}

#[test]
fn max_correction_is_inherited_and_scaled_like_decay() {
    #[derive(Default, Debug, Clone, Smooth)]
    struct Leaf {
        #[smooth]
        pos: f32,
    }

    // decay = 1.0 isolates `max_correction`: the whole reduction is the linear step.
    #[derive(Default, Debug, Clone, Smooth)]
    #[smooth(
        decay = 1.0,
        max_offset = 100.0,
        min_offset_sq = 0.0000001,
        max_correction = 0.25
    )]
    struct Bounded {
        #[smooth]
        pos: f32,
        #[smooth(nested)]
        leaf: Leaf,
    }

    let step = |ratio: f32| {
        let mut s = <Bounded as Smoothable>::Smoother::default();
        s.scale_decay(ratio);
        let pre = Bounded {
            pos: 1.0,
            leaf: Leaf { pos: 1.0 },
        };
        let post = Bounded::default();
        effective_decay(&mut s, pre, post)
    };

    let unscaled = step(1.0);
    assert!(
        (unscaled.pos - 0.75).abs() < 1e-6,
        "one 0.25 step off a 1.0 offset, got {}",
        unscaled.pos
    );
    assert!(
        (unscaled.leaf.pos - 0.75).abs() < 1e-6,
        "nested child must inherit max_correction, got {}",
        unscaled.leaf.pos
    );

    // Half-length visual frames spend half the correction budget each.
    let scaled = step(0.5);
    assert!(
        (scaled.pos - 0.875).abs() < 1e-6,
        "max_correction is per sim tick and must scale to the frame, got {}",
        scaled.pos
    );
    assert!(
        (scaled.leaf.pos - 0.875).abs() < 1e-6,
        "nested child must be scaled too, got {}",
        scaled.leaf.pos
    );
}

#[test]
fn a_jump_beyond_max_offset_snaps_instead_of_interpolating() {
    #[derive(Default, Debug, Clone, Smooth)]
    #[smooth(decay = 0.5, max_offset = 10.0, min_offset_sq = 0.0000001)]
    struct Warping {
        #[smooth]
        pos: f32,
    }

    let mut s = <Warping as Smoothable>::Smoother::default();

    // Inside the threshold: interpolated at the halfway point.
    let mut current = Warping { pos: 8.0 };
    s.apply(&Warping { pos: 0.0 }, &mut current, 0.5);
    assert_eq!(current.pos, 4.0);

    // Beyond it: a teleport, so the target is taken whole even mid-frame.
    let mut current = Warping { pos: 400.0 };
    s.apply(&Warping { pos: 0.0 }, &mut current, 0.5);
    assert_eq!(current.pos, 400.0, "a teleport must not be swept across");
}

#[test]
fn motion_ratio_snaps_a_field_at_rest_but_not_a_moving_one() {
    #[derive(Default, Debug, Clone, Smooth)]
    #[smooth(
        decay = 1.0,
        max_offset = 1000.0,
        min_offset_sq = 0.0000001,
        motion_ratio = 2.0
    )]
    struct Tracked {
        #[smooth]
        pos: f32,
    }

    // decay = 1.0, so anything that shrinks the offset here is `motion_ratio` alone.
    let inject = |travel: f32| {
        let mut s = <Tracked as Smoothable>::Smoother::default();
        s.on_rollback(&Tracked { pos: 10.0 }, &Tracked { pos: 0.0 }); // offset = 10
        let prev = Tracked { pos: 0.0 };
        let mut current = Tracked { pos: travel };
        s.apply(&prev, &mut current, 1.0);
        current.pos - travel // whatever offset survived
    };

    assert_eq!(
        inject(0.0),
        0.0,
        "a field that did not move must snap: any residual offset is motion that is not happening"
    );
    assert_eq!(
        inject(2.0),
        4.0,
        "moving 2.0/tick allows 2.0 * 2.0 = 4.0 of the 10.0 offset to hide inside it"
    );
    assert_eq!(
        inject(50.0),
        10.0,
        "moving far outruns the cap, so the offset is untouched and decay alone governs"
    );
}

#[test]
fn motion_ratio_is_inherited_and_not_rescaled() {
    #[derive(Default, Debug, Clone, Smooth)]
    struct Leaf {
        #[smooth]
        pos: f32,
    }

    #[derive(Default, Debug, Clone, Smooth)]
    #[smooth(
        decay = 1.0,
        max_offset = 1000.0,
        min_offset_sq = 0.0000001,
        motion_ratio = 2.0
    )]
    struct Parent {
        #[smooth(nested)]
        leaf: Leaf,
    }

    // Dimensionless: a ratio of travelled distance holds regardless of frame length.
    for ratio in [1.0, 0.5, 0.25] {
        let mut s = <Parent as Smoothable>::Smoother::default();
        s.scale_decay(ratio);
        s.on_rollback(
            &Parent {
                leaf: Leaf { pos: 10.0 },
            },
            &Parent::default(),
        );
        let mut current = Parent {
            leaf: Leaf { pos: 2.0 },
        };
        s.apply(
            &Parent {
                leaf: Leaf { pos: 0.0 },
            },
            &mut current,
            1.0,
        );
        assert_eq!(
            current.leaf.pos - 2.0,
            4.0,
            "nested child must inherit motion_ratio, and it must not scale with the frame (ratio {ratio})"
        );
    }
}
