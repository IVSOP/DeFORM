use std::{collections::HashMap, hash::BuildHasherDefault, marker::PhantomData, rc::Rc};

use deform_core::{Smooth, SmoothParams, Smoothable};
use glam::{Vec2, Vec3};

#[derive(Clone, Debug, Default, Smooth)]
struct Player {
    #[smooth]
    position: f32,
    score: u32,
}

#[derive(Default, Smooth)]
#[smooth(decay = 0.5, max_offset = 10.0, min_offset_sq = 0)]
struct World {
    #[smooth]
    player: Player,
    #[smooth]
    players: HashMap<u32, Player>,
    untouched: Player,
}

fn players(position: f32) -> HashMap<u32, Player> {
    HashMap::from([(1, Player { position, score: 7 })])
}

fn params() -> SmoothParams {
    SmoothParams {
        decay: 0.5,
        max_offset_sq: 100.0,
        min_offset_sq: 0.0,
        ..SmoothParams::default()
    }
}

#[test]
fn only_selected_fields_recurse_and_new_entities_snap() {
    let prev = World::default();
    let mut current = World {
        player: Player {
            position: 8.0,
            score: 7,
        },
        players: players(8.0),
        untouched: Player {
            position: 8.0,
            score: 7,
        },
    };
    WorldSmoother::default().apply(&prev, &mut current, 0.25);
    assert_eq!(current.player.position, 2.0);
    assert_eq!(current.player.score, 7);
    assert_eq!(current.players[&1].position, 8.0);
    assert_eq!(current.untouched.position, 8.0);
}

#[test]
fn leaves_support_f64_and_vector_offsets() {
    #[derive(Smooth)]
    #[smooth(decay = 0.5, min_offset_sq = 0)]
    struct Values {
        #[smooth]
        double: f64,
        #[smooth]
        xy: Vec2,
        #[smooth]
        xyz: Vec3,
    }
    let values = |v: f32| Values {
        double: f64::from(v),
        xy: Vec2::splat(v),
        xyz: Vec3::splat(v),
    };
    let mut smoother = ValuesSmoother::default();
    smoother.on_rollback(&values(2.0), &values(0.0));
    let mut current = values(8.0);
    smoother.apply(&values(0.0), &mut current, 0.5);
    assert_eq!(current.double, 5.0);
    assert_eq!(current.xy, Vec2::splat(5.0));
    assert_eq!(current.xyz, Vec3::splat(5.0));
    assert_eq!(smoother.correction_magnitude_sq(), 6.0);
}

#[derive(Default, Smooth)]
#[smooth(decay = 0.25)]
struct SlowChild {
    #[smooth]
    leaf: Player,
    #[smooth]
    players: HashMap<u32, Player>,
}

#[derive(Default, Smooth)]
#[smooth(decay = 0.5, max_offset = 10, min_offset_sq = 0)]
struct Parent {
    #[smooth]
    child: SlowChild,
}

fn parent(position: f32) -> Parent {
    Parent {
        child: SlowChild {
            leaf: Player { position, score: 0 },
            players: players(position),
        },
    }
}

#[test]
fn partial_overrides_inherit_the_other_parameters_through_the_subtree() {
    let mut smoother = ParentSmoother::default();
    smoother.on_rollback(&parent(1.0), &parent(0.0));
    let mut current = parent(0.0);
    smoother.apply(&parent(0.0), &mut current, 1.0);
    // If the child's unspecified min_offset_sq used the root default (4), these
    // small offsets would disappear. Its authored decay still wins over 0.5.
    assert_eq!(current.child.leaf.position, 0.25);
    assert_eq!(current.child.players[&1].position, 0.25);

    smoother.reset();
    smoother.on_rollback(&parent(11.0), &parent(0.0));
    assert_eq!(smoother.corrections_discarded(), 2);
    assert_eq!(smoother.correction_magnitude_sq(), 0.0);
}

#[test]
fn changing_params_and_scale_reaches_existing_and_future_map_entries() {
    type Smoother = <HashMap<u32, Player> as Smoothable>::Smoother;
    let run = |scale_first: bool| {
        let mut smoother = Smoother::default();
        // Create a child before configuring its parent.
        smoother.apply(&players(0.0), &mut players(0.0), 1.0);
        if scale_first {
            smoother.scale_decay(0.5);
        }
        smoother.set_params(SmoothParams {
            decay: 0.25,
            max_correction: 0.25,
            ..params()
        });
        if !scale_first {
            smoother.scale_decay(0.5);
        }
        let mut pre = players(2.0);
        pre.insert(2, pre[&1].clone());
        let mut post = players(0.0);
        post.insert(2, post[&1].clone());
        smoother.on_rollback(&pre, &post);
        let mut current = post.clone();
        smoother.apply(&post, &mut current, 1.0);
        for player in current.values() {
            assert_eq!(player.position, 0.875); // 2 * sqrt(0.25) - 0.25 * 0.5
        }
    };
    run(true);
    run(false);
}

#[test]
fn repeated_rollbacks_accumulate_and_reset_keeps_configuration() {
    let mut smoother = <f32 as Smoothable>::Smoother::default();
    smoother.set_params(params());
    smoother.scale_decay(2.0);
    smoother.on_rollback(&4.0, &0.0);
    let mut current = 0.0;
    smoother.apply(&0.0, &mut current, 1.0);
    assert_eq!(current, 1.0);
    smoother.on_rollback(&2.0, &0.0);
    assert_eq!(smoother.correction_magnitude_sq(), 9.0);
    smoother.reset();
    assert_eq!(smoother.correction_magnitude_sq(), 0.0);
    smoother.on_rollback(&4.0, &0.0);
    current = 0.0;
    smoother.apply(&0.0, &mut current, 1.0);
    assert_eq!(current, 1.0);
}

#[test]
fn a_teleport_clears_the_residual_offset() {
    let mut smoother = <f32 as Smoothable>::Smoother::default();
    smoother.set_params(params());
    smoother.on_rollback(&4.0, &0.0);
    let mut current = 20.0;
    smoother.apply(&0.0, &mut current, 0.5);
    assert_eq!(current, 20.0);
    assert_eq!(smoother.correction_magnitude_sq(), 0.0);
    current = 21.0;
    smoother.apply(&20.0, &mut current, 0.5);
    assert_eq!(current, 20.5);
}

#[test]
fn map_removal_releases_offsets_and_reused_keys_start_fresh() {
    let mut smoother = <HashMap<u32, Player> as Smoothable>::Smoother::default();
    smoother.set_params(params());
    smoother.on_rollback(&players(4.0), &players(0.0));
    assert_eq!(smoother.correction_magnitude_sq(), 16.0);
    smoother.apply(&players(0.0), &mut HashMap::new(), 1.0);
    assert_eq!(smoother.correction_magnitude_sq(), 0.0);
    let mut current = players(2.0);
    smoother.apply(&HashMap::new(), &mut current, 0.5);
    assert_eq!(current[&1].position, 2.0);
    let mut next = players(4.0);
    smoother.apply(&current, &mut next, 0.5);
    assert_eq!(next[&1].position, 3.0);
}

#[test]
fn new_map_entries_drop_old_offsets_even_without_an_empty_frame() {
    let mut smoother = <HashMap<u32, Player> as Smoothable>::Smoother::default();
    smoother.set_params(params());
    smoother.on_rollback(&players(4.0), &players(0.0));
    smoother.apply(&HashMap::new(), &mut players(2.0), 0.5);
    assert_eq!(smoother.correction_magnitude_sq(), 0.0);
    smoother.on_rollback(&players(4.0), &players(0.0));
    smoother.on_rollback(&HashMap::new(), &players(2.0));
    assert_eq!(smoother.correction_magnitude_sq(), 0.0);
}

#[test]
fn discarded_counts_survive_removal_rollback_and_reset() {
    let mut smoother = <HashMap<u32, Player> as Smoothable>::Smoother::default();
    smoother.set_params(params());
    smoother.on_rollback(&players(11.0), &players(0.0));
    assert_eq!(smoother.corrections_discarded(), 1);
    smoother.apply(&players(0.0), &mut HashMap::new(), 1.0);
    assert_eq!(smoother.corrections_discarded(), 1);
    smoother.on_rollback(&players(11.0), &players(0.0));
    smoother.on_rollback(&players(0.0), &HashMap::new());
    assert_eq!(smoother.corrections_discarded(), 2);
    smoother.on_rollback(&players(11.0), &players(0.0));
    smoother.reset();
    smoother.reset();
    assert_eq!(smoother.corrections_discarded(), 3);
}

#[test]
fn nested_maps_and_type_aliases_use_the_same_recursive_interface() {
    type CustomMap =
        HashMap<u32, f32, BuildHasherDefault<std::collections::hash_map::DefaultHasher>>;
    #[derive(Smooth)]
    #[smooth(decay = 0.5, min_offset_sq = 0)]
    struct Aliased {
        #[smooth]
        maps: HashMap<u32, CustomMap>,
    }
    let value = |v| Aliased {
        maps: HashMap::from([(1, CustomMap::from_iter([(2, v)]))]),
    };
    let mut smoother = AliasedSmoother::default();
    smoother.scale_decay(2.0);
    smoother.on_rollback(&value(4.0), &value(0.0));
    let mut current = value(0.0);
    smoother.apply(&value(0.0), &mut current, 1.0);
    assert_eq!(current.maps[&1][&2], 1.0);
    smoother.reset();
    assert_eq!(smoother.correction_magnitude_sq(), 0.0);
}

#[test]
fn generics_lifetimes_and_unselected_types_need_no_extra_bounds() {
    // Rc is not Send; the unselected payload must not constrain the smoother.
    // The field name also deliberately matches an internal generated field.
    #[derive(Smooth)]
    struct Generic<'a, T, U, const N: usize>
    where
        T: Smoothable,
    {
        #[smooth]
        __fields: T,
        payload: &'a U,
        marker: PhantomData<[u8; N]>,
    }
    let payload = Rc::new(0);
    let prev = Generic::<_, _, 3> {
        __fields: 0.0_f32,
        payload: &payload,
        marker: PhantomData,
    };
    let mut current = Generic::<_, _, 3> {
        __fields: 8.0_f32,
        payload: &payload,
        marker: PhantomData,
    };
    let mut smoother = GenericSmoother::<'_, f32, Rc<i32>, 3>::default().clone();
    smoother.apply(&prev, &mut current, 0.5);
    assert_eq!(current.__fields, 4.0);
    assert!(Rc::ptr_eq(current.payload, &payload));
}

#[test]
fn empty_selection_is_a_noop() {
    #[derive(Smooth)]
    struct Discrete {
        score: u32,
    }
    let mut smoother = DiscreteSmoother::default();
    smoother.scale_decay(0.5);
    smoother.on_rollback(&Discrete { score: 1 }, &Discrete { score: 2 });
    let mut current = Discrete { score: 2 };
    smoother.apply(&Discrete { score: 1 }, &mut current, 0.5);
    smoother.reset();
    assert_eq!(current.score, 2);
    assert_eq!(smoother.correction_magnitude_sq(), 0.0);
}

#[test]
fn zero_frame_ratio_preserves_offsets_and_does_not_enable_disabled_caps() {
    let mut smoother = <f64 as Smoothable>::Smoother::default();
    smoother.set_params(params());
    smoother.scale_decay(0.0);
    smoother.on_rollback(&4.0, &0.0);
    let mut current = 0.0;
    smoother.apply(&0.0, &mut current, 1.0);
    assert_eq!(current, 4.0);
}
