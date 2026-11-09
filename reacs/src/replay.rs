use std::{
    fmt,
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, ErrorKind, Read, Write},
    path::Path,
    ptr::addr_of_mut,
    rc::Rc,
};

use bincode::Options;
use serde::{Deserialize, Serialize};

use crate::{
    Tick,
    hashed::Hashed,
    registry::registered_ids,
    schedule::{Mut, system_graph},
    signal::{InputSignal, Signal, SignalData, SignalType, Signals},
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

pub fn send_recorded(world: &mut World, signal: u128, payload: &[u8]) -> bool {
    let inputs = input_signals();
    match inputs.binary_search_by_key(&signal, |input| input.uuid) {
        Ok(at) => (inputs[at].resend)(world, payload),
        Err(_) => false,
    }
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

pub fn build_id() -> u64 {
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
pub enum RecordingError {
    Io(io::Error),
    NotARecording,
    DifferentBuild,
    Corrupted,
}

impl fmt::Display for RecordingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordingError::Io(error) => write!(f, "can't read the recording: {error}"),
            RecordingError::NotARecording => write!(f, "this file is not a recording"),
            RecordingError::DifferentBuild => {
                write!(f, "this recording was made by a different build")
            }
            RecordingError::Corrupted => write!(f, "this recording is corrupted"),
        }
    }
}

impl std::error::Error for RecordingError {}

impl From<io::Error> for RecordingError {
    fn from(error: io::Error) -> Self {
        RecordingError::Io(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedInputs {
    pub signal: u128,
    pub payloads: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedHashes {
    ids: Rc<[u128]>,
    values: Vec<u64>,
}

impl RecordedHashes {
    pub fn ids(&self) -> &[u128] {
        &self.ids
    }

    pub fn values(&self) -> &[u64] {
        &self.values
    }

    pub fn iter(&self) -> impl Iterator<Item = (u128, u64)> + '_ {
        self.ids.iter().copied().zip(self.values.iter().copied())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedTick {
    pub tick: u64,
    pub inputs: Vec<RecordedInputs>,
    pub hashes: Option<RecordedHashes>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedSection {
    pub tick: u64,
    pub state: Vec<u8>,
    pub ticks: Vec<RecordedTick>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    pub sections: Vec<RecordedSection>,
    pub truncated: bool,
}

fn read_state(reader: &mut impl Read, len: u64) -> io::Result<Option<Vec<u8>>> {
    let mut state = Vec::new();
    let read = reader.take(len).read_to_end(&mut state)?;
    Ok((read as u64 == len).then_some(state))
}

impl Recording {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RecordingError> {
        let mut reader = BufReader::new(File::open(path)?);

        let mut magic = [0u8; 8];
        match reader.read_exact(&mut magic) {
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                return Err(RecordingError::NotARecording);
            }
            result => result?,
        }
        if &magic != MAGIC {
            return Err(RecordingError::NotARecording);
        }

        let header: Header = options()
            .deserialize_from(&mut reader)
            .map_err(|_| RecordingError::Corrupted)?;
        if header.build_id != build_id() {
            return Err(RecordingError::DifferentBuild);
        }
        let state = read_state(&mut reader, header.state_len)?.ok_or(RecordingError::Corrupted)?;

        let mut recording = Self {
            sections: vec![RecordedSection {
                tick: header.start_tick,
                state,
                ticks: Vec::new(),
            }],
            truncated: false,
        };
        recording.truncated = recording.read_records(&mut reader, &header.inputs)?;
        Ok(recording)
    }

    fn read_records(
        &mut self,
        reader: &mut BufReader<File>,
        signals: &[u128],
    ) -> Result<bool, RecordingError> {
        let mut tick = self.sections[0].tick;
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
                    return Err(RecordingError::Corrupted);
                }
            };
            match record {
                Record::Tick { gap, hash, inputs } => {
                    tick = tick.checked_add(gap).ok_or(RecordingError::Corrupted)?;
                    let hashes = match hash {
                        Some(record) => {
                            if let Some(new_table) = record.table {
                                table = Rc::from(new_table);
                            }
                            if record.values.len() != table.len() {
                                return Err(RecordingError::Corrupted);
                            }
                            Some(RecordedHashes {
                                ids: Rc::clone(&table),
                                values: record.values,
                            })
                        }
                        None => None,
                    };
                    let mut recorded = Vec::with_capacity(inputs.len());
                    for batch in inputs {
                        let signal = signals
                            .get(batch.kind as usize)
                            .ok_or(RecordingError::Corrupted)?;
                        recorded.push(RecordedInputs {
                            signal: *signal,
                            payloads: batch.payloads,
                        });
                    }
                    self.sections.last_mut().unwrap().ticks.push(RecordedTick {
                        tick,
                        inputs: recorded,
                        hashes,
                    });
                }
                Record::Section {
                    tick: section,
                    state_len,
                } => {
                    let Some(state) = read_state(&mut *reader, state_len)? else {
                        return Ok(true);
                    };
                    tick = section;
                    self.sections.push(RecordedSection {
                        tick,
                        state,
                        ticks: Vec::new(),
                    });
                }
            }
        }
    }
}
