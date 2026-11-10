use reacs::prelude::*;
use reacs::record::{Playback, Recorder, Recording, send_recorded};
use std::fs;
use std::path::PathBuf;

#[signal(input)]
pub struct Order(pub u64);

#[signal(input)]
pub struct Pause(pub u8);

#[signal]
pub struct Blow(pub u64);

#[resource]
pub struct Trace(pub u64);

#[resource]
pub struct Rng(pub u64);

#[resource(local)]
pub struct Bias(pub u64);

fn mix(trace: &mut Trace, value: u64) {
    trace.0 = trace.0.wrapping_mul(1_000_003).wrapping_add(value);
}

#[system]
fn take_orders(
    mut orders: Mut<Signal<Order>>,
    mut pauses: Mut<Signal<Pause>>,
    mut blows: Mut<Signal<Blow>>,
    mut rng: Mut<Resource<Rng>>,
) {
    for order in orders.receive() {
        (*rng).0 = (*rng).0.wrapping_mul(31).wrapping_add(order.0);
        blows.emit(Blow((*rng).0));
    }
    for pause in pauses.receive() {
        blows.emit(Blow(pause.0 as u64));
    }
}

#[system(run_after(take_orders))]
fn land_blows(
    mut blows: Mut<Signal<Blow>>,
    mut trace: Mut<Resource<Trace>>,
    bias: LocalResource<Bias>,
) {
    for blow in blows.receive() {
        mix(&mut trace, blow.0 + (*bias).0);
    }
    mix(&mut trace, (*bias).0 + 1);
}

#[system(run_after(land_blows))]
fn echo_order(mut orders: Mut<Signal<Order>>) {
    if orders.tick() == 5 {
        orders.emit(Order(500));
    }
}

fn path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("reacs-recording-{}-{name}", std::process::id()))
}

fn play(world: &mut World, recorder: &mut Recorder, ticks: std::ops::Range<u64>) {
    for tick in ticks {
        if tick % 3 == 0 {
            world.input(Order(tick));
        }
        if tick % 7 == 0 {
            world.input(Pause(tick as u8));
        }
        recorder.record(world);
        world.step();
    }
}

#[test]
fn the_chained_form_writes_the_same_file_as_the_two_lines() {
    let run = |name: &str, chained: bool| {
        let file = path(name);
        let mut world = World::new();
        let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(5);
        for tick in 0..30 {
            world.input(Order(tick));
            if chained {
                world.record(&mut recorder).step();
            } else {
                recorder.record(&world);
                world.step();
            }
        }
        recorder.finish().unwrap();
        let bytes = fs::read(&file).unwrap();
        fs::remove_file(&file).unwrap();
        bytes
    };

    assert_eq!(run("two-lines", false), run("chained", true));
}

#[test]
fn ticks_with_nothing_to_record_take_no_space() {
    let size = |ticks: u64| {
        let file = path(&format!("quiet-{ticks}"));
        let mut world = World::new();
        let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(u64::MAX);
        for _ in 0..ticks {
            recorder.record(&world);
            world.step();
        }
        recorder.finish().unwrap();
        let size = fs::metadata(&file).unwrap().len();
        fs::remove_file(&file).unwrap();
        size
    };

    assert_eq!(size(3), size(250));
}

#[test]
fn a_game_can_compare_two_worlds_by_their_resource_hashes() {
    let mut world = World::new();
    let mut copy = World::new();
    for tick in 0..20 {
        world.input(Order(tick));
        world.step();
        copy.input(Order(tick));
        copy.step();
    }
    assert_eq!(world.resource_hashes(), copy.resource_hashes());

    world.input(Order(1));
    copy.input(Order(2));
    world.step();
    copy.step();
    world.step();
    copy.step();

    let ids = reacs::registry::registered_ids();
    let changed: Vec<&str> = world
        .resource_hashes()
        .iter()
        .zip(copy.resource_hashes())
        .filter(|(ours, theirs)| **ours != *theirs)
        .map(|(ours, _)| {
            assert!(ids.contains(&ours.0));
            reacs::registry::get_registry_entry(ours.0)
                .unwrap()
                .type_name
        })
        .collect();
    assert!(
        changed.iter().any(|name| name.ends_with("Rng")),
        "{changed:?}"
    );
    assert!(
        changed.iter().any(|name| name.ends_with("Trace")),
        "{changed:?}"
    );
    assert!(
        !changed.iter().any(|name| name.ends_with("Tick")),
        "{changed:?}"
    );
}

#[test]
fn a_recording_reads_back_as_data_a_tool_can_run_again() {
    let file = path("reader");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(4);
    play(&mut world, &mut recorder, 0..10);
    let save = world.save();
    play(&mut world, &mut recorder, 10..12);
    world.load(&save);
    play(&mut world, &mut recorder, 10..14);
    recorder.finish().unwrap();

    let recording = Recording::open(&file).unwrap();
    fs::remove_file(&file).unwrap();

    assert!(!recording.truncated);
    let starts: Vec<u64> = recording.sections.iter().map(|x| x.tick).collect();
    assert_eq!(starts, [0, 10]);
    let ends: Vec<u64> = recording.sections.iter().map(|x| x.end).collect();
    assert_eq!(ends, [12, 14]);
    let ticks: Vec<u64> = recording.sections[0].ticks.iter().map(|x| x.tick).collect();
    assert_eq!(ticks, [0, 3, 4, 6, 7, 8, 9]);

    let mut copy = World::new();
    assert!(copy.restore(&recording.sections[0].state).is_some());
    for recorded in &recording.sections[0].ticks {
        while copy.get_resource::<Tick>().unwrap().0 < recorded.tick {
            copy.step();
        }
        for inputs in &recorded.inputs {
            for payload in &inputs.payloads {
                assert!(send_recorded(&mut copy, inputs.signal, payload));
            }
        }
        if let Some(hashes) = &recorded.hashes {
            assert_eq!(*hashes, copy.resource_hashes(), "tick {}", recorded.tick);
        }
    }
    assert!(!send_recorded(&mut copy, 12345, &[1]));
    assert!(copy.restore(b"not a save").is_none());
}

fn record_session(name: &str) -> (PathBuf, World) {
    let file = path(name);
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(4);
    play(&mut world, &mut recorder, 0..10);
    let save = world.save();
    play(&mut world, &mut recorder, 10..12);
    world.load(&save);
    play(&mut world, &mut recorder, 10..14);
    recorder.finish().unwrap();
    (file, world)
}

#[test]
fn a_playback_replays_a_session_with_a_load_into_the_same_world() {
    let (file, original) = record_session("playback");

    let mut world = World::new();
    let mut playback = world.to_playback(&file).unwrap();
    fs::remove_file(&file).unwrap();
    let mut steps = 0;
    while !playback.finished() {
        world.replay(&mut playback).step();
        steps += 1;
    }

    assert_eq!(playback.diverged(), None);
    assert_eq!(steps, 12 + 4);
    assert_eq!(world.get_resource::<Tick>().unwrap().0, 14);
    assert_eq!(world.resource_hashes(), original.resource_hashes());
    assert_eq!(world.save(), original.save());
}

#[test]
fn a_playback_reports_where_a_different_world_diverges() {
    let (file, _) = record_session("diverging");

    let mut world = World::new();
    let mut playback = world.to_playback(&file).unwrap();
    fs::remove_file(&file).unwrap();
    world.insert_local_resource(Bias(1));
    while !playback.finished() {
        world.replay(&mut playback).step();
    }

    let divergence = playback.diverged().unwrap();
    assert_eq!(divergence.tick, 4);
    let names: Vec<&str> = divergence
        .resources
        .iter()
        .map(|id| reacs::registry::get_registry_entry(*id).unwrap().type_name)
        .collect();
    assert!(
        names.iter().any(|name| name.ends_with("Trace")),
        "{names:?}"
    );
    assert!(!names.iter().any(|name| name.ends_with("Rng")), "{names:?}");
}

fn tick(world: &World) -> u64 {
    world.get_resource::<Tick>().unwrap().0
}

#[test]
fn a_game_can_pause_and_change_speed_while_replaying() {
    let (file, original) = record_session("viewer");

    let mut world = World::new();
    let mut playback = world.to_playback(&file).unwrap();
    fs::remove_file(&file).unwrap();
    let speeds = [1, 0, 0, 2, 4, 0, 1, 3];
    let mut frame = 0;
    let mut seen = Vec::new();
    while !playback.finished() {
        for _ in 0..speeds[frame % speeds.len()] {
            if playback.finished() {
                break;
            }
            world.replay(&mut playback).step();
        }
        seen.push(world.get_resource::<Trace>().unwrap().0);
        frame += 1;
    }

    assert_eq!(playback.diverged(), None);
    assert!(frame > 10);
    assert!(seen.windows(2).any(|pair| pair[0] == pair[1]));
    assert_eq!(world.save(), original.save());
}

#[test]
fn a_replay_can_stop_at_a_tick_and_go_on_as_a_new_recording() {
    for stop in [6, 12, 13] {
        let (file, _) = record_session(&format!("branch-source-{stop}"));
        let branch = path(&format!("branch-{stop}"));

        let mut world = World::new();
        let mut playback = world.to_playback(&file).unwrap();
        fs::remove_file(&file).unwrap();
        while !playback.finished() && tick(&world) < stop {
            world.replay(&mut playback).step();
        }
        drop(playback);
        assert_eq!(tick(&world), stop);

        let mut recorder = world.to_recorder(&branch).unwrap().with_hash_every(4);
        for t in stop..stop + 8 {
            if t % 2 == 0 {
                world.input(Order(t * 10));
            }
            world.record(&mut recorder).step();
        }
        recorder.finish().unwrap();

        let recording = Recording::open(&branch).unwrap();
        assert_eq!(recording.sections.len(), 1);
        assert_eq!(recording.sections[0].tick, stop);

        let mut copy = World::new();
        let mut playback = copy.to_playback(&branch).unwrap();
        fs::remove_file(&branch).unwrap();
        while !playback.finished() {
            copy.replay(&mut playback).step();
        }
        assert_eq!(playback.diverged(), None, "stop at {stop}");
        assert_eq!(tick(&copy), stop + 8);
        assert_eq!(copy.save(), world.save(), "stop at {stop}");
    }
}

fn replay_to(world: &mut World, playback: &mut Playback, stop: u64) {
    while !playback.finished() && tick(world) < stop {
        world.replay(playback).step();
    }
    assert_eq!(tick(world), stop);
}

fn play_branch(world: &mut World, recorder: &mut Recorder, ticks: std::ops::Range<u64>) {
    for t in ticks {
        if t % 2 == 0 {
            world.input(Order(t * 10));
        }
        world.record(recorder).step();
    }
}

fn replay_whole(file: &PathBuf) -> (World, Recording) {
    let recording = Recording::open(file).unwrap();
    let mut world = World::new();
    let mut playback = world.to_playback(file).unwrap();
    while !playback.finished() {
        world.replay(&mut playback).step();
    }
    assert_eq!(playback.diverged(), None);
    (world, recording)
}

#[test]
fn a_branch_keeps_the_history_and_replays_from_the_start() {
    for (stop, sections) in [(0, 1), (6, 1), (12, 1), (13, 2)] {
        let (file, _) = record_session(&format!("history-source-{stop}"));
        let branch = path(&format!("history-{stop}"));

        let mut world = World::new();
        let mut playback = world.to_playback(&file).unwrap();
        fs::remove_file(&file).unwrap();
        replay_to(&mut world, &mut playback, stop);
        let mut recorder = playback.to_recorder(&world, &branch).unwrap();
        play_branch(&mut world, &mut recorder, stop..stop + 8);
        recorder.finish().unwrap();

        let (copy, recording) = replay_whole(&branch);
        fs::remove_file(&branch).unwrap();
        assert_eq!(recording.sections[0].tick, 0, "stop at {stop}");
        assert_eq!(recording.sections.len(), sections, "stop at {stop}");
        assert_eq!(tick(&copy), stop + 8);
        assert_eq!(copy.save(), world.save(), "stop at {stop}");
    }
}

#[test]
fn a_branch_can_replace_the_file_it_replays() {
    let (file, _) = record_session("replace");

    let mut world = World::new();
    let mut playback = world.to_playback(&file).unwrap();
    replay_to(&mut world, &mut playback, 7);
    let mut recorder = playback.to_recorder(&world, &file).unwrap();
    play_branch(&mut world, &mut recorder, 7..15);
    recorder.finish().unwrap();

    let (copy, recording) = replay_whole(&file);
    fs::remove_file(&file).unwrap();
    assert_eq!(recording.sections.len(), 1);
    assert_eq!(copy.save(), world.save());
}

#[test]
fn a_branch_of_a_branch_keeps_both_histories() {
    let (file, _) = record_session("nested");

    let mut world = World::new();
    let mut playback = world.to_playback(&file).unwrap();
    replay_to(&mut world, &mut playback, 6);
    let mut recorder = playback.to_recorder(&world, &file).unwrap();
    play_branch(&mut world, &mut recorder, 6..14);
    recorder.finish().unwrap();

    let mut world = World::new();
    let mut playback = world.to_playback(&file).unwrap();
    replay_to(&mut world, &mut playback, 10);
    let mut recorder = playback.to_recorder(&world, &file).unwrap();
    for t in 10..15 {
        world.input(Pause(t as u8));
        world.record(&mut recorder).step();
    }
    recorder.finish().unwrap();

    let (copy, _) = replay_whole(&file);
    fs::remove_file(&file).unwrap();
    assert_eq!(tick(&copy), 15);
    assert_eq!(copy.save(), world.save());
}

#[test]
fn inputs_on_the_branch_tick_are_recorded_once() {
    for replayed_first in [false, true] {
        let (file, _) = record_session(&format!("boundary-source-{replayed_first}"));
        let branch = path(&format!("boundary-{replayed_first}"));

        let mut world = World::new();
        let mut playback = world.to_playback(&file).unwrap();
        fs::remove_file(&file).unwrap();
        replay_to(&mut world, &mut playback, 9);
        if replayed_first {
            let _ = world.replay(&mut playback);
        }
        world.input(Order(999));
        let mut recorder = playback.to_recorder(&world, &branch).unwrap();
        world.record(&mut recorder).step();
        play_branch(&mut world, &mut recorder, 10..14);
        recorder.finish().unwrap();

        let (copy, recording) = replay_whole(&branch);
        fs::remove_file(&branch).unwrap();
        let at_nine = recording.sections[0]
            .ticks
            .iter()
            .find(|x| x.tick == 9)
            .unwrap();
        let orders = at_nine
            .inputs
            .iter()
            .map(|x| x.payloads.len())
            .sum::<usize>();
        assert_eq!(
            orders,
            if replayed_first { 2 } else { 1 },
            "{replayed_first}"
        );
        assert_eq!(copy.save(), world.save(), "{replayed_first}");
    }
}

#[test]
fn a_branch_refuses_a_world_the_playback_did_not_bring_there() {
    let (file, _) = record_session("refused");
    let mut world = World::new();
    let mut playback = world.to_playback(&file).unwrap();
    fs::remove_file(&file).unwrap();
    replay_to(&mut world, &mut playback, 5);
    world.step();
    world.step();

    let error = playback
        .to_recorder(&world, path("refused-branch"))
        .err()
        .unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn a_load_to_the_next_tick_is_replayed() {
    let mut other = World::new();
    for t in 0..8 {
        other.input(Order(t * 100 + 1));
        other.step();
    }
    let other_game = other.save();

    let file = path("next-tick-load");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(4);
    play(&mut world, &mut recorder, 0..8);
    assert_eq!(tick(&world), 8);
    world.load(&other_game);
    assert_eq!(tick(&world), 8);
    play(&mut world, &mut recorder, 8..12);
    recorder.finish().unwrap();

    let (copy, recording) = replay_whole(&file);
    fs::remove_file(&file).unwrap();
    assert_eq!(recording.sections.len(), 2);
    assert_eq!(copy.save(), world.save());
}

#[test]
fn a_load_between_recording_and_stepping_a_tick_is_replayed() {
    let file = path("mid-frame-load");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(4);
    play(&mut world, &mut recorder, 0..6);
    let quick = world.save();
    world.input(Order(77));
    recorder.record(&world);
    world.load(&quick);
    play(&mut world, &mut recorder, 6..10);
    recorder.finish().unwrap();

    let (copy, recording) = replay_whole(&file);
    fs::remove_file(&file).unwrap();
    assert_eq!(recording.sections.len(), 2);
    assert_eq!(recording.sections[0].end, 6);
    assert_eq!(copy.save(), world.save());
}

#[test]
fn a_file_that_is_not_a_recording_is_rejected() {
    let file = path("junk");
    fs::write(&file, b"definitely not a recording").unwrap();
    let result = Recording::open(&file);
    fs::remove_file(&file).unwrap();

    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
}
