use reacs::prelude::*;
use reacs::replay::{Recorder, Replay, ReplayError};
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
    std::env::temp_dir().join(format!("reacs-replay-{}-{name}", std::process::id()))
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
fn a_recorded_run_replays_with_every_hash_matching() {
    let file = path("match");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(8);
    play(&mut world, &mut recorder, 0..200);
    recorder.finish().unwrap();

    let report = Replay::open(&file).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();

    assert_eq!(report.divergence, None);
    assert!(!report.truncated);
    assert_eq!(report.hashes_checked, 25);
    assert_eq!(report.inputs, 67 + 29);
}

#[test]
fn replaying_twice_gives_the_same_report() {
    let file = path("twice");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(4);
    play(&mut world, &mut recorder, 0..64);
    recorder.finish().unwrap();

    let replay = Replay::open(&file).unwrap();
    let first = replay.verify().unwrap();
    let second = replay.verify().unwrap();
    fs::remove_file(&file).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.divergence, None);
}

#[test]
fn inputs_sent_by_systems_are_not_recorded() {
    let file = path("system");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(1);
    for _ in 0..12 {
        recorder.record(&world);
        world.step();
    }
    recorder.finish().unwrap();

    let report = Replay::open(&file).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();

    assert_eq!(report.inputs, 0);
    assert_eq!(report.divergence, None);
}

#[test]
fn inputs_sent_in_build_are_part_of_the_start_state() {
    let file = path("build");
    let mut world = World::build(|setup| {
        setup.signal::<Order>().emit(Order(99));
    });
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(1);
    world.input(Order(1));
    for _ in 0..4 {
        recorder.record(&world);
        world.step();
    }
    recorder.finish().unwrap();

    let report = Replay::open(&file).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();

    assert_eq!(report.inputs, 1);
    assert_eq!(report.divergence, None);
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
fn recording_the_same_tick_twice_stores_its_inputs_once() {
    let file = path("double");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(2);
    world.input(Order(1));
    recorder.record(&world);
    recorder.record(&world);
    world.step();
    recorder.record(&world);
    recorder.finish().unwrap();

    let report = Replay::open(&file).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();

    assert_eq!(report.inputs, 1);
    assert_eq!(report.divergence, None);
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

    assert_eq!(size(3), size(300));
}

#[test]
fn a_local_resource_changing_gameplay_is_reported_between_two_hashes() {
    let file = path("local");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(8);
    for tick in 0..64 {
        if tick == 30 {
            world.get_local_resource_mut::<Bias>().unwrap().0 = 1;
        }
        recorder.record(&world);
        world.step();
    }
    recorder.finish().unwrap();

    let report = Replay::open(&file).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();

    let divergence = report.divergence.unwrap();
    assert_eq!(divergence.last_match, Some(24));
    assert_eq!(divergence.tick, 32);
    let names: Vec<&str> = divergence.resources.iter().map(|x| x.name).collect();
    assert_eq!(names.len(), 1, "{names:?}");
    assert!(names[0].ends_with("Trace"), "{names:?}");
    assert_ne!(
        divergence.resources[0].expected,
        divergence.resources[0].found
    );
}

#[signal]
pub struct Churn(pub u64);

#[resource]
pub struct ChurnCount(pub u64);

#[system]
fn churn(mut churns: Mut<Signal<Churn>>, mut count: Mut<Resource<ChurnCount>>) {
    let tick = churns.tick();
    for offset in 0..20 {
        churns.send(Churn(offset), tick..tick + 3 + offset);
    }
    let received = churns.receive().count() as u64;
    (*count).0 = (*count).0.wrapping_add(received);
}

#[test]
fn hashes_ignore_caches_that_a_restored_world_does_not_have() {
    let file = path("caches");
    let mut world = World::new();
    for _ in 0..200 {
        world.step();
    }
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(1);
    for _ in 0..200 {
        recorder.record(&world);
        world.step();
    }
    recorder.finish().unwrap();

    let report = Replay::open(&file).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();

    assert_eq!(report.hashes_checked, 200);
    assert_eq!(report.divergence, None);
}

#[resource]
pub struct Extra(pub u64);

#[resource]
pub struct LateTable(pub u64);

#[system]
fn add_extra(mut world: Mut<WorldAccess>) {
    if world
        .get_local_resource::<Bias>()
        .is_some_and(|bias| bias.0 == 2)
    {
        world.insert_resource(Extra(1));
    }
    if world.get_resource::<Tick>().unwrap().0 == 6 {
        world.insert_resource(LateTable(1));
    }
}

#[test]
fn a_resource_only_the_original_run_has_is_named() {
    let file = path("one-side");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(4);
    for tick in 0..16 {
        if tick == 9 {
            world.get_local_resource_mut::<Bias>().unwrap().0 = 2;
        }
        recorder.record(&world);
        world.step();
    }
    recorder.finish().unwrap();

    let report = Replay::open(&file).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();

    let divergence = report.divergence.unwrap();
    assert_eq!((divergence.last_match, divergence.tick), (Some(8), 12));
    let extra = divergence
        .resources
        .iter()
        .find(|x| x.name.ends_with("Extra"))
        .unwrap();
    assert!(extra.expected.is_some());
    assert_eq!(extra.found, None);
}

#[test]
fn a_resource_added_during_the_run_keeps_the_hashes_matching() {
    let file = path("late-table");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(2);
    play(&mut world, &mut recorder, 0..20);
    recorder.finish().unwrap();

    let report = Replay::open(&file).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();

    assert_eq!(report.hashes_checked, 10);
    assert_eq!(report.divergence, None);
}

#[test]
fn a_load_during_recording_starts_a_new_section() {
    let file = path("load");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(4);
    play(&mut world, &mut recorder, 0..10);
    let save = world.save();
    play(&mut world, &mut recorder, 10..20);
    world.load(&save);
    play(&mut world, &mut recorder, 10..30);
    recorder.finish().unwrap();

    let report = Replay::open(&file).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();

    assert_eq!(report.divergence, None);
    assert_eq!(report.last_tick, 28);
}

#[test]
fn a_step_that_was_not_recorded_starts_a_new_section() {
    let file = path("gap");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(1);
    play(&mut world, &mut recorder, 0..6);
    world.input(Order(77));
    world.step();
    play(&mut world, &mut recorder, 7..14);
    recorder.finish().unwrap();

    let report = Replay::open(&file).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();

    assert_eq!(report.divergence, None);
}

#[test]
fn a_cut_file_replays_up_to_the_cut() {
    let file = path("cut");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(4);
    play(&mut world, &mut recorder, 0..40);
    recorder.finish().unwrap();

    let bytes = fs::read(&file).unwrap();
    fs::write(&file, &bytes[..bytes.len() - 3]).unwrap();
    let report = Replay::open(&file).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();

    assert!(report.truncated);
    assert_eq!(report.divergence, None);
    assert!(report.last_tick < 39);
}

#[test]
fn a_recording_is_on_disk_without_calling_finish() {
    let file = path("no-finish");
    let mut world = World::new();
    let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(4);
    play(&mut world, &mut recorder, 0..20);
    let on_disk = fs::read(&file).unwrap();
    std::mem::forget(recorder);

    let copy = path("no-finish-copy");
    fs::write(&copy, &on_disk).unwrap();
    let report = Replay::open(&copy).unwrap().verify().unwrap();
    fs::remove_file(&file).unwrap();
    fs::remove_file(&copy).unwrap();

    assert!(!report.truncated);
    assert_eq!(report.divergence, None);
    assert!(report.inputs > 0);
}

#[test]
fn another_file_is_rejected() {
    let file = path("junk");
    fs::write(&file, b"definitely not a replay file").unwrap();
    let result = Replay::open(&file);
    fs::remove_file(&file).unwrap();

    assert!(matches!(result, Err(ReplayError::NotAReplay)));
}

#[test]
fn a_recording_from_another_build_is_rejected() {
    let file = path("build-id");
    let world = World::new();
    world.to_recorder(&file).unwrap().finish().unwrap();

    let mut bytes = fs::read(&file).unwrap();
    bytes[10] ^= 0xFF;
    fs::write(&file, &bytes).unwrap();
    let result = Replay::open(&file);
    fs::remove_file(&file).unwrap();

    assert!(matches!(result, Err(ReplayError::DifferentBuild)));
}
