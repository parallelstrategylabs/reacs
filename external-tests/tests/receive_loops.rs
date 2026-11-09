use reacs::prelude::*;

#[signal]
pub struct Ping(pub u64);

#[signal(entity)]
pub struct Burn(pub Entity);

#[signal(input)]
pub struct Kill(pub Entity);

#[resource]
pub struct ForSeen {
    pub pings: Vec<(u64, u64)>,
    pub burns: Vec<(u64, Entity)>,
}

#[resource]
pub struct ForEachSeen {
    pub pings: Vec<(u64, u64)>,
    pub burns: Vec<(u64, Entity)>,
}

#[system(run_before(for_receiver, for_each_receiver))]
fn killer(mut kills: Mut<Signal<Kill>>, mut em: Mut<EntityManager>) {
    for kill in kills.receive() {
        em.despawn(kill.0);
    }
}

#[system]
fn for_receiver(
    mut pings: Mut<Signal<Ping>>,
    mut burns: Mut<Signal<Burn>>,
    tick: Resource<Tick>,
    mut seen: Mut<Resource<ForSeen>>,
) {
    let tick = (*tick).0;
    for ping in pings.receive() {
        seen.pings.push((tick, ping.0));
    }
    for burn in burns.receive() {
        seen.burns.push((tick, burn.0));
    }
}

#[system]
fn for_each_receiver(
    mut pings: Mut<Signal<Ping>>,
    mut burns: Mut<Signal<Burn>>,
    tick: Resource<Tick>,
    mut seen: Mut<Resource<ForEachSeen>>,
) {
    let tick = (*tick).0;
    pings
        .receive()
        .for_each(|ping| seen.pings.push((tick, ping.0)));
    burns
        .receive()
        .for_each(|burn| seen.burns.push((tick, burn.0)));
}

#[system(run_after(for_receiver, for_each_receiver))]
fn sender(mut pings: Mut<Signal<Ping>>, tick: Resource<Tick>) {
    let tick = (*tick).0;
    pings.emit(Ping(tick * 10));
    if tick == 1 {
        pings.send(Ping(100), 1..4);
    }
}

#[test]
fn for_and_for_each_receive_the_same_signals() {
    let mut units = (Entity::default(), Entity::default());
    let mut world = World::build(|setup| {
        let (first, second) = (setup.spawn(), setup.spawn());
        setup.signal::<Ping>().emit(Ping(1));
        setup.signal::<Ping>().send(Ping(2), ..);
        setup.signal::<Burn>().send(Burn(first), ..);
        setup.signal::<Burn>().send(Burn(second), ..);
        units = (first, second);
    });
    let (first, second) = units;

    world.step();
    world.step();
    world.input(Kill(first));
    for _ in 0..3 {
        world.step();
    }

    let expected_pings = [
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 2),
        (2, 10),
        (2, 100),
        (2, 2),
        (3, 20),
        (3, 100),
        (3, 2),
        (4, 30),
        (4, 2),
    ];
    let expected_burns = [
        (0, first),
        (0, second),
        (1, first),
        (1, second),
        (2, second),
        (3, second),
        (4, second),
    ];

    let by_for = world.get_resource::<ForSeen>().unwrap();
    let by_for_each = world.get_resource::<ForEachSeen>().unwrap();
    assert_eq!(by_for.pings, expected_pings);
    assert_eq!(by_for.burns, expected_burns);
    assert_eq!(by_for_each.pings, by_for.pings);
    assert_eq!(by_for_each.burns, by_for.burns);
}
