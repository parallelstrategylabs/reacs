use std::{
    fmt,
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, ErrorKind, Read, Write},
    ops::Range,
    path::{Path, PathBuf},
    ptr::addr_of_mut,
    rc::Rc,
};

use bincode::Options;
use serde::{Deserialize, Serialize};

use crate::{
    Tick,
    hashed::Hashed,
    registry::{get_registry_entry, registered_ids},
    schedule::{Mut, system_graph},
    signal::{InputSignal, Signal, SignalData, SignalType, Signals},
    snapshot::{SNAPSHOT_BUDGET_BYTES, SNAPSHOT_EVERY, SnapshotStore},
    state_hash::hash_bytes,
    world::World,
};

const MAGIC: &[u8; 8] = b"REACSRP1";
pub const REPLAY_HASH_EVERY: u64 = 64;

fn options() -> bincode::DefaultOptions {
    bincode::DefaultOptions::new()
}

struct InputEntry {
    uuid: u128,
    init: fn(&mut World),
    next_id: fn(&World) -> u64,
    capture: fn(&World, u64, &mut Vec<Vec<u8>>),
    resend: fn(&mut World, &[u8]) -> bool,
}

static mut INPUT_REGISTRY: Option<Vec<InputEntry>> = None;

fn init_input<S: SignalType>(world: &mut World) {
    world.init_signal::<S>();
}

fn next_input_id<S: SignalType>(world: &World) -> u64 {
    world
        .get_resource::<SignalData<S>>()
        .map_or(0, |data| data.next)
}

fn capture_input<S: SignalType>(world: &World, first_id: u64, out: &mut Vec<Vec<u8>>) {
    let Some(signals) = world.get_resource::<Signals<S>>() else {
        return;
    };
    if signals.transient_buffer.is_empty() {
        return;
    }
    out.extend(
        signals
            .transient_buffer
            .iter()
            .filter(|instance| instance.id >= first_id)
            .map(|instance| {
                options()
                    .serialize(&instance.payload)
                    .expect("failed to encode an input signal")
            }),
    );
}

fn resend_input<S: SignalType>(world: &mut World, bytes: &[u8]) -> bool {
    let Ok(signal) = options().deserialize::<S>(bytes) else {
        return false;
    };
    world.fetch_param::<Mut<Signal<S>>>().emit(signal);
    true
}

pub fn register_input_signal<S: InputSignal>() {
    let uuid = Signals::<S>::TYPE_UUID;
    unsafe {
        let ptr = addr_of_mut!(INPUT_REGISTRY);
        let inputs = (*ptr).get_or_insert_with(Vec::new);
        let i = inputs.partition_point(|x| x.uuid < uuid);
        inputs.insert(
            i,
            InputEntry {
                uuid,
                init: init_input::<S>,
                next_id: next_input_id::<S>,
                capture: capture_input::<S>,
                resend: resend_input::<S>,
            },
        );
    }
}

fn input_signals() -> &'static [InputEntry] {
    unsafe {
        let ptr = addr_of_mut!(INPUT_REGISTRY);
        (*ptr).as_deref().unwrap_or(&[])
    }
}

pub(crate) fn init_input_signals(world: &mut World) {
    for input in input_signals() {
        (input.init)(world);
    }
}

fn build_id() -> u64 {
    let mut registered = registered_ids();
    registered.sort_unstable();

    let mut bytes = Vec::new();
    let systems = system_graph().iter().map(|system| system.uuid);
    let inputs = input_signals().iter().map(|input| input.uuid);
    for id in registered.into_iter().chain(systems).chain(inputs) {
        bytes.extend_from_slice(&id.to_le_bytes());
    }
    hash_bytes(&bytes)
}

fn current_tick(world: &World) -> u64 {
    world.get_resource::<Tick>().map_or(0, |tick| tick.0)
}

fn encode_error(error: bincode::Error) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, error)
}

#[derive(Serialize, Deserialize)]
struct Header {
    build_id: u64,
    start_tick: u64,
    inputs: Vec<u128>,
    state_len: u64,
}

#[derive(Serialize, Deserialize)]
struct InputBatch {
    kind: u32,
    payloads: Vec<Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
struct HashRecord {
    table: Option<Vec<u128>>,
    values: Vec<u64>,
}

#[derive(Serialize, Deserialize)]
enum Record {
    Tick {
        gap: u64,
        hash: Option<HashRecord>,
        inputs: Vec<InputBatch>,
    },
    Section {
        tick: u64,
        state_len: u64,
    },
}

pub struct Recorder {
    out: BufWriter<File>,
    hash_every: u64,
    first_ids: Vec<u64>,
    last: Option<u64>,
    written: u64,
    scratch: Vec<Vec<u8>>,
    table: Vec<u128>,
    error: Option<io::Error>,
}

impl Recorder {
    pub fn with_hash_every(mut self, ticks: u64) -> Self {
        assert!(ticks > 0, "hash_every must be at least 1");
        self.hash_every = ticks;
        self
    }

    pub fn record(&mut self, world: &World) {
        if self.error.is_some() {
            return;
        }
        if let Err(error) = self.write_tick(world) {
            self.error = Some(error);
        }
    }

    pub fn finish(mut self) -> io::Result<()> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        self.out.flush()
    }

    fn write_tick(&mut self, world: &World) -> io::Result<()> {
        let tick = current_tick(world);
        if self.last == Some(tick) {
            return Ok(());
        }
        if tick != self.last.map_or(self.written, |last| last + 1) {
            self.write_section(world, tick)?;
        }
        self.last = Some(tick);

        let mut inputs = Vec::new();
        for (kind, input) in input_signals().iter().enumerate() {
            (input.capture)(world, self.first_ids[kind], &mut self.scratch);
            if !self.scratch.is_empty() {
                inputs.push(InputBatch {
                    kind: kind as u32,
                    payloads: std::mem::take(&mut self.scratch),
                });
            }
        }

        let hash = (tick % self.hash_every == 0).then(|| self.hash_record(world));
        if inputs.is_empty() && hash.is_none() {
            return Ok(());
        }

        let gap = tick - self.written;
        self.written = tick;
        options()
            .serialize_into(&mut self.out, &Record::Tick { gap, hash, inputs })
            .map_err(encode_error)?;
        self.out.flush()
    }

    fn hash_record(&mut self, world: &World) -> HashRecord {
        let hashes = world.resource_hashes();
        let same_table = hashes
            .iter()
            .map(|hash| hash.0)
            .eq(self.table.iter().copied());
        let table = (!same_table).then(|| {
            self.table = hashes.iter().map(|hash| hash.0).collect();
            self.table.clone()
        });
        HashRecord {
            table,
            values: hashes.iter().map(|hash| hash.1).collect(),
        }
    }

    fn write_section(&mut self, world: &World, tick: u64) -> io::Result<()> {
        let state = world.save();
        let section = Record::Section {
            tick,
            state_len: state.len() as u64,
        };
        options()
            .serialize_into(&mut self.out, &section)
            .map_err(encode_error)?;
        self.out.write_all(&state)?;
        self.written = tick;
        self.first_ids = next_ids(world);
        self.out.flush()
    }
}

fn next_ids(world: &World) -> Vec<u64> {
    input_signals()
        .iter()
        .map(|input| (input.next_id)(world))
        .collect()
}

impl World {
    pub fn to_recorder(&self, path: impl AsRef<Path>) -> io::Result<Recorder> {
        let mut out = BufWriter::new(File::create(path)?);
        let state = self.save();
        let start_tick = current_tick(self);
        let header = Header {
            build_id: build_id(),
            start_tick,
            inputs: input_signals().iter().map(|input| input.uuid).collect(),
            state_len: state.len() as u64,
        };

        out.write_all(MAGIC)?;
        options()
            .serialize_into(&mut out, &header)
            .map_err(encode_error)?;
        out.write_all(&state)?;
        out.flush()?;

        Ok(Recorder {
            out,
            hash_every: REPLAY_HASH_EVERY,
            first_ids: next_ids(self),
            last: None,
            written: start_tick,
            scratch: Vec::new(),
            table: Vec::new(),
            error: None,
        })
    }

    #[must_use = "call .step() on the returned world"]
    pub fn record(&mut self, recorder: &mut Recorder) -> &mut Self {
        recorder.record(self);
        self
    }
}

#[derive(Debug)]
pub enum ReplayError {
    Io(io::Error),
    NotAReplay,
    DifferentBuild,
    Corrupted,
    BeforeStart { tick: u64, start: u64 },
    Diverged(Divergence),
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplayError::Io(error) => write!(f, "can't read the replay: {error}"),
            ReplayError::NotAReplay => write!(f, "this file is not a replay"),
            ReplayError::DifferentBuild => {
                write!(f, "this replay was recorded by a different build")
            }
            ReplayError::Corrupted => write!(f, "this replay is corrupted"),
            ReplayError::BeforeStart { tick, start } => {
                write!(
                    f,
                    "tick {tick} is before the recording, which starts at tick {start}"
                )
            }
            ReplayError::Diverged(divergence) => {
                match divergence.last_match {
                    Some(last) => write!(
                        f,
                        "the replay diverged between ticks {last} and {}:",
                        divergence.tick
                    )?,
                    None => write!(
                        f,
                        "the replay diverged at or before tick {}:",
                        divergence.tick
                    )?,
                }
                for resource in &divergence.resources {
                    write!(f, " {}", resource.name)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ReplayError {}

impl From<io::Error> for ReplayError {
    fn from(error: io::Error) -> Self {
        ReplayError::Io(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDifference {
    pub name: &'static str,
    pub expected: Option<u64>,
    pub found: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub last_match: Option<u64>,
    pub tick: u64,
    pub resources: Vec<ResourceDifference>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayReport {
    pub last_tick: u64,
    pub inputs: u64,
    pub hashes_checked: u64,
    pub divergence: Option<Divergence>,
    pub truncated: bool,
}

struct HashCheck {
    table: Rc<[u128]>,
    values: Vec<u64>,
}

fn resource_name(id: u128) -> &'static str {
    get_registry_entry(id).map_or("unregistered", |entry| entry.type_name)
}

fn differences(check: &HashCheck, found: &[(u128, u64)]) -> Vec<ResourceDifference> {
    let mut out = Vec::new();
    let (mut expected_at, mut found_at) = (0, 0);
    while expected_at < check.table.len() || found_at < found.len() {
        let expected_id = check.table.get(expected_at).copied().unwrap_or(u128::MAX);
        let found_id = found.get(found_at).map_or(u128::MAX, |hash| hash.0);
        if expected_id == found_id {
            if check.values[expected_at] != found[found_at].1 {
                out.push(ResourceDifference {
                    name: resource_name(expected_id),
                    expected: Some(check.values[expected_at]),
                    found: Some(found[found_at].1),
                });
            }
            expected_at += 1;
            found_at += 1;
        } else if expected_id < found_id {
            out.push(ResourceDifference {
                name: resource_name(expected_id),
                expected: Some(check.values[expected_at]),
                found: None,
            });
            expected_at += 1;
        } else {
            out.push(ResourceDifference {
                name: resource_name(found_id),
                expected: None,
                found: Some(found[found_at].1),
            });
            found_at += 1;
        }
    }
    out
}

struct TickItem {
    tick: u64,
    hash: Option<HashCheck>,
    inputs: Vec<InputBatch>,
}

struct Segment {
    tick: u64,
    state: Vec<u8>,
    items: Range<usize>,
}

#[derive(Default)]
struct Progress {
    inputs: u64,
    hashes_checked: u64,
    last_match: Option<u64>,
}

fn apply_item(
    world: &mut World,
    item: &TickItem,
    progress: &mut Progress,
) -> Result<Option<Divergence>, ReplayError> {
    for batch in &item.inputs {
        let input = input_signals()
            .get(batch.kind as usize)
            .ok_or(ReplayError::Corrupted)?;
        for payload in &batch.payloads {
            if !(input.resend)(world, payload) {
                return Err(ReplayError::Corrupted);
            }
            progress.inputs += 1;
        }
    }
    if let Some(check) = &item.hash {
        progress.hashes_checked += 1;
        let resources = differences(check, &world.resource_hashes());
        if !resources.is_empty() {
            return Ok(Some(Divergence {
                last_match: progress.last_match,
                tick: item.tick,
                resources,
            }));
        }
        progress.last_match = Some(item.tick);
    }
    Ok(None)
}

#[derive(Clone, Copy)]
struct SnapshotSettings {
    every: u64,
    budget: u64,
}

pub struct Replay {
    path: PathBuf,
    segments: Vec<Segment>,
    items: Vec<TickItem>,
    truncated: bool,
    snapshots: Option<SnapshotSettings>,
}

fn read_state(reader: &mut impl Read, len: u64) -> io::Result<Option<Vec<u8>>> {
    let mut state = Vec::new();
    let read = reader.take(len).read_to_end(&mut state)?;
    Ok((read as u64 == len).then_some(state))
}

impl Replay {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ReplayError> {
        let path = path.as_ref().to_path_buf();
        let mut reader = BufReader::new(File::open(&path)?);

        let mut magic = [0u8; 8];
        match reader.read_exact(&mut magic) {
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                return Err(ReplayError::NotAReplay);
            }
            result => result?,
        }
        if &magic != MAGIC {
            return Err(ReplayError::NotAReplay);
        }

        let header: Header = options()
            .deserialize_from(&mut reader)
            .map_err(|_| ReplayError::Corrupted)?;
        if header.build_id != build_id() {
            return Err(ReplayError::DifferentBuild);
        }
        let start_state =
            read_state(&mut reader, header.state_len)?.ok_or(ReplayError::Corrupted)?;

        let mut replay = Self {
            path,
            snapshots: None,
            segments: vec![Segment {
                tick: header.start_tick,
                state: start_state,
                items: 0..0,
            }],
            items: Vec::new(),
            truncated: false,
        };
        replay.truncated = replay.read_records(&mut reader)?;
        Ok(replay)
    }

    fn read_records(&mut self, reader: &mut BufReader<File>) -> Result<bool, ReplayError> {
        let mut tick = self.segments[0].tick;
        let mut table: Rc<[u128]> = Rc::from(Vec::new());
        loop {
            if reader.fill_buf()?.is_empty() {
                return Ok(false);
            }
            let record: Record = match options().deserialize_from(&mut *reader) {
                Ok(record) => record,
                Err(error) => {
                    if matches!(&*error, bincode::ErrorKind::Io(io)
                        if io.kind() == ErrorKind::UnexpectedEof)
                    {
                        return Ok(true);
                    }
                    return Err(ReplayError::Corrupted);
                }
            };
            match record {
                Record::Tick { gap, hash, inputs } => {
                    tick = tick.checked_add(gap).ok_or(ReplayError::Corrupted)?;
                    let hash = match hash {
                        Some(record) => {
                            if let Some(new_table) = record.table {
                                table = Rc::from(new_table);
                            }
                            if record.values.len() != table.len() {
                                return Err(ReplayError::Corrupted);
                            }
                            Some(HashCheck {
                                table: Rc::clone(&table),
                                values: record.values,
                            })
                        }
                        None => None,
                    };
                    self.items.push(TickItem { tick, hash, inputs });
                    let segment = self.segments.last_mut().unwrap();
                    segment.items.end = self.items.len();
                }
                Record::Section {
                    tick: section,
                    state_len,
                } => {
                    let Some(state) = read_state(&mut *reader, state_len)? else {
                        return Ok(true);
                    };
                    tick = section;
                    self.segments.push(Segment {
                        tick,
                        state,
                        items: self.items.len()..self.items.len(),
                    });
                }
            }
        }
    }

    fn snapshot_settings(&mut self) -> &mut SnapshotSettings {
        self.snapshots.get_or_insert(SnapshotSettings {
            every: SNAPSHOT_EVERY,
            budget: SNAPSHOT_BUDGET_BYTES,
        })
    }

    pub fn with_snapshots(mut self) -> Self {
        self.snapshot_settings();
        self
    }

    pub fn with_snapshot_every(mut self, ticks: u64) -> Self {
        assert!(ticks > 0, "snapshot_every must be at least 1");
        self.snapshot_settings().every = ticks;
        self
    }

    pub fn with_snapshot_budget(mut self, bytes: u64) -> Self {
        self.snapshot_settings().budget = bytes;
        self
    }

    pub fn snapshot_folder(&self) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(".snapshots");
        PathBuf::from(name)
    }

    fn owner(&self, tick: u64) -> Option<usize> {
        self.segments.iter().rposition(|x| x.tick <= tick)
    }

    fn recorded_until(&self, segment: usize) -> u64 {
        let segment = &self.segments[segment];
        self.items[segment.items.clone()]
            .last()
            .map_or(segment.tick, |item| item.tick)
    }

    pub fn start_tick(&self) -> u64 {
        self.segments.iter().map(|x| x.tick).min().unwrap()
    }

    pub fn last_tick(&self) -> u64 {
        self.recorded_until(self.segments.len() - 1)
    }

    pub fn verify(&self) -> Result<ReplayReport, ReplayError> {
        let mut world = World::new();
        let mut progress = Progress::default();
        let mut report = ReplayReport {
            last_tick: self.segments[0].tick,
            inputs: 0,
            hashes_checked: 0,
            divergence: None,
            truncated: self.truncated,
        };

        for segment in &self.segments {
            world
                .restore_recorded(&segment.state)
                .ok_or(ReplayError::Corrupted)?;
            report.last_tick = segment.tick;
            progress.last_match = None;

            for item in &self.items[segment.items.clone()] {
                while current_tick(&world) < item.tick {
                    world.step();
                }
                report.last_tick = item.tick;
                report.divergence = apply_item(&mut world, item, &mut progress)?;
                report.inputs = progress.inputs;
                report.hashes_checked = progress.hashes_checked;
                if report.divergence.is_some() {
                    return Ok(report);
                }
            }
        }
        Ok(report)
    }

    pub fn seek(&self, tick: u64) -> Result<ReplayPlayer<'_>, ReplayError> {
        let segment = self.owner(tick).ok_or(ReplayError::BeforeStart {
            tick,
            start: self.start_tick(),
        })?;
        let store = self
            .snapshots
            .map(|_| SnapshotStore::open(self.snapshot_folder(), build_id()));
        let mut player = ReplayPlayer {
            replay: self,
            world: World::new(),
            segment,
            next: 0,
            progress: Progress::default(),
            store,
            started_at: 0,
            last_snapshot: 0,
            snapshots_written: 0,
            snapshot_error: None,
        };
        if !player.enter_snapshot(segment, tick) {
            player.enter(segment)?;
        }
        player.started_at = player.tick();
        while player.tick() < tick {
            player.advance()?;
        }
        Ok(player)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerReport {
    pub tick: u64,
    pub inputs: u64,
    pub hashes_checked: u64,
    pub past_recording: bool,
    pub started_at: u64,
    pub snapshots_written: u64,
}

pub struct ReplayPlayer<'a> {
    replay: &'a Replay,
    world: World,
    segment: usize,
    next: usize,
    progress: Progress,
    store: Option<SnapshotStore>,
    started_at: u64,
    last_snapshot: u64,
    snapshots_written: u64,
    snapshot_error: Option<io::Error>,
}

impl ReplayPlayer<'_> {
    pub fn snapshot_error(&self) -> Option<&io::Error> {
        self.snapshot_error.as_ref()
    }

    pub fn world(&self) -> &World {
        &self.world
    }

    pub fn tick(&self) -> u64 {
        current_tick(&self.world)
    }

    pub fn report(&self) -> PlayerReport {
        let tick = self.tick();
        PlayerReport {
            tick,
            inputs: self.progress.inputs,
            hashes_checked: self.progress.hashes_checked,
            past_recording: tick > self.replay.recorded_until(self.segment),
            started_at: self.started_at,
            snapshots_written: self.snapshots_written,
        }
    }

    pub fn advance(&mut self) -> Result<(), ReplayError> {
        let tick = self.tick() + 1;
        let owner = self.replay.owner(tick).unwrap_or(self.segment);
        if owner != self.segment {
            return self.enter(owner);
        }
        self.world.step();
        self.apply()
    }

    fn enter(&mut self, segment: usize) -> Result<(), ReplayError> {
        let entered = &self.replay.segments[segment];
        self.world
            .restore_recorded(&entered.state)
            .ok_or(ReplayError::Corrupted)?;
        self.segment = segment;
        self.next = entered.items.start;
        self.progress.last_match = None;
        self.last_snapshot = entered.tick;
        self.apply()
    }

    fn enter_snapshot(&mut self, segment: usize, tick: u64) -> bool {
        let replay = self.replay;
        let items = replay.segments[segment].items.clone();
        loop {
            let Some(store) = &mut self.store else {
                return false;
            };
            let Some(at) = store.latest(segment as u64, tick) else {
                return false;
            };

            let index = items.start + replay.items[items.clone()].partition_point(|x| x.tick < at);
            let check = replay.items[..items.end]
                .get(index)
                .filter(|item| item.tick == at)
                .and_then(|item| item.hash.as_ref());
            let restored = check.is_some_and(|check| {
                store.read(segment as u64, at).is_some_and(|state| {
                    self.world.restore_recorded(&state).is_some()
                        && differences(check, &self.world.resource_hashes()).is_empty()
                })
            });
            if !restored {
                store.remove(segment as u64, at);
                continue;
            }

            self.segment = segment;
            self.next = index + 1;
            self.progress.hashes_checked += 1;
            self.progress.last_match = Some(at);
            self.last_snapshot = at;
            return true;
        }
    }

    fn write_snapshot(&mut self, tick: u64) {
        let (Some(settings), Some(store)) = (self.replay.snapshots, &mut self.store) else {
            return;
        };
        if self.snapshot_error.is_some() {
            return;
        }
        let segment = self.segment as u64;
        if store.contains(segment, tick) {
            self.last_snapshot = tick;
            return;
        }
        if tick - self.last_snapshot < settings.every {
            return;
        }

        let starts: Vec<u64> = self.replay.segments.iter().map(|x| x.tick).collect();
        match store.write(segment, tick, &self.world.save(), settings.budget, &starts) {
            Ok(true) => {
                self.snapshots_written += 1;
                self.last_snapshot = tick;
            }
            Ok(false) => {}
            Err(error) => self.snapshot_error = Some(error),
        }
    }

    fn apply(&mut self) -> Result<(), ReplayError> {
        let tick = self.tick();
        let end = self.replay.segments[self.segment].items.end;
        while self.next < end && self.replay.items[self.next].tick <= tick {
            let item = &self.replay.items[self.next];
            self.next += 1;
            if item.tick < tick {
                continue;
            }
            if let Some(divergence) = apply_item(&mut self.world, item, &mut self.progress)? {
                return Err(ReplayError::Diverged(divergence));
            }
            if item.hash.is_some() {
                self.write_snapshot(tick);
            }
        }
        Ok(())
    }
}
