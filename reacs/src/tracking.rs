use std::{
    collections::{BTreeMap, VecDeque},
    ops::Range,
    ptr::addr_of_mut,
};

use serde::{Deserialize, Serialize};

use crate::{
    Tick,
    registry::get_registry_entry,
    schedule::{ANONYMOUS_SYSTEM, system_graph},
    signal::{SignalData, SignalFilter, SignalInstance, SignalType, Signals, WindowEntry},
    world::World,
};

pub const UNKNOWN_SENDER: u128 = u128::MAX;
pub const TRACKER_WINDOW_TICKS: u64 = 4096;
const MIN_TRIM_RUNS: usize = 64;
const SNAPSHOT_MAGIC: [u8; 8] = *b"REACSTRK";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentCount {
    pub sender: u128,
    pub count: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceivedCount {
    pub receiver: u128,
    pub sender: u128,
    pub sent_tick: Option<u64>,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedName {
    pub id: u128,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedSignalFrame {
    pub signal: u128,
    pub sent: Vec<SentCount>,
    pub received: Vec<ReceivedCount>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedFrame {
    pub tick: u64,
    pub signals: Vec<TrackedSignalFrame>,
}

#[derive(Clone, Copy, Debug)]
struct SenderRun {
    start: u64,
    end: u64,
    sender: u128,
    tick: u64,
    keep_until: u64,
}

#[derive(Clone, Debug, Default)]
struct LiveSignal {
    synced: u64,
    trim_at: usize,
    runs: Vec<SenderRun>,
    sent: Vec<SentCount>,
    received: Vec<ReceivedCount>,
}

impl LiveSignal {
    fn add_run(&mut self, run: SenderRun) {
        let merges = self.runs.last_mut().filter(|last| {
            last.end == run.start && last.sender == run.sender && last.tick == run.tick
        });
        if let Some(last) = merges {
            last.end = run.end;
            last.keep_until = last.keep_until.max(run.keep_until);
            return;
        }

        self.runs.push(run);
        if self.runs.len() >= self.trim_at.max(MIN_TRIM_RUNS) {
            self.runs.retain(|kept| kept.keep_until > run.tick);
            self.trim_at = self.runs.len() * 2;
        }
    }

    fn add_received(&mut self, receiver: u128, tally: Tally) {
        for (sender, sent_tick, count) in tally {
            let existing = self
                .received
                .iter_mut()
                .find(|x| x.receiver == receiver && x.sender == sender && x.sent_tick == sent_tick);
            match existing {
                Some(existing) => existing.count += count,
                None => self.received.push(ReceivedCount {
                    receiver,
                    sender,
                    sent_tick,
                    count,
                }),
            }
        }
    }
}

type Tally = Vec<(u128, Option<u64>, u64)>;

fn record_sent<S: SignalType>(
    signals: &Signals<S>,
    next: u64,
    sender: u128,
    tick: u64,
    live: &mut LiveSignal,
) {
    let start = live.synced;
    if start == next {
        return;
    }
    live.synced = next;

    let pending = &signals.transitory_pending;
    let keep_until = pending[pending.partition_point(|entry| entry.1.id < start)..]
        .iter()
        .fold(tick + 2, |latest, entry| latest.max(entry.0.end));
    live.add_run(SenderRun {
        start,
        end: next,
        sender,
        tick,
        keep_until,
    });

    let count = next - start;
    match live.sent.iter_mut().find(|x| x.sender == sender) {
        Some(existing) => existing.count += count,
        None => live.sent.push(SentCount { sender, count }),
    }
}

fn tally_sorted<T>(
    items: &[T],
    id: impl Fn(&T) -> u64,
    scan: bool,
    delivered: impl Fn(&T) -> bool,
    runs: &[SenderRun],
    windowed: bool,
    tally: &mut Tally,
) {
    let mut i = 0;
    while i < items.len() {
        let first = id(&items[i]);
        let at = runs.partition_point(|run| run.end <= first);
        let run = runs.get(at).filter(|run| run.start <= first);
        let limit = match run {
            Some(run) => run.end,
            None => runs.get(at).map_or(u64::MAX, |next| next.start),
        };

        let stop = i + items[i..].partition_point(|x| id(x) < limit);
        let count = if scan {
            items[i..stop].iter().filter(|x| delivered(x)).count() as u64
        } else {
            (stop - i) as u64
        };
        i = stop;
        if count == 0 {
            continue;
        }

        let sender = run.map_or(UNKNOWN_SENDER, |run| run.sender);
        let sent_tick = if windowed {
            None
        } else {
            run.map(|run| run.tick)
        };
        match tally.iter_mut().find(|x| x.0 == sender && x.1 == sent_tick) {
            Some(existing) => existing.2 += count,
            None => tally.push((sender, sent_tick, count)),
        }
    }
}

fn tally_one_tick<S: SignalType>(
    signals: &Signals<S>,
    low: u64,
    high: u64,
    runs: &[SenderRun],
    context: &<S::Filter as SignalFilter<S>>::Context<'_>,
    tally: &mut Tally,
) {
    if low >= high {
        return;
    }
    let instance = |x: &SignalInstance<S>| S::Filter::keep(context, &x.payload);
    for list in [&signals.transient, &signals.transient_buffer] {
        let unread =
            &list[list.partition_point(|x| x.id < low)..list.partition_point(|x| x.id < high)];
        tally_sorted(
            unread,
            |x| x.id,
            S::Filter::ACTIVE,
            instance,
            runs,
            false,
            tally,
        );
    }
}

fn tally_windows<S: SignalType>(
    signals: &Signals<S>,
    runs: &[SenderRun],
    context: &<S::Filter as SignalFilter<S>>::Context<'_>,
    tally: &mut Tally,
) {
    let filtered = S::Filter::ACTIVE;
    let instance = |x: &SignalInstance<S>| S::Filter::keep(context, &x.payload);
    let entry = |x: &WindowEntry<S>| !x.0.is_cancelled() && S::Filter::keep(context, &x.1.payload);

    for chunk in &signals.transitory_active {
        let scan = filtered || chunk.cancelled > 0;
        tally_sorted(chunk.live(), |x| x.1.id, scan, entry, runs, true, tally);
    }
    tally_sorted(
        &signals.permanent_joining,
        |x| x.1.id,
        true,
        entry,
        runs,
        true,
        tally,
    );

    let permanent = &signals.permanent;
    let ranges = [
        (0, signals.permanent_write),
        (signals.permanent_read, permanent.len()),
    ];
    for (mut position, limit) in ranges {
        while position < limit {
            let next = if signals.permanent_cancelled > 0 {
                signals.permanent_cancelled_bits.next_set(position, limit)
            } else {
                limit
            };
            let run = &permanent[position..next];
            tally_sorted(run, |x| x.id, filtered, instance, runs, true, tally);
            position = next + 1;
        }
    }
}

struct Before {
    next: u64,
    cursor: Option<(u64, u64)>,
    tally: Option<Tally>,
}

fn cursor_of<S: SignalType>(data: &SignalData<S>, system: u128) -> Option<(u64, u64)> {
    let i = data
        .cursors
        .binary_search_by_key(&system, |x| x.system)
        .ok()?;
    Some((data.cursors[i].next, data.cursors[i].windows_tick))
}

fn before_system<S: SignalType>(world: &World, system: u128, live: &LiveSignal) -> Option<Before> {
    let data = world.get_resource::<SignalData<S>>()?;
    let cursor = cursor_of(&data, system);
    let tally = match cursor {
        Some((next, _)) if S::Filter::ACTIVE => {
            let signals = world.get_resource::<Signals<S>>()?;
            let context = S::Filter::fetch(world);
            let mut tally = Tally::new();
            tally_one_tick(&signals, next, data.next, &live.runs, &context, &mut tally);
            tally_windows(&signals, &live.runs, &context, &mut tally);
            Some(tally)
        }
        _ => None,
    };

    Some(Before {
        next: data.next,
        cursor,
        tally,
    })
}

fn after_system<S: SignalType>(
    world: &World,
    system: u128,
    tick: u64,
    before: Before,
    live: &mut LiveSignal,
) {
    let (Some(signals), Some(data)) = (
        world.get_resource::<Signals<S>>(),
        world.get_resource::<SignalData<S>>(),
    ) else {
        return;
    };

    record_sent(&signals, data.next, system, tick, live);

    let Some((to, windows_tick)) = cursor_of(&data, system) else {
        return;
    };
    if windows_tick != tick || before.cursor.is_some_and(|x| x.1 == tick) {
        return;
    }

    let context = S::Filter::fetch(world);
    let tally = match before.tally {
        Some(mut tally) => {
            tally_one_tick(&signals, before.next, to, &live.runs, &context, &mut tally);
            tally
        }
        None => {
            let from = before.cursor.map_or(0, |x| x.0);
            let mut tally = Tally::new();
            tally_one_tick(&signals, from, to, &live.runs, &context, &mut tally);
            tally_windows(&signals, &live.runs, &context, &mut tally);
            tally
        }
    };
    live.add_received(system, tally);
}

fn record_outside<S: SignalType>(world: &World, tick: u64, live: &mut LiveSignal) {
    if let (Some(signals), Some(data)) = (
        world.get_resource::<Signals<S>>(),
        world.get_resource::<SignalData<S>>(),
    ) {
        if data.next < live.synced {
            *live = LiveSignal {
                synced: data.next,
                ..Default::default()
            };
            return;
        }
        record_sent(&signals, data.next, ANONYMOUS_SYSTEM, tick, live);
    }
}

fn baseline<S: SignalType>(world: &World) -> u64 {
    world
        .get_resource::<SignalData<S>>()
        .map_or(0, |data| data.next)
}

struct SignalProbe {
    id: u128,
    name: &'static str,
    before: fn(&World, u128, &LiveSignal) -> Option<Before>,
    after: fn(&World, u128, u64, Before, &mut LiveSignal),
    outside: fn(&World, u64, &mut LiveSignal),
    baseline: fn(&World) -> u64,
}

static mut SIGNAL_PROBES: Option<Vec<SignalProbe>> = None;

pub(crate) fn register_signal_probe<S: SignalType>() {
    let probe = SignalProbe {
        id: S::TYPE_UUID,
        name: std::any::type_name::<S>(),
        before: before_system::<S>,
        after: after_system::<S>,
        outside: record_outside::<S>,
        baseline: baseline::<S>,
    };
    unsafe {
        let ptr = addr_of_mut!(SIGNAL_PROBES);
        let probes = (*ptr).get_or_insert_with(Vec::new);
        let i = probes.partition_point(|x| x.id < probe.id);
        probes.insert(i, probe);
    }
}

fn probes() -> &'static [SignalProbe] {
    unsafe {
        let ptr = addr_of_mut!(SIGNAL_PROBES);
        (*ptr).as_deref().unwrap_or(&[])
    }
}

fn name_of(names: &[TrackedName], id: u128) -> &str {
    names
        .binary_search_by_key(&id, |x| x.id)
        .map_or("unknown", |i| &names[i].name)
}

fn current_tick(world: &World) -> u64 {
    world.get_resource::<Tick>().map_or(0, |tick| tick.0)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackedInterval {
    pub start_tick: u64,
    pub end_tick: u64,
    pub frames: Vec<TrackedFrame>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackingSave {
    systems: Vec<TrackedName>,
    signals: Vec<TrackedName>,
    window_ticks: u64,
    start_tick: u64,
    end_tick: u64,
    frames: VecDeque<TrackedFrame>,
    intervals: BTreeMap<u64, TrackedInterval>,
}

impl TrackingSave {
    pub fn window_ticks(&self) -> u64 {
        self.window_ticks
    }

    pub fn tick_range(&self) -> Range<u64> {
        self.start_tick..self.end_tick
    }

    pub fn frames(&self) -> impl Iterator<Item = &TrackedFrame> {
        self.frames.iter()
    }

    pub fn intervals(&self) -> &BTreeMap<u64, TrackedInterval> {
        &self.intervals
    }

    pub fn system_name(&self, id: u128) -> &str {
        name_of(&self.systems, id)
    }

    pub fn signal_name(&self, id: u128) -> &str {
        name_of(&self.signals, id)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = SNAPSHOT_MAGIC.to_vec();
        bincode::serialize_into(&mut bytes, self).expect("failed to encode tracking save");
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        bincode::deserialize(bytes.strip_prefix(&SNAPSHOT_MAGIC)?).ok()
    }
}

pub struct Tracker {
    log: TrackingSave,
    live: Vec<LiveSignal>,
    paused: bool,
    restart: bool,
    copy: Option<World>,
}

fn copy_world(from: &World, to: &mut World) {
    to.resources = from
        .resources
        .iter()
        .map(|(id, cell)| {
            let entry = get_registry_entry(*id).unwrap_or_else(|| {
                panic!(
                    "Tracker can't copy resource {id:#034x}: it isn't registered, use #[resource]"
                )
            });
            let resource = cell.try_borrow().unwrap_or_else(|_| {
                panic!(
                    "Tracker can't copy resource `{}` while it's borrowed for writing",
                    entry.type_name
                )
            });
            (*id, (entry.restore)(&(entry.serialize)(&*resource)))
        })
        .collect();
}

impl World {
    pub fn to_tracker(&self) -> Tracker {
        let mut systems: Vec<TrackedName> = system_graph()
            .iter()
            .map(|system| TrackedName {
                id: system.uuid,
                name: format!("{}::{}", system.module, system.name),
            })
            .collect();
        systems.push(TrackedName {
            id: ANONYMOUS_SYSTEM,
            name: "outside".to_string(),
        });
        systems.sort_unstable_by_key(|system| system.id);

        let signals = probes()
            .iter()
            .map(|probe| TrackedName {
                id: probe.id,
                name: probe.name.to_string(),
            })
            .collect();

        let mut tracker = Tracker {
            log: TrackingSave {
                systems,
                signals,
                window_ticks: TRACKER_WINDOW_TICKS,
                start_tick: 0,
                end_tick: 0,
                frames: VecDeque::new(),
                intervals: BTreeMap::new(),
            },
            live: Vec::new(),
            paused: false,
            restart: false,
            copy: None,
        };
        tracker.begin(self);
        tracker
    }
}

impl Tracker {
    pub fn with_window(mut self, ticks: u64) -> Self {
        assert!(ticks > 0, "the tracker window needs at least one tick");
        self.log.window_ticks = ticks;
        self
    }

    fn rebaseline(&mut self, world: &World) {
        self.live = probes()
            .iter()
            .map(|probe| LiveSignal {
                synced: (probe.baseline)(world),
                ..Default::default()
            })
            .collect();
    }

    fn begin(&mut self, world: &World) {
        let tick = current_tick(world);
        self.log.start_tick = tick;
        self.log.end_tick = tick;
        self.restart = false;
        self.rebaseline(world);
    }

    fn freeze(&mut self) {
        let (start_tick, end_tick) = (self.log.start_tick, self.log.end_tick);
        let frames: Vec<TrackedFrame> = self.log.frames.drain(..).collect();
        if end_tick > start_tick {
            self.log.intervals.insert(
                start_tick,
                TrackedInterval {
                    start_tick,
                    end_tick,
                    frames,
                },
            );
        }
    }

    fn realign(&mut self, world: &World, tick: u64) {
        if tick > self.log.end_tick {
            self.freeze();
            self.begin(world);
            return;
        }

        while self
            .log
            .frames
            .back()
            .is_some_and(|frame| frame.tick >= tick)
        {
            self.log.frames.pop_back();
        }
        self.log.end_tick = tick;
        self.log.start_tick = self.log.start_tick.min(tick);
        self.rebaseline(world);
    }

    pub fn step(&mut self, world: &World) {
        if self.paused {
            return;
        }

        let tick = current_tick(world);
        if self.restart {
            self.begin(world);
        } else if tick != self.log.end_tick {
            self.realign(world, tick);
        }

        let copy = self.copy.get_or_insert_with(World::new);
        copy_world(world, copy);
        for (probe, live) in probes().iter().zip(&mut self.live) {
            (probe.outside)(copy, tick, live);
        }

        let mut before = Vec::with_capacity(probes().len());
        for system in system_graph().iter() {
            before.extend(
                probes()
                    .iter()
                    .zip(&self.live)
                    .map(|(probe, live)| (probe.before)(copy, system.uuid, live)),
            );
            system.run(copy);
            for ((probe, live), before) in probes().iter().zip(&mut self.live).zip(before.drain(..))
            {
                if let Some(before) = before {
                    (probe.after)(copy, system.uuid, tick, before, live);
                }
            }
        }

        self.finish_frame(tick);
    }

    fn finish_frame(&mut self, tick: u64) {
        let window = self.log.window_ticks;
        while self
            .log
            .frames
            .front()
            .is_some_and(|frame| frame.tick + window <= tick)
        {
            self.log.frames.pop_front();
        }

        let signals: Vec<TrackedSignalFrame> = probes()
            .iter()
            .zip(&mut self.live)
            .filter(|(_, live)| !live.sent.is_empty() || !live.received.is_empty())
            .map(|(probe, live)| TrackedSignalFrame {
                signal: probe.id,
                sent: std::mem::take(&mut live.sent),
                received: std::mem::take(&mut live.received),
            })
            .collect();
        if !signals.is_empty() {
            self.log.frames.push_back(TrackedFrame { tick, signals });
        }

        self.log.end_tick = tick + 1;
        self.log.start_tick = self.log.start_tick.max((tick + 1).saturating_sub(window));
    }

    pub fn pause(&mut self) {
        if !self.paused {
            self.paused = true;
            self.freeze();
        }
    }

    pub fn resume(&mut self) {
        if self.paused {
            self.paused = false;
            self.restart = true;
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn reset(&mut self) {
        self.log.frames.clear();
        self.log.intervals.clear();
        self.restart = true;
    }

    pub fn log(&self) -> &TrackingSave {
        &self.log
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.log.to_bytes()
    }
}
