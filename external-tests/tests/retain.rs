use reacs::prelude::*;
use reacs::signal::Signals;

#[component]
pub struct Fuel(pub u32);

#[signal(entity, input)]
pub struct Ignite(pub Entity);

#[signal(entity)]
pub struct Burn(pub Entity);

#[component]
pub struct Ash(pub u32);

#[signal(entity, input)]
pub struct Light(pub Entity);

#[signal(entity)]
pub struct Smoke(pub Entity);

#[resource]
pub struct SmokeLog {
    pub ran: Vec<u64>,
    pub received: Vec<u64>,
}

#[resource]
pub struct Log {
    pub burned: Vec<(u64, Entity)>,
    pub seen_after: Vec<(u64, Entity)>,
    pub cancelled: u64,
}

#[system]
fn ignite(mut ignites: Mut<Receiver<Ignite>>, mut burns: Mut<Sender<Burn>>) {
    for &Ignite(unit) in ignites.receive() {
        burns.send(Burn(unit), ..);
    }
}

#[system(run_after(ignite))]
fn burn(mut burns: Mut<Signal<Burn>>, mut fuel: Mut<Component<Fuel>>, mut log: Mut<Resource<Log>>) {
    let tick = burns.tick();
    let mut burned = Vec::new();
    let cancelled = burns.retain(|&Burn(unit)| {
        burned.push((tick, unit));
        let Some(left) = fuel.get_mut(unit) else {
            return false;
        };
        left.0 -= 1;
        left.0 > 0
    });
    log.burned.extend(burned);
    log.cancelled += cancelled as u64;
}

#[system(run_after(burn))]
fn watch(mut burns: Mut<Receiver<Burn>>, mut log: Mut<Resource<Log>>) {
    let tick = burns.tick();
    let seen: Vec<_> = burns.receive().map(|&Burn(unit)| (tick, unit)).collect();
    log.seen_after.extend(seen);
}

fn world(fuel: [u32; 3]) -> (World, [Entity; 3]) {
    let mut units = [Entity::default(); 3];
    let world = World::build(|setup| {
        for (unit, fuel) in units.iter_mut().zip(fuel) {
            *unit = setup.spawn();
            setup.set_component(*unit, Fuel(fuel));
        }
    });
    (world, units)
}

fn ticks_of(list: &[(u64, Entity)], unit: Entity) -> Vec<u64> {
    list.iter().filter(|x| x.1 == unit).map(|x| x.0).collect()
}

#[test]
fn open_signal_stops_when_retain_returns_false() {
    let (mut world, [a, b, c]) = world([3, 1, 5]);
    world.input(Ignite(a));
    world.input(Ignite(b));
    world.step();
    world.input(Ignite(c));
    for _ in 0..10 {
        world.step();
    }

    let log = world.get_resource::<Log>().unwrap();
    assert_eq!(ticks_of(&log.burned, a), [0, 1, 2]);
    assert_eq!(ticks_of(&log.burned, b), [0]);
    assert_eq!(ticks_of(&log.burned, c), [1, 2, 3, 4, 5]);
    // A system after the retaining one doesn't get the signal on the tick it's cancelled.
    assert_eq!(ticks_of(&log.seen_after, a), [0, 1]);
    assert_eq!(ticks_of(&log.seen_after, b), [] as [u64; 0]);
    assert_eq!(ticks_of(&log.seen_after, c), [1, 2, 3, 4]);
    assert_eq!(log.cancelled, 3);
    drop(log);
    for unit in [a, b, c] {
        assert_eq!(world.get_component::<Fuel>(unit).unwrap().0, 0);
    }
    let signals = world.get_resource::<Signals<Burn>>().unwrap();
    assert_eq!(signals.permanent_len(), 0, "cancelled signals left storage");
}

#[test]
fn retain_matches_after_save_and_load() {
    let (mut world, [a, b, c]) = world([4, 9, 6]);
    world.input(Ignite(a));
    world.input(Ignite(b));
    world.input(Ignite(c));
    for _ in 0..3 {
        world.step();
    }

    let bytes = world.save();
    let mut restored = World::new();
    restored.load(&bytes);
    for _ in 0..10 {
        world.step();
        restored.step();
        assert_eq!(world.save(), restored.save());
    }
    for unit in [a, b, c] {
        assert_eq!(restored.get_component::<Fuel>(unit).unwrap().0, 0);
    }
}

#[system]
fn light(mut lights: Mut<Receiver<Light>>, mut smoke: Mut<Sender<Smoke>>) {
    for &Light(unit) in lights.receive() {
        smoke.send(Smoke(unit), ..);
    }
}

#[system(run_after(light))]
fn smoke(
    mut smoke: Mut<Receiver<Smoke>>,
    mut ash: Mut<Component<Ash>>,
    mut log: Mut<Resource<SmokeLog>>,
) {
    let tick = smoke.tick();
    let mut received = false;
    smoke.retain(|&Smoke(unit)| {
        received = true;
        let Some(left) = ash.get_mut(unit) else {
            return false;
        };
        left.0 -= 1;
        left.0 > 0
    });
    log.ran.push(tick);
    if received {
        log.received.push(tick);
    }
}

#[test]
fn receiver_retain_sleeps_before_and_after_its_signals() {
    let mut unit = Entity::default();
    let mut world = World::build(|setup| {
        unit = setup.spawn();
        setup.set_component(unit, Ash(3));
    });
    for _ in 0..2 {
        world.step();
    }
    world.input(Light(unit));
    for _ in 0..20 {
        world.step();
    }

    let log = world.get_resource::<SmokeLog>().unwrap();
    assert_eq!(log.received, [2, 3, 4]);
    assert_eq!(
        log.ran.first(),
        Some(&2),
        "ran before anything was sent: {:?}",
        log.ran
    );
    // It may wake a little after the cancel, until the cancelled signal leaves storage.
    assert!(
        log.ran.iter().all(|&tick| tick <= 6),
        "kept running after its signal was cancelled: {:?}",
        log.ran
    );
    drop(log);
    assert_eq!(world.get_component::<Ash>(unit).unwrap().0, 0);
}
