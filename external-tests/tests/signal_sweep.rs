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

#[signal(input)]
pub struct Kill(pub Entity);

#[resource]
pub struct AttackSeen {
    pub targets: Vec<Entity>,
}

#[system(run_before(burn, attack_reader))]
fn killer(mut kills: Mut<Signal<Kill>>, mut em: Mut<EntityManager>) {
    for kill in kills.receive() {
        em.despawn(kill.0);
    }
}

#[system]
fn attack_reader(mut attacks: Mut<Signal<Attack>>, mut seen: Mut<Resource<AttackSeen>>) {
    seen.targets.clear();
    for attack in attacks.receive() {
        seen.targets.push(attack.target);
    }
}

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
    let mut units = (Entity::default(), Entity::default());
    let mut world = World::build(|setup| {
        let (first, second) = (setup.spawn(), setup.spawn());
        setup.signal::<Burn>().send(Burn(first), ..);
        setup.signal::<Burn>().send(Burn(second), ..);
        units = (first, second);
    });
    let (first, second) = units;

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
            .permanent_len(),
        2
    );

    world.input(Kill(first));
    world.step();

    assert_eq!(world.get_resource::<Burned>().unwrap().entities, [second]);
    assert_eq!(
        world
            .get_resource::<Signals<Burn>>()
            .unwrap()
            .permanent_len(),
        1
    );
}

fn permanent_len<S: SignalType>(world: &World) -> usize {
    world.get_resource::<Signals<S>>().unwrap().permanent_len()
}

#[test]
fn entity_signals_are_removed_when_their_entity_dies() {
    let mut units = (Entity::default(), Entity::default());
    let mut world = World::build(|setup| {
        setup.init_signal::<Poison>();
        setup.init_signal::<Regen>();
        setup.init_signal::<Attack>();
        setup.init_signal::<Heal>();
        let (dead, alive) = (setup.spawn(), setup.spawn());

        setup.signal::<Poison>().send(Poison(dead), ..);
        setup.signal::<Poison>().send(Poison(alive), ..);
        setup.signal::<Regen>().send(
            Regen {
                amount: 1,
                target: dead,
            },
            ..,
        );
        setup.signal::<Regen>().send(
            Regen {
                amount: 1,
                target: alive,
            },
            ..,
        );
        setup.signal::<Attack>().send(
            Attack {
                source: alive,
                target: dead,
            },
            ..,
        );
        setup.signal::<Attack>().send(
            Attack {
                source: dead,
                target: alive,
            },
            ..,
        );
        setup.signal::<Heal>().send(Heal(alive, dead), ..);
        setup.signal::<Heal>().send(Heal(dead, alive), ..);
        units = (dead, alive);
    });
    let (dead, alive) = units;

    for _ in 0..3 {
        world.step();
    }
    assert_eq!(permanent_len::<Poison>(&world), 2);
    assert_eq!(permanent_len::<Regen>(&world), 2);
    assert_eq!(permanent_len::<Attack>(&world), 2);
    assert_eq!(permanent_len::<Heal>(&world), 2);

    world.input(Kill(dead));
    world.step();

    assert_eq!(permanent_len::<Poison>(&world), 1);
    assert_eq!(permanent_len::<Regen>(&world), 1);
    assert_eq!(permanent_len::<Attack>(&world), 1);
    assert_eq!(permanent_len::<Heal>(&world), 1);
    assert_eq!(world.get_resource::<AttackSeen>().unwrap().targets, [alive]);
}

#[test]
fn system_with_entity_manager_and_entity_signal_runs_without_a_borrow_panic() {
    let mut units = (Entity::default(), Entity::default());
    let mut world = World::build(|setup| {
        let (first, second, third) = (setup.spawn(), setup.spawn(), setup.spawn());
        setup.signal::<Chain>().send(Chain(first), ..);
        setup.signal::<Chain>().send(Chain(second), ..);
        setup.signal::<Chain>().send(Chain(third), ..);
        setup.get_resource_mut::<ChainTargets>().unwrap().next = vec![second];
        units = (first, third);
    });
    let (first, third) = units;

    world.step();

    assert_eq!(
        world.get_resource::<ChainTargets>().unwrap().delivered,
        [first, third]
    );
}
