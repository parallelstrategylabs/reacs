use reacs::prelude::*;
use reacs::tracking::TrackingSave;

#[signal(input)]
struct FixturePing(u32);

#[signal]
struct FixtureBeam(u32);

#[signal]
struct FixtureEcho(u32);

#[signal]
struct FixtureSelf(u32);

#[system]
fn fixture_early(mut pings: Mut<Signal<FixturePing>>) {
    let _ = pings.receive().count();
}

#[system(run_after(fixture_early))]
fn fixture_emit(mut pings: Mut<Signal<FixturePing>>, mut beams: Mut<Signal<FixtureBeam>>) {
    let tick = pings.tick();
    if tick != 1 {
        pings.emit(FixturePing(1));
        pings.emit(FixturePing(2));
    }
    if tick == 0 {
        beams.send(FixtureBeam(1), 2..4);
    }
}

#[system(run_after(fixture_emit))]
fn fixture_late(mut pings: Mut<Signal<FixturePing>>, mut echoes: Mut<Signal<FixtureEcho>>) {
    if pings.receive().count() > 0 {
        echoes.emit(FixtureEcho(1));
    }
}

#[system(run_after(fixture_late))]
fn fixture_tail(mut echoes: Mut<Signal<FixtureEcho>>, mut own: Mut<Signal<FixtureSelf>>) {
    if echoes.receive().count() > 0 {
        own.emit(FixtureSelf(1));
    }
    let _ = own.receive().count();
}

#[system]
fn fixture_beam_reader(mut beams: Mut<Signal<FixtureBeam>>) {
    let _ = beams.receive().count();
}

pub fn recorded(ticks: u64) -> TrackingSave {
    recorded_paused(ticks, None)
}

pub fn recorded_paused(ticks: u64, pause_after: Option<u64>) -> TrackingSave {
    let mut world = World::new();
    let mut tracker = world.to_tracker();
    for tick in 0..ticks {
        tracker.step(&world);
        world.step();
        if pause_after == Some(tick) {
            tracker.pause();
            tracker.resume();
        }
    }
    TrackingSave::from_bytes(&tracker.snapshot()).unwrap()
}

pub fn recorded_with_outside_send() -> TrackingSave {
    let mut world = World::new();
    let mut tracker = world.to_tracker();
    world.input(FixturePing(9));
    tracker.step(&world);
    TrackingSave::from_bytes(&tracker.snapshot()).unwrap()
}
