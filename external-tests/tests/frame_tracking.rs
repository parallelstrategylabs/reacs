use reacs::prelude::*;
use reacs::tracking::{
    TRACKER_WINDOW_TICKS, TrackedSignalFrame, Tracker, TrackerMessage, TrackingSave, UNKNOWN_SENDER,
};
use std::cell::Cell;
use std::net::TcpStream;
use std::rc::Rc;
use std::time::Duration;

#[signal]
struct TrackPing(u64);

#[signal]
struct TrackPong(u64);

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
    opens: u64,
    finite: bool,
    cancel_at: u64,
}

#[resource]
struct HitPlan {
    first: Entity,
    dead: Entity,
    emit_at: u64,
    kill_at: u64,
    modify_at: u64,
    modify_id: u64,
}

#[resource]
struct DiePlan {
    target: Entity,
    at: u64,
}

#[resource(local)]
struct TrackInbox(Rc<Cell<u64>>);

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
        for _ in 0..plan.opens.max(1) {
            if plan.finite {
                pings.send(TrackPing(10), tick..1000);
            } else {
                pings.send(TrackPing(10), ..);
            }
        }
    }
}

#[system(run_after(track_sender_b))]
fn track_late(mut pings: Mut<Signal<TrackPing>>, plan: Resource<TrackPlan>) {
    if plan.reading {
        let _ = pings.receive().count();
        let _ = pings.receive().count();
    }
}

#[system(run_after(track_late))]
fn track_canceller(mut pings: Mut<Signal<TrackPing>>, plan: Resource<TrackPlan>) {
    if plan.cancel_at == pings.tick() + 1 {
        pings.cancel(0);
    }
}

#[signal]
struct Acted(u64);

#[signal]
struct Untouched(u64);

#[system]
fn untouched_sender(mut signals: Mut<Signal<Untouched>>, plan: Resource<ActPlan>) {
    if plan.kind == UNTOUCHED && signals.tick() == 0 {
        signals.send(Untouched(1), ..);
    }
}

#[resource]
struct ActPlan {
    kind: u64,
    action: u64,
    at: u64,
    reads: bool,
    id: u64,
}

const EMIT: u64 = 1;
const WINDOW: u64 = 2;
const OPEN: u64 = 3;
const UNTOUCHED: u64 = 4;
const CANCEL: u64 = 1;
const MODIFY: u64 = 2;

#[system]
fn act_sender(mut signals: Mut<Signal<Acted>>, mut plan: Mut<Resource<ActPlan>>) {
    let tick = signals.tick();
    if tick != 0 {
        return;
    }
    match plan.kind {
        EMIT => plan.id = signals.emit(Acted(1)),
        WINDOW => plan.id = signals.send(Acted(1), tick..tick + 8).unwrap(),
        OPEN => plan.id = signals.send(Acted(1), ..).unwrap(),
        _ => {}
    }
}

#[system(run_after(act_sender))]
fn act_meddler(mut signals: Mut<Signal<Acted>>, plan: Resource<ActPlan>) {
    if plan.reads {
        let _ = signals.receive().count();
    }
    if plan.at != signals.tick() + 1 {
        return;
    }
    match plan.action {
        CANCEL => {
            signals.cancel(plan.id);
        }
        MODIFY => {
            signals.modify(plan.id, Acted(2));
        }
        _ => {}
    }
}

#[system]
fn track_inbox_reader(mut pongs: Mut<Signal<TrackPong>>, inbox: LocalResource<TrackInbox>) {
    for i in 0..(*inbox).0.get() {
        pongs.emit(TrackPong(i));
    }
}

#[system]
fn track_hit_driver(
    mut hits: Mut<Signal<TrackHit>>,
    mut em: Mut<EntityManager>,
    plan: Resource<HitPlan>,
) {
    let next = hits.tick() + 1;
    if plan.emit_at == next {
        hits.emit(TrackHit(plan.first));
        hits.emit(TrackHit(plan.dead));
    }
    if plan.kill_at == next {
        em.despawn(plan.first);
    }
    if plan.modify_at == next {
        hits.modify(plan.modify_id, TrackHit(plan.first));
    }
}

#[system(run_after(track_hit_driver))]
fn track_hit_reader(mut hits: Mut<Signal<TrackHit>>, _em: Mut<EntityManager>) {
    let _ = hits.receive().count();
}

#[system]
fn track_die_driver(mut dies: Mut<Signal<TrackDie>>, plan: Resource<DiePlan>) {
    if plan.at == dies.tick() + 1 {
        dies.emit(TrackDie(plan.target));
    }
}

#[system(run_after(track_die_driver))]
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

fn step(tracker: &mut Tracker, world: &mut World) {
    tracker.step(world);
    world.step();
}

fn latest_tick(tracker: &Tracker) -> u64 {
    tracker.log().tick_range().end - 1
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
        .filter(|x| x.count > 0)
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

type Change = (String, String, Option<u64>, bool, u64, u64, u64);

fn acted_on(tracker: &Tracker, tick: u64) -> Vec<Change> {
    let log = tracker.log();
    let Some(frame) = frame_of(log, tick, "Acted") else {
        return Vec::new();
    };
    frame
        .received
        .iter()
        .filter(|x| x.cancelled + x.modified > 0)
        .map(|x| {
            (
                short(log.system_name(x.receiver)),
                short(log.system_name(x.sender)),
                x.sent_tick,
                x.windowed,
                x.count,
                x.cancelled,
                x.modified,
            )
        })
        .collect()
}

fn run_plan(plan: ActPlan, ticks: u64) -> Tracker {
    let mut world = World::new();
    world.insert_resource(plan);
    let mut tracker = world.to_tracker();
    for _ in 0..ticks {
        step(&mut tracker, &mut world);
    }
    tracker
}

fn change(tick: Option<u64>, windowed: bool, read: u64, cancelled: u64, modified: u64) -> Change {
    (
        "act_meddler".to_string(),
        "act_sender".to_string(),
        tick,
        windowed,
        read,
        cancelled,
        modified,
    )
}

#[test]
fn a_system_that_only_cancels_is_credited_with_the_sender() {
    let tracker = run_plan(
        ActPlan {
            kind: EMIT,
            action: CANCEL,
            at: 1,
            ..Default::default()
        },
        2,
    );
    assert_eq!(acted_on(&tracker, 0), [change(Some(0), false, 0, 1, 0)]);
    assert!(acted_on(&tracker, 1).is_empty());
}

#[test]
fn modifying_a_one_tick_signal_on_the_next_tick_is_recorded() {
    let tracker = run_plan(
        ActPlan {
            kind: EMIT,
            action: MODIFY,
            at: 2,
            ..Default::default()
        },
        3,
    );
    assert_eq!(acted_on(&tracker, 1), [change(Some(0), false, 0, 0, 1)]);
}

#[test]
fn cancelling_a_window_on_its_send_tick_counts_once() {
    let tracker = run_plan(
        ActPlan {
            kind: WINDOW,
            action: CANCEL,
            at: 1,
            ..Default::default()
        },
        2,
    );
    assert_eq!(acted_on(&tracker, 0), [change(Some(0), true, 0, 1, 0)]);
}

#[test]
fn modifying_a_window_on_its_send_tick_counts_once() {
    let tracker = run_plan(
        ActPlan {
            kind: WINDOW,
            action: MODIFY,
            at: 1,
            ..Default::default()
        },
        2,
    );
    assert_eq!(acted_on(&tracker, 0), [change(Some(0), true, 0, 0, 1)]);
}

#[test]
fn a_running_window_can_be_cancelled_and_modified_later() {
    for (action, expected) in [
        (CANCEL, change(Some(0), true, 0, 1, 0)),
        (MODIFY, change(Some(0), true, 0, 0, 1)),
    ] {
        let tracker = run_plan(
            ActPlan {
                kind: WINDOW,
                action,
                at: 4,
                ..Default::default()
            },
            5,
        );
        assert_eq!(acted_on(&tracker, 3), [expected]);
    }
}

#[test]
fn a_permanent_signal_can_be_cancelled_and_modified_later() {
    for (action, expected) in [
        (CANCEL, change(Some(0), true, 0, 1, 0)),
        (MODIFY, change(Some(0), true, 0, 0, 1)),
    ] {
        let tracker = run_plan(
            ActPlan {
                kind: OPEN,
                action,
                at: 6,
                ..Default::default()
            },
            7,
        );
        assert_eq!(acted_on(&tracker, 5), [expected]);
    }
}

#[test]
fn reading_and_cancelling_share_one_record() {
    let tracker = run_plan(
        ActPlan {
            kind: EMIT,
            action: CANCEL,
            at: 1,
            reads: true,
            ..Default::default()
        },
        2,
    );
    assert_eq!(acted_on(&tracker, 0), [change(Some(0), false, 1, 1, 0)]);
}

#[test]
fn reading_and_cancelling_a_running_window_counts_the_read() {
    let tracker = run_plan(
        ActPlan {
            kind: WINDOW,
            action: CANCEL,
            at: 4,
            reads: true,
            ..Default::default()
        },
        5,
    );
    assert_eq!(acted_on(&tracker, 3), [change(Some(0), true, 1, 1, 0)]);
}

#[test]
fn a_signal_nobody_touches_records_no_change() {
    let tracker = run_plan(
        ActPlan {
            kind: UNTOUCHED,
            ..Default::default()
        },
        3,
    );
    assert!(
        tracker
            .log()
            .frames()
            .flat_map(|frame| &frame.signals)
            .flat_map(|signal| &signal.received)
            .all(|x| x.cancelled == 0 && x.modified == 0)
    );
}

#[test]
fn tracking_changes_does_not_affect_the_world() {
    let plan = || ActPlan {
        kind: OPEN,
        action: CANCEL,
        at: 3,
        ..Default::default()
    };
    let mut plain = World::new();
    plain.insert_resource(plan());
    let mut tracked = World::new();
    tracked.insert_resource(plan());
    let mut tracker = tracked.to_tracker();
    for _ in 0..6 {
        plain.step();
        step(&mut tracker, &mut tracked);
    }
    assert_eq!(plain.save(), tracked.save());
}

#[test]
fn counts_each_sender_for_each_receiver_on_the_right_tick() {
    let mut world = world_with(TrackPlan {
        emits_a: 3,
        emits_b: 2,
        ..reading()
    });
    let mut tracker = world.to_tracker();

    step(&mut tracker, &mut world);
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

    step(&mut tracker, &mut world);
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
        step(&mut tracker, &mut world);
        let expected = if tick == 3 || tick == 4 {
            vec![
                row("track_early", "track_sender_b", Some(1), 1),
                row("track_late", "track_sender_b", Some(1), 1),
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
    step(&mut tracker, &mut world);
    assert_eq!(
        received(&tracker, "TrackPing"),
        vec![row("track_late", "track_sender_b", Some(0), 1)]
    );
    for _ in 1..4 {
        step(&mut tracker, &mut world);
        assert_eq!(
            received(&tracker, "TrackPing"),
            vec![
                row("track_early", "track_sender_b", Some(0), 1),
                row("track_late", "track_sender_b", Some(0), 1),
            ]
        );
    }
}

#[test]
fn cancelled_windows_are_not_counted() {
    for finite in [false, true] {
        let mut world = world_with(TrackPlan {
            open_at: 1,
            opens: 2,
            finite,
            cancel_at: 2,
            ..reading()
        });
        let mut tracker = world.to_tracker();
        step(&mut tracker, &mut world);
        step(&mut tracker, &mut world);
        assert_eq!(
            received(&tracker, "TrackPing"),
            vec![
                row("track_early", "track_sender_b", Some(0), 2),
                row("track_late", "track_sender_b", Some(0), 2),
            ]
        );

        step(&mut tracker, &mut world);
        assert_eq!(
            received(&tracker, "TrackPing"),
            vec![
                row("track_early", "track_sender_b", Some(0), 1),
                row("track_late", "track_sender_b", Some(0), 1),
            ]
        );
    }
}

#[test]
fn filtered_counts_follow_deaths_and_payload_changes() {
    let mut world = World::new();
    let (first, second, dead) = {
        let mut em = world.entity_manager();
        let entities = (em.spawn(), em.spawn(), em.spawn());
        em.despawn(entities.2);
        entities
    };
    world.signal::<TrackHit>().send(TrackHit(first), ..);
    let modify_id = world
        .signal::<TrackHit>()
        .send(TrackHit(second), ..)
        .unwrap();
    world.insert_resource(HitPlan {
        first,
        dead,
        emit_at: 2,
        kill_at: 4,
        modify_at: 5,
        modify_id,
    });

    let mut tracker = world.to_tracker();
    step(&mut tracker, &mut world);
    assert_eq!(
        received(&tracker, "TrackHit"),
        vec![row("track_hit_reader", "unknown", None, 2)]
    );

    step(&mut tracker, &mut world);
    assert_eq!(
        received(&tracker, "TrackHit"),
        vec![
            row("track_hit_reader", "track_hit_driver", Some(1), 1),
            row("track_hit_reader", "unknown", None, 2),
        ]
    );

    step(&mut tracker, &mut world);
    step(&mut tracker, &mut world);
    assert_eq!(
        received(&tracker, "TrackHit"),
        vec![row("track_hit_reader", "unknown", None, 1)]
    );

    step(&mut tracker, &mut world);
    assert_eq!(received(&tracker, "TrackHit"), vec![]);
}

#[test]
fn a_signal_whose_entity_dies_in_the_reading_loop_still_counts() {
    let mut world = World::new();
    let target = world.entity_manager().spawn();
    world.insert_resource(DiePlan { target, at: 2 });

    let mut tracker = world.to_tracker();
    step(&mut tracker, &mut world);
    step(&mut tracker, &mut world);
    assert_eq!(
        received(&tracker, "TrackDie"),
        vec![row("track_die_reader", "track_die_driver", Some(1), 1)]
    );

    assert!(!world.entity_manager().is_alive(target));
}

#[test]
fn a_tracked_step_saves_the_same_bytes_as_a_plain_step() {
    let plan = || TrackPlan {
        emits_a: 2,
        window_at: 1,
        open_at: 2,
        ..reading()
    };
    for ticks in 0..8 {
        let mut tracked = world_with(plan());
        let mut plain = world_with(plan());
        let mut tracker = tracked.to_tracker();
        for _ in 0..ticks {
            step(&mut tracker, &mut tracked);
            plain.step();
        }
        assert_eq!(tracked.save(), plain.save(), "after {ticks} ticks");
    }
}

#[test]
fn signals_sent_before_the_tracker_started_have_an_unknown_sender() {
    let mut world = world_with(reading());
    world.signal::<TrackPing>().send(TrackPing(1), ..);
    world.step();
    let mut tracker = world.to_tracker();
    step(&mut tracker, &mut world);

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
    for _ in 0..3 {
        plain.step();
    }

    let mut resumed = world_with(plan());
    resumed.load(&plain.save());

    let mut from_start = tracked.to_tracker();
    for _ in 0..3 {
        step(&mut from_start, &mut tracked);
    }
    let mut from_snapshot = resumed.to_tracker();

    step(&mut from_start, &mut tracked);
    step(&mut from_snapshot, &mut resumed);
    let tick = latest_tick(&from_start);
    let total = |tracker: &Tracker| -> u64 {
        frame_of(tracker.log(), tick, "TrackPing")
            .unwrap()
            .received
            .iter()
            .map(|x| x.count)
            .sum()
    };
    assert_eq!(total(&from_start), total(&from_snapshot));
    assert!(
        frame_of(from_snapshot.log(), tick, "TrackPing")
            .unwrap()
            .received
            .iter()
            .any(|x| x.sender == UNKNOWN_SENDER)
    );

    for _ in 0..4 {
        step(&mut from_start, &mut tracked);
        step(&mut from_snapshot, &mut resumed);
        let tick = latest_tick(&from_start);
        assert_eq!(
            frame_of(from_start.log(), tick, "TrackPing"),
            frame_of(from_snapshot.log(), tick, "TrackPing")
        );
    }
    assert_eq!(tracked.save(), resumed.save());
}

#[test]
fn signals_sent_from_outside_between_steps_are_credited_to_outside() {
    let mut world = World::new();
    let mut tracker = world.to_tracker();
    world.signal::<TrackPong>().emit(TrackPong(1));
    step(&mut tracker, &mut world);

    let frame = frame_of(tracker.log(), 0, "TrackPong").unwrap();
    assert_eq!(
        short(tracker.log().system_name(frame.sent[0].sender)),
        "outside"
    );
    assert_eq!(frame.sent[0].count, 1);
}

#[test]
fn the_tracker_only_reads_the_real_world() {
    let mut world = world_with(TrackPlan {
        emits_a: 2,
        open_at: 1,
        ..reading()
    });
    world.step();
    let before = world.save();
    let mut tracker = world.to_tracker();
    tracker.step(&world);
    assert_eq!(world.save(), before);
    assert_eq!(world.get_resource::<Tick>().unwrap().0, 1);
}

#[test]
fn local_resources_are_not_copied() {
    let inbox = Rc::new(Cell::new(3));
    let mut world = World::new();
    world.insert_local_resource(TrackInbox(inbox));
    let mut tracker = world.to_tracker();
    step(&mut tracker, &mut world);

    assert!(frame_of(tracker.log(), 0, "TrackPong").is_none());
    assert_eq!(
        world
            .get_resource::<reacs::signal::SignalData<TrackPong>>()
            .unwrap()
            .next,
        3
    );
}

#[test]
fn a_plain_step_splits_the_recording_and_is_not_misattributed() {
    let mut world = world_with(TrackPlan {
        emits_a: 2,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    step(&mut tracker, &mut world);
    world.step();
    step(&mut tracker, &mut world);

    let frame = frame_of(tracker.log(), latest_tick(&tracker), "TrackPing").unwrap();
    assert!(frame.sent.iter().all(|x| x.sender != 0));
    assert_eq!(frame.sent.iter().map(|x| x.count).sum::<u64>(), 2);
    assert_eq!(tracker.log().tick_range(), 2..3);
    assert_eq!(tracker.log().intervals()[&0].end_tick, 1);
}

#[test]
fn replaying_from_a_save_overwrites_the_same_ticks() {
    let mut world = world_with(TrackPlan {
        emits_a: 2,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    step(&mut tracker, &mut world);
    let saved = world.save();
    step(&mut tracker, &mut world);
    step(&mut tracker, &mut world);
    assert_eq!(ticks_of(&tracker), [0, 1, 2]);

    world.load(&saved);
    step(&mut tracker, &mut world);
    assert_eq!(ticks_of(&tracker), [0, 1]);
    assert_eq!(tracker.log().tick_range(), 0..2);
}

#[test]
fn the_default_window_is_the_whole_save() {
    let mut world = world_with(TrackPlan {
        emits_a: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    for _ in 0..TRACKER_WINDOW_TICKS + 100 {
        step(&mut tracker, &mut world);
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
        step(&mut tracker, &mut world);
    }

    assert_eq!(tracker.log().window_ticks(), 8);
    assert_eq!(ticks_of(&tracker), (12..20).collect::<Vec<u64>>());
    assert_eq!(tracker.log().tick_range(), 12..20);
}

#[test]
#[should_panic(expected = "at least one tick")]
fn an_empty_window_is_rejected() {
    let world = World::new();
    let _ = world.to_tracker().with_window(0);
}

#[test]
fn idle_ticks_are_inside_the_range_without_a_frame() {
    let mut world = world_with(TrackPlan {
        window_at: 3,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    step(&mut tracker, &mut world);
    step(&mut tracker, &mut world);
    step(&mut tracker, &mut world);

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
    step(&mut tracker, &mut world);
    step(&mut tracker, &mut world);

    tracker.pause();
    assert!(tracker.is_paused());
    assert_eq!(ticks_of(&tracker), Vec::<u64>::new());
    let first = &tracker.log().intervals()[&0];
    assert_eq!((first.start_tick, first.end_tick), (0, 2));
    assert_eq!(first.frames.len(), 2);

    step(&mut tracker, &mut world);
    assert_eq!(ticks_of(&tracker), Vec::<u64>::new());

    tracker.resume();
    step(&mut tracker, &mut world);
    step(&mut tracker, &mut world);
    assert_eq!(ticks_of(&tracker), [3, 4]);
    assert_eq!(tracker.log().tick_range(), 3..5);

    tracker.pause();
    let keys: Vec<u64> = tracker.log().intervals().keys().copied().collect();
    assert_eq!(keys, [0, 3]);
}

#[test]
fn a_tracker_paused_at_creation_records_nothing_until_resumed() {
    let mut world = world_with(TrackPlan {
        emits_a: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    tracker.pause();
    for _ in 0..3 {
        step(&mut tracker, &mut world);
    }
    assert_eq!(ticks_of(&tracker), Vec::<u64>::new());
    assert!(tracker.log().intervals().is_empty());

    tracker.resume();
    step(&mut tracker, &mut world);
    assert_eq!(ticks_of(&tracker), [3]);
    assert_eq!(tracker.log().tick_range(), 3..4);
}

#[test]
fn reset_clears_everything_and_keeps_recording() {
    let mut world = world_with(TrackPlan {
        emits_a: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    step(&mut tracker, &mut world);
    tracker.pause();
    tracker.resume();
    step(&mut tracker, &mut world);

    tracker.reset();
    assert!(tracker.log().intervals().is_empty());
    assert_eq!(ticks_of(&tracker), Vec::<u64>::new());

    step(&mut tracker, &mut world);
    assert_eq!(ticks_of(&tracker), [2]);
    assert_eq!(tracker.log().tick_range(), 2..3);
}

#[test]
fn a_save_restores_frames_intervals_and_names() {
    let mut world = world_with(TrackPlan {
        emits_a: 2,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    step(&mut tracker, &mut world);
    tracker.pause();
    tracker.resume();
    step(&mut tracker, &mut world);

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

fn connect(port: u16) -> TcpStream {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
}

fn read_into(stream: &mut TcpStream, save: &mut Option<TrackingSave>) {
    match TrackerMessage::read_from(stream).unwrap() {
        TrackerMessage::Full(full) => *save = Some(full),
        TrackerMessage::Frame { tick, frame } => save.as_mut().unwrap().push_frame(tick, frame),
    }
}

#[test]
fn a_late_client_gets_the_whole_window_and_then_each_tick() {
    let mut world = world_with(TrackPlan {
        emits_a: 2,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    let port = tracker.listen(0).unwrap();
    for _ in 0..5 {
        step(&mut tracker, &mut world);
    }

    let mut stream = connect(port);
    step(&mut tracker, &mut world);
    let mut save = None;
    read_into(&mut stream, &mut save);
    assert_eq!(save.as_ref().unwrap(), tracker.log());

    for _ in 0..3 {
        step(&mut tracker, &mut world);
        read_into(&mut stream, &mut save);
    }
    assert_eq!(save.as_ref().unwrap(), tracker.log());
    assert_eq!(save.as_ref().unwrap().tick_range(), 0..9);
}

#[test]
fn idle_ticks_still_reach_the_client() {
    let mut world = world_with(reading());
    let mut tracker = world.to_tracker();
    let port = tracker.listen(0).unwrap();
    let mut stream = connect(port);
    step(&mut tracker, &mut world);
    let mut save = None;
    read_into(&mut stream, &mut save);

    step(&mut tracker, &mut world);
    read_into(&mut stream, &mut save);
    assert_eq!(save.as_ref().unwrap().tick_range(), 0..2);
    assert_eq!(save.as_ref().unwrap().frames().count(), 0);
}

#[test]
fn pausing_and_resetting_send_the_new_state() {
    let mut world = world_with(TrackPlan {
        emits_a: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    let port = tracker.listen(0).unwrap();
    let mut stream = connect(port);
    step(&mut tracker, &mut world);
    let mut save = None;
    read_into(&mut stream, &mut save);

    tracker.pause();
    step(&mut tracker, &mut world);
    read_into(&mut stream, &mut save);
    assert_eq!(save.as_ref().unwrap(), tracker.log());
    assert_eq!(save.as_ref().unwrap().intervals().len(), 1);

    tracker.reset();
    step(&mut tracker, &mut world);
    read_into(&mut stream, &mut save);
    assert!(save.as_ref().unwrap().intervals().is_empty());
}

#[test]
fn a_paused_tracker_still_accepts_clients() {
    let mut world = world_with(reading());
    let mut tracker = world.to_tracker();
    let port = tracker.listen(0).unwrap();
    tracker.pause();

    let mut stream = connect(port);
    step(&mut tracker, &mut world);
    let mut save = None;
    read_into(&mut stream, &mut save);
    assert_eq!(save.as_ref().unwrap(), tracker.log());
}

#[test]
fn a_client_that_leaves_or_never_reads_does_not_stop_the_game() {
    let mut world = world_with(TrackPlan {
        emits_a: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    let port = tracker.listen(0).unwrap();
    let gone = connect(port);
    let _never_reads = connect(port);
    step(&mut tracker, &mut world);
    drop(gone);

    for _ in 0..30_000 {
        tracker.step(&world);
        world.step();
    }
    assert_eq!(world.get_resource::<Tick>().unwrap().0, 30_001);
}

#[test]
fn closing_the_tracker_ends_the_connection() {
    let mut world = world_with(reading());
    let mut tracker = world.to_tracker();
    let port = tracker.listen(0).unwrap();
    let mut stream = connect(port);
    step(&mut tracker, &mut world);
    let mut save = None;
    read_into(&mut stream, &mut save);

    drop(tracker);
    assert!(TrackerMessage::read_from(&mut stream).is_err());
}

#[test]
fn save_to_writes_a_readable_file_and_leaves_no_temporary() {
    let mut world = world_with(TrackPlan {
        emits_a: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    step(&mut tracker, &mut world);

    let path = std::env::temp_dir().join(format!("reacs-tracking-{}.bin", std::process::id()));
    tracker.save_to(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&TrackingSave::from_bytes(&bytes).unwrap(), tracker.log());

    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    assert!(!std::path::Path::new(&temporary).exists());
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn the_save_records_the_system_execution_order() {
    let world = world_with(reading());
    let tracker = world.to_tracker();
    let log = tracker.log();
    let position = |name: &str| {
        log.system_order()
            .iter()
            .position(|id| short(log.system_name(*id)) == name)
            .unwrap()
    };

    assert!(position("track_early") < position("track_sender_a"));
    assert!(position("track_sender_a") < position("track_sender_b"));
    assert!(position("track_sender_b") < position("track_late"));
}

#[test]
fn windowed_deliveries_are_marked_and_keep_their_send_tick() {
    let mut world = world_with(TrackPlan {
        open_at: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    step(&mut tracker, &mut world);
    step(&mut tracker, &mut world);

    let copy = frame_of(tracker.log(), 0, "TrackPing").unwrap();
    assert!(copy.received.iter().all(|x| !x.windowed));
    let running = frame_of(tracker.log(), 1, "TrackPing").unwrap();
    assert!(!running.received.is_empty());
    assert!(
        running
            .received
            .iter()
            .all(|x| x.windowed && x.sent_tick == Some(0))
    );
}

#[test]
fn frames_in_returns_only_the_requested_ticks() {
    let mut world = world_with(TrackPlan {
        emits_a: 1,
        ..reading()
    });
    let mut tracker = world.to_tracker();
    for _ in 0..6 {
        step(&mut tracker, &mut world);
    }

    let ticks: Vec<u64> = tracker.log().frames_in(2..4).map(|x| x.tick).collect();
    assert_eq!(ticks, [2, 3]);
    assert_eq!(tracker.log().frames_in(9..12).count(), 0);
}
