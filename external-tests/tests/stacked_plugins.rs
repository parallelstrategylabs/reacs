use plugin_other::plugin_leaf::{CombatLog, Damage, Health, combat_phase};
use plugin_other::{Regen, regenerate};
use plugin_top::despawn_dead;
use plugin_top::plugin_mid::{Died, apply_damage};
use reacs::prelude::*;
use reacs::schedule::SystemId;

#[resource]
pub struct Obituary {
    pub dead: Vec<Entity>,
    pub alive: Vec<bool>,
}

#[system(run_after(despawn_dead))]
fn record_deaths(
    mut died: Mut<Signal<Died>>,
    em: EntityManager,
    mut obituary: Mut<Resource<Obituary>>,
) {
    for signal in died.receive() {
        obituary.dead.push(signal.0);
        obituary.alive.push(em.is_alive(signal.0));
    }
}

fn battle() -> (World, Entity, Entity) {
    let mut world = World::new();
    let (knight, goblin) = {
        let mut em = world.entity_manager();
        (em.spawn(), em.spawn())
    };
    world.set_component(knight, Health(10));
    world.set_component(goblin, Health(5));
    world.signal::<Regen>().send(
        Regen {
            target: knight,
            amount: 1,
        },
        ..,
    );
    world.signal::<Regen>().send(
        Regen {
            target: goblin,
            amount: 1,
        },
        ..,
    );
    world.signal::<Damage>().emit(Damage {
        target: goblin,
        amount: 6,
    });
    world.signal::<Damage>().send(
        Damage {
            target: knight,
            amount: 3,
        },
        1..3,
    );
    (world, knight, goblin)
}

#[test]
fn systems_from_stacked_crates_run_in_declared_order() {
    let _world = World::new();
    let uuids: Vec<_> = system_graph().iter().map(|s| s.uuid).collect();
    let position = |uuid| uuids.iter().position(|u| *u == uuid).unwrap();

    let order = [
        position(<regenerate as SystemId>::UUID),
        position(<combat_phase as SystemId>::UUID),
        position(<apply_damage as SystemId>::UUID),
        position(<despawn_dead as SystemId>::UUID),
        position(<record_deaths as SystemId>::UUID),
    ];
    assert!(order.is_sorted(), "{order:?}");
}

#[test]
fn order_tree_shows_systems_from_every_crate() {
    let world = World::new();
    world.print_system_order();
    let tree = world.system_order();

    for name in [
        "plugin_leaf::leaf_system",
        "plugin_mid::apply_damage",
        "plugin_top::despawn_dead",
        "plugin_other::regenerate",
        "stacked_plugins::record_deaths",
    ] {
        assert!(tree.contains(name), "{name} is missing from:\n{tree}");
    }
}

#[test]
fn plugins_share_signals_components_resources_and_params() {
    let (mut world, knight, goblin) = battle();

    for _ in 0..5 {
        world.step();
    }

    assert_eq!(
        world.get_resource::<CombatLog>().unwrap().lines,
        [
            "0: 0 healed 1, 11 left",
            "0: 1 healed 1, 6 left",
            "0: 1 took 6, 0 left",
            "0: 1 despawned",
            "1: 0 healed 1, 12 left",
            "1: 0 took 3, 9 left",
            "2: 0 healed 1, 10 left",
            "2: 0 took 3, 7 left",
            "3: 0 healed 1, 8 left",
            "4: 0 healed 1, 9 left",
        ]
    );
    assert_eq!(world.get_component::<Health>(knight).map(|h| h.0), Some(9));
    assert!(world.entity_manager().is_alive(knight));
    assert!(!world.entity_manager().is_alive(goblin));

    let obituary = world.get_resource::<Obituary>().unwrap();
    assert_eq!(obituary.dead, [goblin]);
    assert_eq!(obituary.alive, [false]);
}

#[test]
fn restored_stacked_run_matches_original() {
    let (mut original, _, _) = battle();
    original.step();
    original.step();

    let save = original.save();
    let mut restored = World::new();
    restored.load(&save);

    for _ in 0..5 {
        original.step();
        restored.step();
        assert_eq!(restored.save(), original.save());
    }
    assert_eq!(
        restored.get_resource::<CombatLog>().unwrap().lines,
        original.get_resource::<CombatLog>().unwrap().lines
    );
}
