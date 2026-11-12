use reacs::prelude::*;

#[signal]
pub struct Ping(pub u64);

#[signal]
pub struct Pulse(pub u64);

#[signal]
pub struct Beat(pub u64);

#[signal(input)]
pub struct Poke(pub u64);

#[resource]
pub struct Runs {
    pub sender: Vec<u64>,
    pub early: Vec<(u64, Vec<u64>)>,
    pub late: Vec<(u64, Vec<u64>)>,
    pub signal: Vec<u64>,
    pub plain: Vec<u64>,
    pub poke: Vec<(u64, Vec<u64>)>,
    pub pulse: Vec<u64>,
    pub beat: Vec<u64>,
    pub watch: Vec<u64>,
    pub loud: Vec<u64>,
    pub relay: Vec<u64>,
    pub steady: Vec<u64>,
}

#[param(write)]
pub struct Watch {
    pub ping: Receiver<Ping>,
    pub pulse: Receiver<Pulse>,
}

#[param(write)]
pub struct Loud {
    pub ping: Receiver<Ping>,
    pub pulse: Signal<Pulse>,
}

#[param(write)]
pub struct Relay {
    pub ping: Receiver<Ping>,
    pub beat: Sender<Beat>,
}

#[param(write, wake = always)]
pub struct Steady {
    pub ping: Receiver<Ping>,
}

#[system]
fn sender(mut pings: Mut<Sender<Ping>>, mut runs: Mut<Resource<Runs>>) {
    let tick = pings.tick();
    runs.sender.push(tick);
    if tick % 5 == 0 {
        pings.emit(Ping(tick));
    }
}

#[system(run_before(sender))]
fn early(mut pings: Mut<Receiver<Ping>>, mut runs: Mut<Resource<Runs>>) {
    let tick = pings.tick();
    let got = pings.receive().map(|p| p.0).collect();
    runs.early.push((tick, got));
}

#[system(run_after(sender))]
fn late(mut pings: Mut<Receiver<Ping>>, mut runs: Mut<Resource<Runs>>) {
    let tick = pings.tick();
    let got = pings.receive().map(|p| p.0).collect();
    runs.late.push((tick, got));
}

#[system]
fn signal(mut pings: Mut<Signal<Ping>>, mut runs: Mut<Resource<Runs>>) {
    let tick = pings.tick();
    pings.receive().for_each(drop);
    runs.signal.push(tick);
}

#[system]
fn plain(tick: Resource<Tick>, mut runs: Mut<Resource<Runs>>) {
    runs.plain.push((*tick).0);
}

#[system]
fn poke(mut pokes: Mut<Receiver<Poke>>, mut runs: Mut<Resource<Runs>>) {
    let tick = pokes.tick();
    let got = pokes.receive().map(|p| p.0).collect();
    runs.poke.push((tick, got));
}

#[system]
fn pulse(mut pulses: Mut<Receiver<Pulse>>, mut runs: Mut<Resource<Runs>>) {
    let tick = pulses.tick();
    pulses.receive().for_each(drop);
    runs.pulse.push(tick);
}

#[system]
fn beat(mut beats: Mut<Receiver<Beat>>, mut runs: Mut<Resource<Runs>>) {
    let tick = beats.tick();
    beats.receive().for_each(drop);
    runs.beat.push(tick);
}

#[system(run_after(sender))]
fn watch(mut watch: Mut<Watch>, mut runs: Mut<Resource<Runs>>) {
    let tick = watch.ping.tick();
    watch.ping.receive().for_each(drop);
    watch.pulse.receive().for_each(drop);
    runs.watch.push(tick);
}

#[system(run_after(sender))]
fn loud(mut loud: Mut<Loud>, mut runs: Mut<Resource<Runs>>) {
    let tick = loud.ping.tick();
    loud.ping.receive().for_each(drop);
    runs.loud.push(tick);
}

#[system(run_after(sender))]
fn relay(mut relay: Mut<Relay>, mut runs: Mut<Resource<Runs>>) {
    let tick = relay.ping.tick();
    relay.ping.receive().for_each(drop);
    runs.relay.push(tick);
}

#[system(run_after(sender))]
fn steady(mut steady: Mut<Steady>, mut runs: Mut<Resource<Runs>>) {
    let tick = steady.ping.tick();
    steady.ping.receive().for_each(drop);
    runs.steady.push(tick);
}

const TICKS: u64 = 40;

fn new_world() -> World {
    World::build(|setup| {
        setup.signal::<Pulse>().send(Pulse(1), (At(10), For(3)));
        setup.signal::<Pulse>().send(Pulse(2), In(20));
        setup.signal::<Beat>().send(Beat(1), (At(30), ..));
    })
}

fn step(world: &mut World) {
    let tick = world.get_resource::<Tick>().unwrap().0;
    if tick == 3 || tick == 17 {
        world.input(Poke(tick));
    }
    world.step();
}

#[test]
fn gated_systems_run_only_when_they_have_something_to_receive() {
    let mut world = new_world();
    for _ in 0..TICKS {
        step(&mut world);
    }
    let runs = world.get_resource::<Runs>().unwrap();

    let every: Vec<u64> = (0..TICKS).collect();
    let pings: Vec<u64> = (0..TICKS).step_by(5).collect();
    assert_eq!(runs.sender, every);
    assert_eq!(runs.signal, every);
    assert_eq!(runs.plain, every);
    assert_eq!(runs.loud, every);
    assert_eq!(runs.steady, every);

    assert_eq!(
        runs.late,
        pings.iter().map(|&t| (t, vec![t])).collect::<Vec<_>>()
    );
    assert_eq!(
        runs.early,
        pings.iter().map(|&t| (t + 1, vec![t])).collect::<Vec<_>>()
    );
    assert_eq!(runs.relay, pings);

    assert_eq!(runs.poke, vec![(3, vec![3]), (17, vec![17])]);
    assert_eq!(runs.pulse, vec![10, 11, 12, 20]);
    assert_eq!(runs.beat, (30..TICKS).collect::<Vec<_>>());

    let mut watch: Vec<u64> = pings.clone();
    watch.extend([10, 11, 12, 20]);
    watch.sort_unstable();
    watch.dedup();
    assert_eq!(runs.watch, watch);
}

#[test]
fn skipped_ticks_restore_and_continue_identically() {
    let mut straight = new_world();
    for _ in 0..TICKS {
        step(&mut straight);
    }

    let mut first = new_world();
    for _ in 0..TICKS / 2 {
        step(&mut first);
    }
    let bytes = first.save();
    let mut resumed = new_world();
    resumed.load(&bytes);
    for _ in TICKS / 2..TICKS {
        step(&mut resumed);
    }

    assert!(straight.save() == resumed.save());
}

#[test]
fn system_order_marks_gated_systems() {
    let order = World::new().system_order();
    let line = |name: &str| {
        order
            .lines()
            .find(|l| l.ends_with(&format!("::{name}")) || l.contains(&format!("::{name} (")))
            .unwrap_or_else(|| panic!("no `{name}` in\n{order}"))
            .to_string()
    };

    assert!(line("early").ends_with("::early (wakes on Ping)"));
    assert!(line("watch").ends_with("::watch (wakes on Ping, Pulse)"));
    assert!(line("relay").ends_with("::relay (wakes on Ping)"));
    for always in ["sender", "signal", "plain", "loud", "steady"] {
        assert!(
            line(always).ends_with(&format!("::{always}")),
            "{}",
            line(always)
        );
    }
}
