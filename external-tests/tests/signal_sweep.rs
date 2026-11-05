use reacs::prelude::*;
use reacs::signal::Signals;

#[signal(entity)]
pub struct Burn(pub Entity);

#[signal(entity)]
pub struct Poison(pub Entity);

#[signal(entity)]
pub struct Regen {
    pub amount: u64,
    pub target: Entity,
}

#[signal(entity = target)]
pub struct Attack {
    pub source: Entity,
    pub target: Entity,
}

#[signal(entity = 1)]
pub struct Heal(pub Entity, pub Entity);

#[resource]
pub struct Burned {
    pub entities: Vec<Entity>,
}

#[system]
fn burn(mut burns: Mut<Signal<Burn>>, mut burned: Mut<Resource<Burned>>) {
    burned.entities.clear();
    for signal in burns.receive() {
        burned.entities.push(signal.0);
    }
}

#[signal(entity)]
pub struct Chain(pub Entity);

#[resource]
pub struct ChainTargets {
    pub next: Vec<Entity>,
    pub delivered: Vec<Entity>,
}

#[system]
fn chain(
    mut chains: Mut<Signal<Chain>>,
    mut em: Mut<EntityManager>,
    mut targets: Mut<Resource<ChainTargets>>,
) {
    for signal in chains.receive() {
        targets.delivered.push(signal.0);
        if let Some(next) = targets.next.pop() {
            em.despawn(next);
        }
    }
}

#[test]
fn dead_entitys_open_signal_is_removed_during_step() {
    let mut world = World::new();
    let (first, second) = {
        let mut em = world.entity_manager();
        (em.spawn(), em.spawn())
    };
    world.signal::<Burn>().send(Burn(first), ..);
    world.signal::<Burn>().send(Burn(second), ..);

    for _ in 0..3 {
        world.step();
    }
    assert_eq!(
        world.get_resource::<Burned>().unwrap().entities,
        [first, second]
    );
    assert_eq!(
        world
            .get_resource::<Signals<Burn>>()
            .unwrap()
            .permanent
            .len(),
        2
    );

    world.entity_manager().despawn(first);
    world.step();

    assert_eq!(world.get_resource::<Burned>().unwrap().entities, [second]);
    assert_eq!(
        world
            .get_resource::<Signals<Burn>>()
            .unwrap()
            .permanent
            .len(),
        1
    );
}

fn permanent_len<S: SignalType>(world: &World) -> usize {
    world.get_resource::<Signals<S>>().unwrap().permanent.len()
}

#[test]
fn entity_signals_are_removed_when_their_entity_dies() {
    let mut world = World::new();
    world.init_signal::<Poison>();
    world.init_signal::<Regen>();
    world.init_signal::<Attack>();
    world.init_signal::<Heal>();
    let (dead, alive) = {
        let mut em = world.entity_manager();
        (em.spawn(), em.spawn())
    };

    world.signal::<Poison>().send(Poison(dead), ..);
    world.signal::<Poison>().send(Poison(alive), ..);
    world.signal::<Regen>().send(
        Regen {
            amount: 1,
            target: dead,
        },
        ..,
    );
    world.signal::<Regen>().send(
        Regen {
            amount: 1,
            target: alive,
        },
        ..,
    );
    world.signal::<Attack>().send(
        Attack {
            source: alive,
            target: dead,
        },
        ..,
    );
    world.signal::<Attack>().send(
        Attack {
            source: dead,
            target: alive,
        },
        ..,
    );
    world.signal::<Heal>().send(Heal(alive, dead), ..);
    world.signal::<Heal>().send(Heal(dead, alive), ..);

    for _ in 0..3 {
        world.step();
    }
    assert_eq!(permanent_len::<Poison>(&world), 2);
    assert_eq!(permanent_len::<Regen>(&world), 2);
    assert_eq!(permanent_len::<Attack>(&world), 2);
    assert_eq!(permanent_len::<Heal>(&world), 2);

    world.entity_manager().despawn(dead);
    world.step();

    assert_eq!(permanent_len::<Poison>(&world), 1);
    assert_eq!(permanent_len::<Regen>(&world), 1);
    assert_eq!(permanent_len::<Attack>(&world), 1);
    assert_eq!(permanent_len::<Heal>(&world), 1);
    let received: Vec<Entity> = world
        .signal::<Attack>()
        .receive()
        .map(|s| s.target)
        .collect();
    assert_eq!(received, [alive]);
}

#[test]
fn system_with_entity_manager_and_entity_signal_runs_without_a_borrow_panic() {
    let mut world = World::new();
    let (first, second, third) = {
        let mut em = world.entity_manager();
        (em.spawn(), em.spawn(), em.spawn())
    };
    world.signal::<Chain>().send(Chain(first), ..);
    world.signal::<Chain>().send(Chain(second), ..);
    world.signal::<Chain>().send(Chain(third), ..);
    world.get_resource_mut::<ChainTargets>().unwrap().next = vec![second];

    world.step();

    assert_eq!(
        world.get_resource::<ChainTargets>().unwrap().delivered,
        [first, third]
    );
}
