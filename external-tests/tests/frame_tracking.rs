use reacs::prelude::*;
use reacs::tracking::{
    TRACKER_WINDOW_TICKS, TrackedSignalFrame, Tracker, TrackingSave, UNKNOWN_SENDER,
};

#[signal]
struct TrackPing(u64);

#[signal(entity)]
struct TrackHit(Entity);

#[signal(entity)]
struct TrackDie(Entity);

#[resource]
struct TrackPlan {
    reading: bool,
    emits_a: u64,
    emits_b: u64,
    window_at: u64,
    open_at: u64,
}

#[resource]
struct TrackKill(Option<Entity>);

#[system]
fn track_early(mut pings: Mut<Signal<TrackPing>>, plan: Resource<TrackPlan>) {
    if plan.reading {
        let _ = pings.receive().count();
    }
}

#[system(run_after(track_early))]
fn track_sender_a(mut pings: Mut<Signal<TrackPing>>, plan: Resource<TrackPlan>) {
    for i in 0..plan.emits_a {
        pings.emit(TrackPing(i));
    }
}

#[system(run_after(track_sender_a))]
fn track_sender_b(mut pings: Mut<Signal<TrackPing>>, plan: Resource<TrackPlan>) {
    let tick = pings.tick();
    for i in 0..plan.emits_b {
        pings.emit(TrackPing(i));
    }
    if plan.window_at == tick + 1 {
        pings.send(TrackPing(9), tick + 2..tick + 4);
    }
    if plan.open_at == tick + 1 {
        pings.send(TrackPing(10), ..);
    }
}

#[system(run_after(track_sender_b))]
fn track_late(mut pings: Mut<Signal<TrackPing>>, plan: Resource<TrackPlan>) {
    if plan.reading {
        let _ = pings.receive().count();
        let _ = pings.receive().count();
    }
}

#[system]
fn track_killer(mut em: Mut<EntityManager>, mut kill: Mut<Resource<TrackKill>>) {
    if let Some(entity) = (*kill).0.take() {
        em.despawn(entity);
    }
}

#[system(run_after(track_killer))]
fn track_hit_reader(mut hits: Mut<Signal<TrackHit>>, _em: Mut<EntityManager>) {
    let _ = hits.receive().count();
}

#[system]
fn track_die_reader(mut dies: Mut<Signal<TrackDie>>, mut em: Mut<EntityManager>) {
    for &TrackDie(entity) in dies.receive() {
        em.despawn(entity);
    }
}

fn world_with(plan: TrackPlan) -> World {
    let mut world = World::new();
    world.insert_resource(plan);
    world
}

fn reading() -> TrackPlan {
    TrackPlan {
        reading: true,
        ..Default::default()
    }
}

fn short(name: &str) -> String {
    name.rsplit("::").next().unwrap().to_string()
}

fn latest_tick(world: &World) -> u64 {
    world.get_resource::<Tick>().unwrap().0 - 1
}

fn frame_of(log: &TrackingSave, tick: u64, signal: &str) -> Option<TrackedSignalFrame> {
    let frame = log.frames().find(|x| x.tick == tick)?;
    frame
        .signals
        .iter()
        .find(|x| log.signal_name(x.signal).ends_with(signal))
        .cloned()
}

fn ticks_of(tracker: &Tracker) -> Vec<u64> {
    tracker.log().frames().map(|x| x.tick).collect()
}

type Row = (String, String, Option<u64>, u64);

fn received(tracker: &Tracker, signal: &str) -> Vec<Row> {
    let log = tracker.log();
    let Some(frame) = frame_of(log, latest_tick(tracker), signal) else {
        return Vec::new();
    };
    let mut out: Vec<Row> = frame
        .received
        .iter()
        .map(|x| {
            (
                short(log.system_name(x.receiver)),
                short(log.system_name(x.sender)),
                x.sent_tick,
                x.count,
            )
        })
        .collect();
    out.sort();
    out
}

fn row(receiver: &str, sender: &str, tick: Option<u64>, count: u64) -> Row {
    (receiver.to_string(), sender.to_string(), tick, count)
}

#[test]
fn counts_each_sender_for_each_receiver_on_the_right_tick() {
    let mut world = world_with(TrackPlan {
        emits_a: 3,
        emits_b: 2,
        ..reading()
    });
    let mut tracker = world.to_tracker();

    tracker.step();
    assert_eq!(
        received(&tracker, "TrackPing"),
        vec![
            row("track_late", "track_sender_a", Some(0), 3),
            row("track_late", "track_sender_b", Some(0), 2),
        ]
    );

    let frame = frame_of(tracker.log(), 0, "TrackPing").unwrap();
    let mut sent: Vec<_> = frame
        .sent
        .iter()
        .map(|x| (short(tracker.log().system_name(x.sender)), x.count))
        .collect();
    sent.sort();
    assert_eq!(
        sent,
        vec![
            ("track_sender_a".to_string(), 3),
            ("track_sender_b".to_string(), 2)
        ]
    );

    tracker.step();
    assert_eq!(
        received(&tracker, "TrackPing"),
        vec![
            row("track_early", "track_sender_a", Some(0), 3),
            row("track_early", "track_sender_b", Some(0), 2),
            row("track_late", "track_sender_a", Some(1), 3),
            row("track_late", "track_sender_b", Some(1), 2),
        ]
    );
}

#[test]
fn future_window_is_credited_only_while_it_runs() {
    let mut world = world_with(TrackPlan {
        window_at: 2,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    for tick in 0..6 {
        tracker.step();
        let expected = if tick == 3 || tick == 4 {
            vec![
                row("track_early", "track_sender_b", None, 1),
                row("track_late", "track_sender_b", None, 1),
            ]
        } else {
            vec![]
        };
        assert_eq!(received(&tracker, "TrackPing"), expected, "tick {tick}");
    }
}

#[test]
fn open_window_is_credited_through_every_list() {
    let mut world = world_with(TrackPlan {
        open_at: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    tracker.step();
    assert_eq!(
        received(&tracker, "TrackPing"),
        vec![row("track_late", "track_sender_b", Some(0), 1)]
    );
    for _ in 1..4 {
        tracker.step();
        assert_eq!(
            received(&tracker, "TrackPing"),
            vec![
                row("track_early", "track_sender_b", None, 1),
                row("track_late", "track_sender_b", None, 1),
            ]
        );
    }
}

#[test]
fn cancelled_windows_are_not_counted() {
    let mut world = world_with(reading());
    let mut tracker = world.to_tracker();
    let id = tracker
        .signal::<TrackPing>()
        .send(TrackPing(1), 0..u64::MAX - 1)
        .unwrap();
    tracker
        .signal::<TrackPing>()
        .send(TrackPing(2), 0..u64::MAX - 1);
    tracker.step();
    tracker.step();
    assert_eq!(
        received(&tracker, "TrackPing"),
        vec![
            row("track_early", "outside", None, 2),
            row("track_late", "outside", None, 2),
        ]
    );

    tracker.signal::<TrackPing>().cancel(id);
    tracker.step();
    assert_eq!(
        received(&tracker, "TrackPing"),
        vec![
            row("track_early", "outside", None, 1),
            row("track_late", "outside", None, 1),
        ]
    );
}

#[test]
fn filtered_counts_follow_deaths_and_payload_changes() {
    let mut world = world_with(TrackPlan::default());
    let mut tracker = world.to_tracker();
    let (first, second, third) = {
        let mut em = tracker.entity_manager();
        let entities = (em.spawn(), em.spawn(), em.spawn());
        em.despawn(entities.2);
        entities
    };
    tracker.step();

    tracker.signal::<TrackHit>().emit(TrackHit(first));
    tracker.signal::<TrackHit>().emit(TrackHit(third));
    tracker.step();
    assert_eq!(
        received(&tracker, "TrackHit"),
        vec![row("track_hit_reader", "outside", Some(1), 1)]
    );

    tracker.signal::<TrackHit>().send(TrackHit(first), ..);
    let id = tracker
        .signal::<TrackHit>()
        .send(TrackHit(second), ..)
        .unwrap();
    for _ in 0..3 {
        tracker.step();
    }
    assert_eq!(
        received(&tracker, "TrackHit"),
        vec![row("track_hit_reader", "outside", None, 2)]
    );

    tracker.insert_resource(TrackKill(Some(first)));
    tracker.step();
    tracker.step();
    assert_eq!(
        received(&tracker, "TrackHit"),
        vec![row("track_hit_reader", "outside", None, 1)]
    );

    tracker.signal::<TrackHit>().modify(id, TrackHit(first));
    tracker.step();
    assert_eq!(received(&tracker, "TrackHit"), vec![]);
}

#[test]
fn a_signal_whose_entity_dies_in_the_reading_loop_still_counts() {
    let mut world = world_with(TrackPlan::default());
    let mut tracker = world.to_tracker();
    let target = tracker.entity_manager().spawn();
    tracker.step();

    tracker.signal::<TrackDie>().emit(TrackDie(target));
    tracker.step();

    assert!(!tracker.entity_manager().is_alive(target));
    assert_eq!(
        received(&tracker, "TrackDie"),
        vec![row("track_die_reader", "outside", Some(1), 1)]
    );
}

#[test]
fn a_tracked_step_saves_the_same_bytes_as_a_plain_step() {
    let plan = || TrackPlan {
        emits_a: 2,
        window_at: 1,
        open_at: 2,
        ..reading()
    };
    let mut tracked = world_with(plan());
    let mut plain = world_with(plan());
    let mut tracker = tracked.to_tracker();

    for _ in 0..8 {
        tracker.step();
        plain.step();
        assert_eq!(tracker.save(), plain.save());
    }
}

#[test]
fn signals_sent_before_the_tracker_started_have_an_unknown_sender() {
    let mut world = world_with(reading());
    world.signal::<TrackPing>().send(TrackPing(1), ..);
    world.step();
    let mut tracker = world.to_tracker();
    tracker.step();

    let frame = frame_of(tracker.log(), latest_tick(&tracker), "TrackPing").unwrap();
    assert!(!frame.received.is_empty());
    assert!(frame.received.iter().all(|x| x.sender == UNKNOWN_SENDER));
}

#[test]
fn a_tracker_on_a_loaded_snapshot_matches_one_tracking_from_the_start() {
    let plan = || TrackPlan {
        emits_a: 2,
        emits_b: 1,
        ..reading()
    };
    let mut tracked = world_with(plan());
    let mut plain = world_with(plan());
    let mut tracked_log = tracked.to_tracker();
    for _ in 0..3 {
        tracked_log.step();
        plain.step();
    }

    let mut resumed = world_with(plan());
    resumed.load(&plain.save());
    let mut resumed_log = resumed.to_tracker();

    tracked_log.step();
    resumed_log.step();
    assert_eq!(tracked_log.save(), resumed_log.save());

    let tick = latest_tick(&tracked_log);
    let total = |tracker: &Tracker| -> u64 {
        frame_of(tracker.log(), tick, "TrackPing")
            .unwrap()
            .received
            .iter()
            .map(|x| x.count)
            .sum()
    };
    assert_eq!(total(&tracked_log), total(&resumed_log));
    assert!(
        frame_of(resumed_log.log(), tick, "TrackPing")
            .unwrap()
            .received
            .iter()
            .any(|x| x.sender == UNKNOWN_SENDER)
    );

    for _ in 0..4 {
        tracked_log.step();
        resumed_log.step();
        assert_eq!(tracked_log.save(), resumed_log.save());
        let tick = latest_tick(&tracked_log);
        assert_eq!(
            frame_of(tracked_log.log(), tick, "TrackPing"),
            frame_of(resumed_log.log(), tick, "TrackPing")
        );
    }
}

#[test]
fn the_default_window_is_the_whole_save() {
    let mut world = world_with(TrackPlan {
        emits_a: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    for _ in 0..TRACKER_WINDOW_TICKS + 100 {
        tracker.step();
    }

    let save = TrackingSave::from_bytes(&tracker.snapshot()).unwrap();
    assert_eq!(save.frames().count() as u64, TRACKER_WINDOW_TICKS);
    assert_eq!(save.tick_range(), 100..TRACKER_WINDOW_TICKS + 100);
}

#[test]
fn the_dev_can_choose_the_window() {
    let mut world = world_with(TrackPlan {
        emits_a: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker().with_window(8);
    for _ in 0..20 {
        tracker.step();
    }

    assert_eq!(tracker.log().window_ticks(), 8);
    assert_eq!(ticks_of(&tracker), (12..20).collect::<Vec<u64>>());
    assert_eq!(tracker.log().tick_range(), 12..20);
}

#[test]
#[should_panic(expected = "at least one tick")]
fn an_empty_window_is_rejected() {
    let mut world = World::new();
    let _ = world.to_tracker().with_window(0);
}

#[test]
fn idle_ticks_are_inside_the_range_without_a_frame() {
    let mut world = world_with(reading());
    let mut tracker = world.to_tracker();
    tracker.step();
    tracker.step();
    tracker.signal::<TrackPing>().emit(TrackPing(1));
    tracker.step();

    assert_eq!(ticks_of(&tracker), [2]);
    assert_eq!(tracker.log().tick_range(), 0..3);
}

#[test]
fn pausing_keeps_an_interval_and_resuming_starts_a_new_one() {
    let mut world = world_with(TrackPlan {
        emits_a: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    tracker.step();
    tracker.step();

    tracker.pause();
    assert!(tracker.is_paused());
    assert_eq!(ticks_of(&tracker), Vec::<u64>::new());
    let first = &tracker.log().intervals()[&0];
    assert_eq!((first.start_tick, first.end_tick), (0, 2));
    assert_eq!(first.frames.len(), 2);

    tracker.step();
    assert_eq!(ticks_of(&tracker), Vec::<u64>::new());
    assert_eq!(tracker.get_resource::<Tick>().unwrap().0, 3);

    tracker.resume();
    tracker.step();
    tracker.step();
    assert_eq!(ticks_of(&tracker), [3, 4]);
    assert_eq!(tracker.log().tick_range(), 3..5);

    tracker.pause();
    let keys: Vec<u64> = tracker.log().intervals().keys().copied().collect();
    assert_eq!(keys, [0, 3]);
}

#[test]
fn reset_clears_everything_and_keeps_recording() {
    let mut world = world_with(TrackPlan {
        emits_a: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    tracker.step();
    tracker.pause();
    tracker.resume();
    tracker.step();

    tracker.reset();
    assert!(tracker.log().intervals().is_empty());
    assert_eq!(ticks_of(&tracker), Vec::<u64>::new());
    assert_eq!(tracker.log().tick_range(), 2..2);

    tracker.step();
    assert_eq!(ticks_of(&tracker), [2]);
}

#[test]
fn a_save_restores_frames_intervals_and_names() {
    let mut world = world_with(TrackPlan {
        emits_a: 2,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    tracker.step();
    tracker.pause();
    tracker.resume();
    tracker.step();

    let restored = TrackingSave::from_bytes(&tracker.snapshot()).unwrap();
    assert_eq!(&restored, tracker.log());

    let frame = restored.frames().next().unwrap();
    let signal = &frame.signals[0];
    assert!(restored.signal_name(signal.signal).ends_with("TrackPing"));
    assert_eq!(
        short(restored.system_name(signal.sent[0].sender)),
        "track_sender_a"
    );
}

#[test]
fn a_world_save_is_not_a_tracking_save() {
    let world = world_with(reading());
    assert!(TrackingSave::from_bytes(&world.save()).is_none());
    assert!(TrackingSave::from_bytes(&[1, 2, 3]).is_none());
}

#[test]
fn replaying_from_a_save_overwrites_the_same_ticks() {
    let mut world = world_with(TrackPlan {
        emits_a: 2,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    tracker.step();
    let saved = tracker.save();
    tracker.step();
    tracker.step();
    assert_eq!(ticks_of(&tracker), [0, 1, 2]);

    tracker.load(&saved);
    tracker.step();
    assert_eq!(ticks_of(&tracker), [0, 1]);
    assert_eq!(tracker.log().tick_range(), 0..2);
}

#[test]
fn a_plain_step_splits_the_recording_and_is_not_misattributed() {
    let mut world = world_with(TrackPlan {
        emits_a: 2,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    tracker.step();
    (*tracker).step();
    tracker.step();

    let frame = frame_of(tracker.log(), latest_tick(&tracker), "TrackPing").unwrap();
    assert!(frame.sent.iter().all(|x| x.sender != 0));
    assert_eq!(frame.sent.iter().map(|x| x.count).sum::<u64>(), 2);
    assert_eq!(tracker.log().tick_range(), 2..3);
    assert_eq!(tracker.log().intervals()[&0].end_tick, 1);
}
