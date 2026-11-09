use std::{
    collections::{BTreeMap, VecDeque},
    io::{self, ErrorKind, Read, Write},
    net::{TcpListener, TcpStream},
    ops::Range,
    path::Path,
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
const SNAPSHOT_MAGIC: [u8; 8] = *b"REACSTR2";
const HASH_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const HASH_PRIME: u64 = 0x0000_0100_0000_01b3;
const MAX_MESSAGE_BYTES: usize = 1 << 30;
const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;

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
    pub windowed: bool,
    pub count: u64,
    pub cancelled: u64,
    pub modified: u64,
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
    born: Vec<(u64, u64)>,
    suspects: Vec<u64>,
    states: Vec<Option<SignalState>>,
    old_bytes: Vec<u8>,
    new_bytes: Vec<u8>,
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

    fn row(
        &mut self,
        receiver: u128,
        sender: u128,
        sent_tick: Option<u64>,
        windowed: bool,
    ) -> &mut ReceivedCount {
        let existing = self.received.iter().position(|x| {
            x.receiver == receiver
                && x.sender == sender
                && x.sent_tick == sent_tick
                && x.windowed == windowed
        });
        let index = existing.unwrap_or_else(|| {
            self.received.push(ReceivedCount {
                receiver,
                sender,
                sent_tick,
                windowed,
                count: 0,
                cancelled: 0,
                modified: 0,
            });
            self.received.len() - 1
        });
        &mut self.received[index]
    }

    fn add_received(&mut self, receiver: u128, tally: Tally) {
        for (sender, sent_tick, windowed, count) in tally {
            self.row(receiver, sender, sent_tick, windowed).count += count;
        }
    }
}

struct PayloadHash(u64);

impl Write for PayloadHash {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for byte in bytes {
            self.0 = (self.0 ^ u64::from(*byte)).wrapping_mul(HASH_PRIME);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn payload_hash<S: Serialize>(payload: &S) -> u64 {
    let mut hash = PayloadHash(HASH_OFFSET);
    let _ = bincode::serialize_into(&mut hash, payload);
    hash.0
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SignalState {
    payload: u64,
    cancelled: bool,
    windowed: bool,
    one_tick: bool,
    running: bool,
}

fn sender_of(runs: &[SenderRun], id: u64) -> (u128, Option<u64>) {
    let at = runs.partition_point(|run| run.end <= id);
    let run = runs.get(at).filter(|run| run.start <= id);
    (
        run.map_or(UNKNOWN_SENDER, |run| run.sender),
        run.map(|run| run.tick),
    )
}

fn entry_in<S: Copy>(list: &[WindowEntry<S>], id: u64) -> Option<&WindowEntry<S>> {
    list.binary_search_by_key(&id, |x| x.1.id)
        .ok()
        .map(|i| &list[i])
}

fn locate<S: SignalType>(signals: &Signals<S>, id: u64) -> Option<SignalState> {
    let one_tick_copy = [&signals.transient, &signals.transient_buffer]
        .into_iter()
        .find_map(|list| {
            let i = list.binary_search_by_key(&id, |x| x.id).ok()?;
            Some(payload_hash(&list[i].payload))
        });

    let permanent = |range: Range<usize>| {
        let list = &signals.permanent[range.clone()];
        let i = list.binary_search_by_key(&id, |x| x.id).ok()?;
        Some((range.start + i, &list[i]))
    };
    let found = permanent(0..signals.permanent_write)
        .or_else(|| permanent(signals.permanent_read..signals.permanent.len()));
    if let Some((position, instance)) = found {
        return Some(SignalState {
            payload: payload_hash(&instance.payload),
            cancelled: signals.permanent_cancelled > 0
                && signals.permanent_cancelled_bits.get(position),
            windowed: true,
            one_tick: false,
            running: true,
        });
    }

    let lists = [
        (&signals.transitory_pending[..], false),
        (&signals.permanent_joining[..], true),
    ]
    .into_iter()
    .chain(
        signals
            .transitory_active
            .iter()
            .map(|chunk| (chunk.live(), true)),
    )
    .chain(
        signals
            .transitory_inactive
            .iter()
            .map(|chunk| (chunk.live(), false)),
    );
    for (list, running) in lists {
        if let Some(entry) = entry_in(list, id) {
            return Some(SignalState {
                payload: payload_hash(&entry.1.payload),
                cancelled: entry.0.is_cancelled(),
                windowed: true,
                one_tick: one_tick_copy.is_some(),
                running,
            });
        }
    }

    one_tick_copy.map(|payload| SignalState {
        payload,
        cancelled: false,
        windowed: false,
        one_tick: true,
        running: false,
    })
}

fn same_prefix<T: Serialize>(
    old: &[T],
    new: &[T],
    old_bytes: &mut Vec<u8>,
    new_bytes: &mut Vec<u8>,
) -> bool {
    if old.len() > new.len() {
        return false;
    }
    old_bytes.clear();
    new_bytes.clear();
    let _ = bincode::serialize_into(&mut *old_bytes, old);
    let _ = bincode::serialize_into(&mut *new_bytes, &new[..old.len()]);
    old_bytes == new_bytes
}

fn add_differences<T: Serialize>(
    old: &[T],
    new: &[T],
    id: impl Fn(&T) -> u64,
    live: &mut LiveSignal,
) {
    if same_prefix(old, new, &mut live.old_bytes, &mut live.new_bytes) {
        return;
    }
    let mut at = 0;
    for was in old {
        while new.get(at).is_some_and(|x| id(x) < id(was)) {
            at += 1;
        }
        let same = new.get(at).filter(|x| id(x) == id(was)).is_some_and(|now| {
            live.old_bytes.clear();
            live.new_bytes.clear();
            let _ = bincode::serialize_into(&mut live.old_bytes, was);
            let _ = bincode::serialize_into(&mut live.new_bytes, now);
            live.old_bytes == live.new_bytes
        });
        if !same {
            live.suspects.push(id(was));
        }
    }
}

fn capture_born<S: SignalType>(signals: &Signals<S>, start: u64, live: &mut LiveSignal) {
    let first = live.born.len();
    let buffer = &signals.transient_buffer;
    let new = &buffer[buffer.partition_point(|x| x.id < start)..];
    live.born
        .extend(new.iter().map(|x| (x.id, payload_hash(&x.payload))));
    let pending = &signals.transitory_pending;
    let new = &pending[pending.partition_point(|x| x.1.id < start)..];
    live.born
        .extend(new.iter().map(|x| (x.1.id, payload_hash(&x.1.payload))));
    live.born[first..].sort_unstable_by_key(|x| x.0);
}

fn add_changed_born<S: SignalType>(signals: &Signals<S>, live: &mut LiveSignal) {
    let mut suspects = std::mem::take(&mut live.suspects);
    let (mut in_buffer, mut in_pending) = (0, 0);
    let (buffer, pending) = (&signals.transient_buffer, &signals.transitory_pending);
    for (id, payload) in &live.born {
        while buffer.get(in_buffer).is_some_and(|x| x.id < *id) {
            in_buffer += 1;
        }
        while pending.get(in_pending).is_some_and(|x| x.1.id < *id) {
            in_pending += 1;
        }
        let one_tick = buffer.get(in_buffer).filter(|x| x.id == *id);
        let window = pending.get(in_pending).filter(|x| x.1.id == *id);
        let unchanged = match (window, one_tick) {
            (Some(entry), _) => {
                !entry.0.is_cancelled() && payload_hash(&entry.1.payload) == *payload
            }
            (None, Some(instance)) => payload_hash(&instance.payload) == *payload,
            (None, None) => false,
        };
        if !unchanged {
            suspects.push(*id);
        }
    }
    live.suspects = suspects;
}

fn find_suspects<S: SignalType>(world: &World, copy: &World, live: &mut LiveSignal) -> bool {
    live.suspects.clear();
    let (Some(old), Some(new)) = (
        world.get_resource::<Signals<S>>(),
        copy.get_resource::<Signals<S>>(),
    ) else {
        return false;
    };

    let instance_id = |x: &SignalInstance<S>| x.id;
    let entry_id = |x: &WindowEntry<S>| x.1.id;
    add_differences(&old.transient, &new.transient, instance_id, live);
    add_differences(
        &old.transient_buffer,
        &new.transient_buffer,
        instance_id,
        live,
    );
    add_differences(
        &old.transitory_pending,
        &new.transitory_pending,
        entry_id,
        live,
    );
    add_differences(
        &old.permanent_joining,
        &new.permanent_joining,
        entry_id,
        live,
    );
    for (old_chunks, new_chunks) in [
        (&old.transitory_active, &new.transitory_active),
        (&old.transitory_inactive, &new.transitory_inactive),
    ] {
        for (was, now) in old_chunks.iter().zip(new_chunks) {
            add_differences(was.live(), now.live(), entry_id, live);
        }
    }

    let ranges = [
        0..old.permanent_write,
        old.permanent_read..old.permanent.len(),
    ];
    for range in ranges {
        add_differences(
            &old.permanent[range.clone()],
            &new.permanent[range.clone()],
            instance_id,
            live,
        );
        if old.permanent_cancelled != new.permanent_cancelled {
            let mut position = range.start;
            while position < range.end {
                position = new.permanent_cancelled_bits.next_set(position, range.end);
                if position < range.end && !old.permanent_cancelled_bits.get(position) {
                    live.suspects.push(new.permanent[position].id);
                }
                position += 1;
            }
        }
    }

    add_changed_born(&new, live);
    live.suspects.sort_unstable();
    live.suspects.dedup();
    !live.suspects.is_empty()
}

fn begin_attribution<S: SignalType>(copy: &World, live: &mut LiveSignal) {
    live.states.clear();
    if let Some(signals) = copy.get_resource::<Signals<S>>() {
        let located = live.suspects.iter().map(|id| locate(&signals, *id));
        live.states.extend(located);
    }
}

fn cursor_before<S: SignalType>(copy: &World, system: u128) -> Option<(u64, u64)> {
    let data = copy.get_resource::<SignalData<S>>()?;
    cursor_of(&data, system)
}

fn attribute<S: SignalType>(
    copy: &World,
    system: u128,
    tick: u64,
    before: Option<(u64, u64)>,
    live: &mut LiveSignal,
) {
    let (Some(signals), Some(data)) = (
        copy.get_resource::<Signals<S>>(),
        copy.get_resource::<SignalData<S>>(),
    ) else {
        return;
    };
    let after = cursor_of(&data, system);
    let read_now = after.is_some_and(|x| x.1 == tick) && before.is_none_or(|x| x.1 != tick);
    let counted_before_running = S::Filter::ACTIVE && before.is_some();
    let from = before.map_or(0, |x| x.0);
    let to = after.map_or(0, |x| x.0);

    for index in 0..live.suspects.len() {
        let id = live.suspects[index];
        let now = locate(&signals, id);
        let was = std::mem::replace(&mut live.states[index], now);
        let Some(was) = was.filter(|was| !was.cancelled) else {
            continue;
        };
        let cancelled = now.is_none_or(|now| now.cancelled);
        let modified = now.is_some_and(|now| !now.cancelled && now.payload != was.payload);
        if !cancelled && !modified {
            continue;
        }

        let (sender, sent_tick) = sender_of(&live.runs, id);
        let row = live.row(system, sender, sent_tick, was.windowed);
        if modified {
            row.modified += 1;
            continue;
        }
        row.cancelled += 1;
        let delivered = was.running || (was.one_tick && (from..to).contains(&id));
        if read_now && delivered && !counted_before_running {
            row.count += 1;
        }
    }
}

type Tally = Vec<(u128, Option<u64>, bool, u64)>;

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
        let sent_tick = run.map(|run| run.tick);
        match tally
            .iter_mut()
            .find(|x| x.0 == sender && x.1 == sent_tick && x.2 == windowed)
        {
            Some(existing) => existing.3 += count,
            None => tally.push((sender, sent_tick, windowed, count)),
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
    if data.next > before.next {
        capture_born(&signals, before.next, live);
    }

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
        live.born.clear();
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
    find_suspects: fn(&World, &World, &mut LiveSignal) -> bool,
    begin_attribution: fn(&World, &mut LiveSignal),
    cursor_before: fn(&World, u128) -> Option<(u64, u64)>,
    attribute: fn(&World, u128, u64, Option<(u64, u64)>, &mut LiveSignal),
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
        find_suspects: find_suspects::<S>,
        begin_attribution: begin_attribution::<S>,
        cursor_before: cursor_before::<S>,
        attribute: attribute::<S>,
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
    system_order: Vec<u128>,
    signals: Vec<TrackedName>,
    window_ticks: u64,
    start_tick: u64,
    end_tick: u64,
    frames: VecDeque<TrackedFrame>,
    intervals: BTreeMap<u64, TrackedInterval>,
}

impl TrackingSave {
    pub fn system_order(&self) -> &[u128] {
        &self.system_order
    }

    pub fn window_ticks(&self) -> u64 {
        self.window_ticks
    }

    pub fn tick_range(&self) -> Range<u64> {
        self.start_tick..self.end_tick
    }

    pub fn frames(&self) -> impl Iterator<Item = &TrackedFrame> {
        self.frames.iter()
    }

    pub fn frames_in(&self, ticks: Range<u64>) -> impl Iterator<Item = &TrackedFrame> {
        let start = self
            .frames
            .partition_point(|frame| frame.tick < ticks.start);
        let end = self.frames.partition_point(|frame| frame.tick < ticks.end);
        self.frames.range(start..end)
    }

    pub fn intervals(&self) -> &BTreeMap<u64, TrackedInterval> {
        &self.intervals
    }

    pub fn system_name(&self, id: u128) -> &str {
        name_of(&self.systems, id)
    }

    pub fn signal_ids(&self) -> impl Iterator<Item = u128> + '_ {
        self.signals.iter().map(|name| name.id)
    }

    pub fn signal_name(&self, id: u128) -> &str {
        name_of(&self.signals, id)
    }

    pub fn push_frame(&mut self, tick: u64, frame: Option<TrackedFrame>) {
        let window = self.window_ticks;
        while self
            .frames
            .front()
            .is_some_and(|frame| frame.tick + window <= tick)
        {
            self.frames.pop_front();
        }
        self.frames.extend(frame);
        self.end_tick = tick + 1;
        self.start_tick = self.start_tick.max((tick + 1).saturating_sub(window));
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackerMessage {
    Full(TrackingSave),
    Frame {
        tick: u64,
        frame: Option<TrackedFrame>,
    },
}

impl TrackerMessage {
    fn to_wire(&self) -> Vec<u8> {
        let body = bincode::serialize(self).expect("failed to encode tracker message");
        let mut bytes = (body.len() as u32).to_le_bytes().to_vec();
        bytes.extend(body);
        bytes
    }

    pub fn read_from(reader: &mut impl Read) -> io::Result<Self> {
        let mut length = [0; 4];
        reader.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "tracker message is too long",
            ));
        }

        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
        bincode::deserialize(&body).map_err(|error| io::Error::new(ErrorKind::InvalidData, error))
    }
}

struct TrackerClient {
    stream: TcpStream,
    pending: Vec<u8>,
    written: usize,
}

impl TrackerClient {
    fn flush(&mut self) -> bool {
        while self.written < self.pending.len() {
            match self.stream.write(&self.pending[self.written..]) {
                Ok(0) => return false,
                Ok(count) => self.written += count,
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }

        self.pending.drain(..self.written);
        self.written = 0;
        self.pending.len() <= MAX_PENDING_BYTES
    }
}

struct TrackerNet {
    listener: TcpListener,
    clients: Vec<TrackerClient>,
}

pub struct Tracker {
    log: TrackingSave,
    live: Vec<LiveSignal>,
    paused: bool,
    restart: bool,
    copy: Option<World>,
    net: Option<TrackerNet>,
    changed: bool,
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
        let system_order = system_graph().iter().map(|system| system.uuid).collect();
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
                system_order,
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
            net: None,
            changed: false,
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
        self.changed = true;
        self.rebaseline(world);
    }

    fn freeze(&mut self) {
        self.changed = true;
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
        self.changed = true;
        self.rebaseline(world);
    }

    pub fn step(&mut self, world: &World) {
        if self.paused {
            self.pump(None);
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

        self.attribute_changes(world, tick);
        self.finish_frame(tick);
    }

    fn attribute_changes(&mut self, world: &World, tick: u64) {
        let Some(copy) = self.copy.as_mut() else {
            return;
        };
        let mut touched = Vec::new();
        for (index, (probe, live)) in probes().iter().zip(&mut self.live).enumerate() {
            if (probe.find_suspects)(world, copy, live) {
                touched.push(index);
            }
        }
        if touched.is_empty() {
            return;
        }

        copy_world(world, copy);
        for index in &touched {
            (probes()[*index].begin_attribution)(copy, &mut self.live[*index]);
        }
        for system in system_graph().iter() {
            let cursors: Vec<_> = touched
                .iter()
                .map(|index| (probes()[*index].cursor_before)(copy, system.uuid))
                .collect();
            system.run(copy);
            for (index, before) in touched.iter().zip(cursors) {
                (probes()[*index].attribute)(
                    copy,
                    system.uuid,
                    tick,
                    before,
                    &mut self.live[*index],
                );
            }
        }
    }

    fn finish_frame(&mut self, tick: u64) {
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
        let frame = (!signals.is_empty()).then_some(TrackedFrame { tick, signals });

        let message = self
            .net
            .as_ref()
            .is_some_and(|net| !net.clients.is_empty())
            .then(|| TrackerMessage::Frame {
                tick,
                frame: frame.clone(),
            });
        self.log.push_frame(tick, frame);
        self.pump(message);
    }

    fn pump(&mut self, frame: Option<TrackerMessage>) {
        let Some(net) = &mut self.net else {
            return;
        };

        let update = if std::mem::take(&mut self.changed) {
            (!net.clients.is_empty()).then(|| TrackerMessage::Full(self.log.clone()))
        } else {
            frame
        };
        if let Some(update) = update {
            let bytes = update.to_wire();
            for client in &mut net.clients {
                client.pending.extend_from_slice(&bytes);
            }
        }

        while let Ok((stream, _)) = net.listener.accept() {
            if stream.set_nonblocking(true).is_err() {
                continue;
            }
            let _ = stream.set_nodelay(true);
            net.clients.push(TrackerClient {
                stream,
                pending: TrackerMessage::Full(self.log.clone()).to_wire(),
                written: 0,
            });
        }

        net.clients.retain_mut(TrackerClient::flush);
    }

    pub fn listen(&mut self, port: u16) -> io::Result<u16> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        self.net = Some(TrackerNet {
            listener,
            clients: Vec::new(),
        });
        Ok(port)
    }

    pub fn save_to(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        let mut temporary = path.as_os_str().to_owned();
        temporary.push(".tmp");
        std::fs::write(&temporary, self.snapshot())?;
        std::fs::rename(&temporary, path)
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
        self.changed = true;
    }

    pub fn log(&self) -> &TrackingSave {
        &self.log
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.log.to_bytes()
    }
}
