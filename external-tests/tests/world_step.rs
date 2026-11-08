use reacs::prelude::*;

#[signal]
pub struct Once(pub u64);

#[signal]
pub struct Windowed(pub u64);

#[resource]
pub struct Seen {
    pub ticks: Vec<u64>,
    pub once: Vec<(u64, u64)>,
    pub windowed: Vec<(u64, u64)>,
}

#[system]
fn record(
    mut once: Mut<Signal<Once>>,
    mut windowed: Mut<Signal<Windowed>>,
    tick: Resource<Tick>,
    mut seen: Mut<Resource<Seen>>,
) {
    let tick = (*tick).0;
    seen.ticks.push(tick);
    for signal in once.receive() {
        seen.once.push((tick, signal.0));
    }
    for signal in windowed.receive() {
        seen.windowed.push((tick, signal.0));
    }
}

#[test]
fn world_new_starts_at_tick_zero() {
    let world = World::new();

    assert_eq!(world.get_resource::<Tick>().unwrap().0, 0);
}

#[test]
fn step_runs_systems_then_advances_the_tick() {
    let mut world = World::new();

    world.step();
    world.step();

    assert_eq!(world.get_resource::<Tick>().unwrap().0, 2);
    assert_eq!(world.get_resource::<Seen>().unwrap().ticks, [0, 1]);
}

#[test]
fn step_delivers_a_one_tick_signal_once() {
    let mut world = World::new();
    world.signal::<Once>().emit(Once(7));

    world.step();
    world.step();

    assert_eq!(world.get_resource::<Seen>().unwrap().once, [(0, 7)]);
}

#[test]
fn step_delivers_a_windowed_signal_every_tick_of_its_window() {
    let mut world = World::new();
    world.signal::<Windowed>().send(Windowed(9), 1..3);

    for _ in 0..5 {
        world.step();
    }

    assert_eq!(
        world.get_resource::<Seen>().unwrap().windowed,
        [(1, 9), (2, 9)]
    );
}
