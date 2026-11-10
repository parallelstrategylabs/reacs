use std::{
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, BufWriter, ErrorKind, Read, Write},
    path::{Path, PathBuf},
    ptr::addr_of_mut,
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

const MAGIC: &[u8; 8] = b"REACSRC2";
pub const RECORD_HASH_EVERY: u64 = 64;

fn options() -> bincode::DefaultOptions {
    bincode::DefaultOptions::new()
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, message)
}

fn corrupted() -> io::Error {
    invalid("this recording is corrupted")
}

struct InputEntry {
    uuid: u128,
    init: fn(&mut World),
    next_id: fn(&World) -> u64,
    capture: fn(&World, u64, &mut Vec<Vec<u8>>),
    resend: fn(&mut World, &[u8]) -> bool,
}

static mut INPUT_REGISTRY: Option<Vec<InputEntry>> = None;

fn next_input_id<S: SignalType>(world: &World) -> u64 {
    let data = world.get_resource::<SignalData<S>>();
    data.map_or(0, |data| data.next)
}

fn capture_input<S: SignalType>(world: &World, first_id: u64, out: &mut Vec<Vec<u8>>) {
    if let Some(signals) = world.get_resource::<Signals<S>>() {
        let sent = signals.transient_buffer.iter();
        out.extend(sent.filter(|x| x.id >= first_id).map(|x| {
            let payload = options().serialize(&x.payload);
            payload.expect("failed to encode an input signal")
        }));
    }
}

fn resend_input<S: SignalType>(world: &mut World, bytes: &[u8]) -> bool {
    let signal = options().deserialize::<S>(bytes);
    signal.is_ok_and(|signal| {
        world.fetch_param::<Mut<Signal<S>>>().emit(signal);
        true
    })
}

pub fn register_input_signal<S: InputSignal>() {
    let entry = InputEntry {
        uuid: Signals::<S>::TYPE_UUID,
        init: World::init_signal::<S>,
        next_id: next_input_id::<S>,
        capture: capture_input::<S>,
        resend: resend_input::<S>,
    };
    unsafe {
        let inputs = (*addr_of_mut!(INPUT_REGISTRY)).get_or_insert_with(Vec::new);
        let at = inputs.partition_point(|x| x.uuid < entry.uuid);
        inputs.insert(at, entry);
    }
}

fn input_signals() -> &'static [InputEntry] {
    unsafe { (*addr_of_mut!(INPUT_REGISTRY)).as_deref().unwrap_or(&[]) }
}

pub(crate) fn init_input_signals(world: &mut World) {
    for input in input_signals() {
        (input.init)(world);
    }
}

fn next_ids(world: &World) -> Vec<u64> {
    let inputs = input_signals().iter();
    inputs.map(|input| (input.next_id)(world)).collect()
}

pub fn send_recorded(world: &mut World, signal: u128, payload: &[u8]) -> bool {
    let inputs = input_signals();
    let found = inputs.binary_search_by_key(&signal, |input| input.uuid);
    found.is_ok_and(|at| (inputs[at].resend)(world, payload))
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

#[derive(Serialize, Deserialize)]
struct Header {
    build_id: u64,
    start_tick: u64,
    state_len: u64,
}

#[derive(Serialize, Deserialize)]
enum Record {
    Tick {
        gap: u64,
        hashes: Option<(Option<Vec<u128>>, Vec<u64>)>,
        inputs: Vec<(u32, Vec<Vec<u8>>)>,
    },
    Section {
        tick: u64,
        state_len: u64,
        ended: Option<u64>,
    },
    End {
        tick: u64,
    },
}

fn write<T: Serialize>(out: &mut impl Write, value: &T, state: &[u8]) -> io::Result<()> {
    let encoded = options().serialize_into(&mut *out, value);
    encoded.map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    out.write_all(state)?;
    out.flush()
}

pub struct Recorder {
    out: BufWriter<File>,
    hash_every: u64,
    first_ids: Vec<u64>,
    last: Option<u64>,
    written: u64,
    table: Vec<u128>,
    loads: u64,
    steps: u64,
    section_start: u64,
    error: Option<io::Error>,
}

impl Recorder {
    pub fn with_hash_every(mut self, ticks: u64) -> Self {
        assert!(ticks > 0, "hash_every must be at least 1");
        self.hash_every = ticks;
        self
    }

    pub fn record(&mut self, world: &World) {
        if self.error.is_none() {
            self.error = self.write_tick(world).err();
        }
    }

    pub fn finish(mut self) -> io::Result<()> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        if let Some(tick) = self.last {
            write(&mut self.out, &Record::End { tick }, &[])?;
        }
        self.out.flush()
    }

    fn write_tick(&mut self, world: &World) -> io::Result<()> {
        let tick = current_tick(world);
        let loaded = world.loads != self.loads;
        if self.last == Some(tick) && !loaded {
            return Ok(());
        }
        if loaded || tick != self.last.map_or(self.written, |last| last + 1) {
            let state = world.save();
            let state_len = state.len() as u64;
            let ran = world.steps.saturating_sub(self.steps);
            let reached = self.last.map_or(self.section_start, |last| last + ran);
            let ended = (reached > self.section_start).then(|| reached - 1);
            let record = Record::Section {
                tick,
                state_len,
                ended,
            };
            write(&mut self.out, &record, &state)?;
            self.written = tick;
            self.section_start = tick;
            self.first_ids = next_ids(world);
        }
        self.loads = world.loads;
        self.steps = world.steps;
        self.last = Some(tick);

        let mut inputs = Vec::new();
        for (kind, (input, first_id)) in input_signals().iter().zip(&self.first_ids).enumerate() {
            let mut payloads = Vec::new();
            (input.capture)(world, *first_id, &mut payloads);
            if !payloads.is_empty() {
                inputs.push((kind as u32, payloads));
            }
        }

        let hashes = (tick % self.hash_every == 0).then(|| {
            let (ids, values): (Vec<u128>, Vec<u64>) = world.resource_hashes().into_iter().unzip();
            let table = (ids != self.table).then(|| {
                self.table.clone_from(&ids);
                ids
            });
            (table, values)
        });
        if inputs.is_empty() && hashes.is_none() {
            return Ok(());
        }

        let gap = tick - self.written;
        self.written = tick;
        let record = Record::Tick {
            gap,
            hashes,
            inputs,
        };
        write(&mut self.out, &record, &[])
    }
}

impl World {
    pub fn to_recorder(&self, path: impl AsRef<Path>) -> io::Result<Recorder> {
        let mut out = BufWriter::new(File::create(path)?);
        let state = self.save();
        let start_tick = current_tick(self);
        let header = Header {
            build_id: build_id(),
            start_tick,
            state_len: state.len() as u64,
        };
        out.write_all(MAGIC)?;
        write(&mut out, &header, &state)?;

        Ok(Recorder {
            out,
            hash_every: RECORD_HASH_EVERY,
            first_ids: next_ids(self),
            last: None,
            written: start_tick,
            table: Vec::new(),
            loads: self.loads,
            steps: self.steps,
            section_start: start_tick,
            error: None,
        })
    }

    #[must_use = "call .step() on the returned world"]
    pub fn record(&mut self, recorder: &mut Recorder) -> &mut Self {
        recorder.record(self);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedInputs {
    pub signal: u128,
    pub payloads: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedTick {
    pub tick: u64,
    pub inputs: Vec<RecordedInputs>,
    pub hashes: Option<Vec<(u128, u64)>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedSection {
    pub tick: u64,
    pub end: u64,
    pub state: Vec<u8>,
    pub ticks: Vec<RecordedTick>,
}

const UNKNOWN_END: u64 = u64::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    pub sections: Vec<RecordedSection>,
    pub truncated: bool,
}

fn read_section(
    reader: &mut impl Read,
    tick: u64,
    len: u64,
) -> io::Result<Option<RecordedSection>> {
    let mut state = Vec::new();
    let read = reader.take(len).read_to_end(&mut state)?;
    Ok((read as u64 == len).then_some(RecordedSection {
        tick,
        end: UNKNOWN_END,
        state,
        ticks: Vec::new(),
    }))
}

impl Recording {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut reader = BufReader::new(File::open(path)?);

        let mut magic = [0u8; 8];
        if reader.read_exact(&mut magic).is_err() || &magic != MAGIC {
            return Err(invalid("this file is not a recording"));
        }
        let header = options().deserialize_from::<_, Header>(&mut reader);
        let header = header.map_err(|_| corrupted())?;
        if header.build_id != build_id() {
            return Err(invalid("this recording was made by a different build"));
        }
        let start = read_section(&mut reader, header.start_tick, header.state_len)?;

        let mut recording = Self {
            sections: vec![start.ok_or_else(corrupted)?],
            truncated: false,
        };
        recording.truncated = recording.read_records(&mut reader)?;
        let last = recording.sections.last_mut().unwrap();
        if last.end == UNKNOWN_END {
            last.end = last.ticks.last().map_or(last.tick, |x| x.tick + 1);
        }
        Ok(recording)
    }

    fn read_records(&mut self, reader: &mut BufReader<File>) -> io::Result<bool> {
        let mut tick = self.sections[0].tick;
        let mut table = Vec::new();
        while !reader.fill_buf()?.is_empty() {
            let record = match options().deserialize_from(&mut *reader) {
                Ok(record) => record,
                Err(error) => {
                    let cut = matches!(&*error, bincode::ErrorKind::Io(io)
                        if io.kind() == ErrorKind::UnexpectedEof);
                    return if cut { Ok(true) } else { Err(corrupted()) };
                }
            };
            match record {
                Record::Tick {
                    gap,
                    hashes,
                    inputs,
                } => {
                    tick = tick.checked_add(gap).ok_or_else(corrupted)?;
                    let hashes = match hashes {
                        Some((new_table, values)) => {
                            table = new_table.unwrap_or(table);
                            if values.len() != table.len() {
                                return Err(corrupted());
                            }
                            Some(table.iter().copied().zip(values).collect())
                        }
                        None => None,
                    };
                    let inputs = inputs.into_iter().map(|(kind, payloads)| {
                        let input = input_signals().get(kind as usize).ok_or_else(corrupted)?;
                        Ok(RecordedInputs {
                            signal: input.uuid,
                            payloads,
                        })
                    });
                    let inputs = inputs.collect::<io::Result<_>>()?;
                    self.sections.last_mut().unwrap().ticks.push(RecordedTick {
                        tick,
                        inputs,
                        hashes,
                    });
                }
                Record::Section {
                    tick: start,
                    state_len,
                    ended,
                } => {
                    let previous = self.sections.last_mut().unwrap();
                    let end = match ended {
                        Some(last) => last.checked_add(1).ok_or_else(corrupted)?,
                        None => previous.tick,
                    };
                    if end < previous.tick || previous.ticks.last().is_some_and(|x| x.tick > end) {
                        return Err(corrupted());
                    }
                    previous.end = end;
                    let Some(section) = read_section(&mut *reader, start, state_len)? else {
                        return Ok(true);
                    };
                    tick = start;
                    self.sections.push(section);
                }
                Record::End { tick: last } => {
                    let section = self.sections.last_mut().unwrap();
                    let end = last.checked_add(1).ok_or_else(corrupted)?;
                    if end < section.tick || section.ticks.last().is_some_and(|x| x.tick >= end) {
                        return Err(corrupted());
                    }
                    section.end = end;
                }
            }
        }
        Ok(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    pub tick: u64,
    pub resources: Vec<u128>,
}

pub struct Playback {
    recording: Recording,
    section: usize,
    next: usize,
    at: u64,
    section_ids: Vec<u64>,
    divergence: Option<Divergence>,
}

fn differing(recorded: &[(u128, u64)], current: &[(u128, u64)]) -> Vec<u128> {
    let mut out = Vec::new();
    let (mut a, mut b) = (recorded.iter().peekable(), current.iter().peekable());
    loop {
        match (a.peek(), b.peek()) {
            (Some(x), Some(y)) if x.0 == y.0 => {
                if x.1 != y.1 {
                    out.push(x.0);
                }
                a.next();
                b.next();
            }
            (Some(x), Some(y)) if x.0 < y.0 => {
                out.push(x.0);
                a.next();
            }
            (Some(_), Some(y)) => {
                out.push(y.0);
                b.next();
            }
            (Some(x), None) => {
                out.push(x.0);
                a.next();
            }
            (None, Some(y)) => {
                out.push(y.0);
                b.next();
            }
            (None, None) => return out,
        }
    }
}

impl Playback {
    pub fn recording(&self) -> &Recording {
        &self.recording
    }

    pub fn finished(&self) -> bool {
        let sections = &self.recording.sections;
        self.section + 1 == sections.len() && self.at >= sections[self.section].end
    }

    pub fn diverged(&self) -> Option<&Divergence> {
        self.divergence.as_ref()
    }

    fn apply(&mut self, world: &mut World) {
        if self.finished() {
            return;
        }
        while self.section + 1 < self.recording.sections.len()
            && current_tick(world) >= self.recording.sections[self.section].end
        {
            self.section += 1;
            self.next = 0;
            let state = &self.recording.sections[self.section].state;
            world
                .restore(state)
                .unwrap_or_else(|| panic!("this recording is corrupted"));
            self.section_ids = next_ids(world);
        }

        let tick = current_tick(world);
        let ticks = &self.recording.sections[self.section].ticks;
        while self.next < ticks.len() && ticks[self.next].tick < tick {
            self.next += 1;
        }
        if let Some(recorded) = ticks.get(self.next).filter(|x| x.tick == tick) {
            for inputs in &recorded.inputs {
                for payload in &inputs.payloads {
                    send_recorded(world, inputs.signal, payload);
                }
            }
            if let Some(hashes) = &recorded.hashes
                && self.divergence.is_none()
            {
                let resources = differing(hashes, &world.resource_hashes());
                if !resources.is_empty() {
                    self.divergence = Some(Divergence { tick, resources });
                }
            }
            self.next += 1;
        }
        self.at = tick + 1;
    }

    pub fn to_recorder(self, world: &World, path: impl AsRef<Path>) -> io::Result<Recorder> {
        let tick = current_tick(world);
        let sections = &self.recording.sections[..=self.section];
        let current = &sections[self.section];
        if tick < current.tick || (tick != self.at && tick + 1 != self.at) {
            let message = "the world isn't where the playback left it";
            return Err(io::Error::new(ErrorKind::InvalidInput, message));
        }

        let path = path.as_ref();
        let mut temp = path.as_os_str().to_owned();
        temp.push(".reacs-tmp");
        let temp = PathBuf::from(temp);
        let history = self.write_history(&temp, tick);
        let (written, table) = match history.and_then(|x| std::fs::rename(&temp, path).map(|_| x)) {
            Ok(history) => history,
            Err(error) => {
                let _ = std::fs::remove_file(&temp);
                return Err(error);
            }
        };

        let first_ids = if tick == current.tick {
            self.section_ids
        } else {
            vec![0; input_signals().len()]
        };
        Ok(Recorder {
            out: BufWriter::new(OpenOptions::new().append(true).open(path)?),
            hash_every: RECORD_HASH_EVERY,
            first_ids,
            last: (tick > current.tick).then(|| tick - 1),
            written,
            table,
            loads: world.loads,
            steps: world.steps,
            section_start: current.tick,
            error: None,
        })
    }

    fn write_history(&self, temp: &Path, tick: u64) -> io::Result<(u64, Vec<u128>)> {
        let sections = &self.recording.sections[..=self.section];
        let mut out = BufWriter::new(File::create(temp)?);
        let start = &sections[0];
        let header = Header {
            build_id: build_id(),
            start_tick: start.tick,
            state_len: start.state.len() as u64,
        };
        out.write_all(MAGIC)?;
        write(&mut out, &header, &start.state)?;

        let mut written = start.tick;
        let mut table = Vec::new();
        for (i, section) in sections.iter().enumerate() {
            if i > 0 {
                let previous = &sections[i - 1];
                let record = Record::Section {
                    tick: section.tick,
                    state_len: section.state.len() as u64,
                    ended: (previous.end > previous.tick).then(|| previous.end - 1),
                };
                write(&mut out, &record, &section.state)?;
                written = section.tick;
            }
            let last = i == self.section;
            for recorded in section.ticks.iter().take_while(|x| !last || x.tick < tick) {
                let hashes = recorded.hashes.as_ref().map(|hashes| {
                    let (ids, values): (Vec<u128>, Vec<u64>) = hashes.iter().copied().unzip();
                    let changed = (ids != table).then(|| {
                        table.clone_from(&ids);
                        ids
                    });
                    (changed, values)
                });
                let inputs = recorded.inputs.iter().map(|inputs| {
                    let found = input_signals().binary_search_by_key(&inputs.signal, |x| x.uuid);
                    let kind = found.map_err(|_| corrupted())?;
                    Ok((kind as u32, inputs.payloads.clone()))
                });
                let record = Record::Tick {
                    gap: recorded.tick - written,
                    hashes,
                    inputs: inputs.collect::<io::Result<_>>()?,
                };
                write(&mut out, &record, &[])?;
                written = recorded.tick;
            }
        }
        out.flush()?;
        Ok((written, table))
    }
}

impl World {
    pub fn to_playback(&mut self, path: impl AsRef<Path>) -> io::Result<Playback> {
        let recording = Recording::open(path)?;
        let start = &recording.sections[0];
        if self.restore(&start.state).is_none() {
            return Err(corrupted());
        }
        let at = start.tick;
        Ok(Playback {
            recording,
            section: 0,
            next: 0,
            at,
            section_ids: next_ids(self),
            divergence: None,
        })
    }

    #[must_use = "call .step() on the returned world"]
    pub fn replay(&mut self, playback: &mut Playback) -> &mut Self {
        playback.apply(self);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use std::{fs, path::PathBuf};

    #[signal(input)]
    struct Press(u32);

    #[signal(input)]
    struct Turn(i8);

    #[resource]
    struct Score(u64);

    fn path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("reacs-record-{}-{name}", std::process::id()))
    }

    fn advance(world: &mut World) {
        world.steps += 1;
        world.cleanup_signals();
        world.get_resource_mut::<Tick>().unwrap().0 += 1;
    }

    fn encoded<S: SignalType>(signal: S) -> Vec<u8> {
        options().serialize(&signal).unwrap()
    }

    fn payloads_of<S: SignalType>(recorded: &RecordedTick) -> Vec<Vec<u8>> {
        let id = Signals::<S>::TYPE_UUID;
        let found = recorded.inputs.iter().find(|inputs| inputs.signal == id);
        found.map_or_else(Vec::new, |inputs| inputs.payloads.clone())
    }

    fn finish(file: &PathBuf, recorder: Recorder) -> Vec<u8> {
        recorder.finish().unwrap();
        let bytes = fs::read(file).unwrap();
        fs::remove_file(file).unwrap();
        bytes
    }

    fn open(name: &str, bytes: &[u8]) -> io::Result<Recording> {
        let file = path(name);
        fs::write(&file, bytes).unwrap();
        let recording = Recording::open(&file);
        fs::remove_file(&file).unwrap();
        recording
    }

    fn raw_records(bytes: &[u8]) -> (Header, Vec<Record>) {
        let mut rest = &bytes[MAGIC.len()..];
        let header: Header = options().deserialize_from(&mut rest).unwrap();
        rest = &rest[header.state_len as usize..];
        let mut records = Vec::new();
        while !rest.is_empty() {
            let record = options().deserialize_from(&mut rest).unwrap();
            if let Record::Section { state_len, .. } = &record {
                rest = &rest[*state_len as usize..];
            }
            records.push(record);
        }
        (header, records)
    }

    fn with_record(bytes: &[u8], record: Record) -> Vec<u8> {
        let mut bytes = bytes.to_vec();
        write(&mut bytes, &record, &[]).unwrap();
        bytes
    }

    fn short_run(name: &str) -> Vec<u8> {
        let file = path(name);
        let mut world = World::new();
        let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(2);
        for tick in 0..6 {
            world.input(Press(tick));
            recorder.record(&world);
            advance(&mut world);
        }
        finish(&file, recorder)
    }

    #[test]
    fn inputs_are_stored_by_tick_and_type_in_send_order() {
        let file = path("inputs");
        let mut world = World::new();
        let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(u64::MAX);
        world.input(Press(1));
        world.input(Turn(-1));
        world.input(Press(2));
        recorder.record(&world);
        advance(&mut world);
        recorder.record(&world);
        advance(&mut world);
        world.input(Press(3));
        recorder.record(&world);
        let recording = open("inputs-read", &finish(&file, recorder)).unwrap();

        assert!(!recording.truncated);
        assert_eq!(recording.sections.len(), 1);
        let ticks = &recording.sections[0].ticks;
        assert_eq!(ticks.iter().map(|x| x.tick).collect::<Vec<_>>(), [0, 2]);
        assert_eq!(
            payloads_of::<Press>(&ticks[0]),
            [encoded(Press(1)), encoded(Press(2))]
        );
        assert_eq!(payloads_of::<Turn>(&ticks[0]), [encoded(Turn(-1))]);
        assert_eq!(payloads_of::<Press>(&ticks[1]), [encoded(Press(3))]);
        assert_eq!(payloads_of::<Turn>(&ticks[1]), Vec::<Vec<u8>>::new());
        assert!(ticks[0].hashes.is_some() && ticks[1].hashes.is_none());
    }

    #[test]
    fn inputs_sent_before_the_recorder_stay_in_the_start_state() {
        let file = path("early");
        let mut world = World::new();
        world.input(Press(9));
        let mut recorder = world.to_recorder(&file).unwrap();
        let start = world.save();
        world.input(Press(1));
        recorder.record(&world);
        let recording = open("early-read", &finish(&file, recorder)).unwrap();

        assert_eq!(recording.sections[0].state, start);
        let ticks = &recording.sections[0].ticks;
        assert_eq!(payloads_of::<Press>(&ticks[0]), [encoded(Press(1))]);
    }

    #[test]
    fn recorded_hashes_are_the_worlds_hashes_on_that_tick() {
        let file = path("hashes");
        let mut world = World::new();
        let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(2);
        let mut expected = Vec::new();
        for tick in 0..6 {
            if tick == 3 {
                world.insert_resource(Score(7));
            }
            world.input(Press(tick));
            recorder.record(&world);
            if tick % 2 == 0 {
                expected.push(world.resource_hashes());
            }
            advance(&mut world);
        }
        let bytes = finish(&file, recorder);

        let recording = open("hashes-read", &bytes).unwrap();
        let hashes = recording.sections[0].ticks.iter();
        let hashes: Vec<_> = hashes.filter_map(|x| x.hashes.clone()).collect();
        assert_eq!(hashes, expected);
        assert_eq!(hashes[2].len(), hashes[1].len() + 1);

        let tables: Vec<bool> = raw_records(&bytes)
            .1
            .iter()
            .filter_map(|record| match record {
                Record::Tick {
                    hashes: Some((table, _)),
                    ..
                } => Some(table.is_some()),
                _ => None,
            })
            .collect();
        assert_eq!(tables, [true, false, true]);
    }

    #[test]
    fn a_tick_is_written_once_however_often_it_is_recorded() {
        let file = path("twice");
        let mut world = World::new();
        let mut recorder = world.to_recorder(&file).unwrap();
        world.input(Press(1));
        recorder.record(&world);
        world.input(Press(2));
        recorder.record(&world);
        let _ = world.record(&mut recorder);
        let records = raw_records(&finish(&file, recorder)).1;

        assert_eq!(records.len(), 2);
        let Record::Tick { gap, inputs, .. } = &records[0] else {
            panic!("expected a tick record");
        };
        assert_eq!((*gap, inputs.len(), inputs[0].1.len()), (0, 1, 1));
        assert!(matches!(records[1], Record::End { tick: 0 }));
    }

    #[test]
    fn a_tick_that_does_not_follow_the_last_one_starts_a_section() {
        let file = path("section");
        let mut world = World::new();
        let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(u64::MAX);
        recorder.record(&world);
        advance(&mut world);
        advance(&mut world);
        world.input(Press(5));
        let jumped = world.save();
        recorder.record(&world);
        advance(&mut world);
        world.input(Press(6));
        recorder.record(&world);
        world.load(&jumped);
        recorder.record(&world);
        let recording = open("section-read", &finish(&file, recorder)).unwrap();

        let starts: Vec<u64> = recording.sections.iter().map(|x| x.tick).collect();
        assert_eq!(starts, [0, 2, 2]);
        assert_eq!(recording.sections[1].state, jumped);
        assert_eq!(recording.sections[2].state, jumped);
        let second = &recording.sections[1].ticks;
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].tick, 3);
        assert_eq!(payloads_of::<Press>(&second[0]), [encoded(Press(6))]);
        assert!(recording.sections[2].ticks.is_empty());
    }

    #[test]
    fn sections_know_the_tick_the_original_run_reached() {
        let file = path("ends");
        let mut world = World::new();
        let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(u64::MAX);
        recorder.record(&world);
        advance(&mut world);
        advance(&mut world);
        let jumped = world.save();
        recorder.record(&world);
        advance(&mut world);
        recorder.record(&world);
        advance(&mut world);
        world.load(&jumped);
        recorder.record(&world);
        let recording = open("ends-read", &finish(&file, recorder)).unwrap();

        let sections: Vec<(u64, u64)> =
            recording.sections.iter().map(|x| (x.tick, x.end)).collect();
        assert_eq!(sections, [(0, 2), (2, 4), (2, 3)]);
    }

    #[test]
    fn a_recording_without_finish_ends_after_its_last_tick() {
        let file = path("unfinished");
        let mut world = World::new();
        let mut recorder = world.to_recorder(&file).unwrap().with_hash_every(u64::MAX);
        for tick in 0..4 {
            if tick < 3 {
                world.input(Press(tick));
            }
            recorder.record(&world);
            advance(&mut world);
        }
        drop(recorder);
        let bytes = fs::read(&file).unwrap();
        fs::remove_file(&file).unwrap();

        let recording = open("unfinished-read", &bytes).unwrap();
        assert_eq!(recording.sections[0].end, 3);

        let file = path("empty");
        let recorder = World::new().to_recorder(&file).unwrap();
        let recording = open("empty-read", &finish(&file, recorder)).unwrap();
        assert_eq!(
            (recording.sections[0].tick, recording.sections[0].end),
            (0, 0)
        );
    }

    fn replay_file(name: &str, bytes: &[u8], world: &mut World) -> Playback {
        let file = path(name);
        fs::write(&file, bytes).unwrap();
        let playback = world.to_playback(&file);
        fs::remove_file(&file).unwrap();
        let mut playback = playback.unwrap();
        while !playback.finished() {
            let world = world.replay(&mut playback);
            advance(world);
        }
        playback
    }

    #[test]
    fn a_playback_runs_the_recording_again() {
        let mut world = World::new();
        let playback = replay_file("playback", &short_run("playback-run"), &mut world);

        assert_eq!(playback.diverged(), None);
        assert_eq!(current_tick(&world), 6);
        assert_eq!(playback.recording().sections[0].ticks.len(), 6);
    }

    #[test]
    fn a_playback_reports_the_first_tick_that_differs() {
        let mut world = World::new();
        world.insert_resource(Score(1));
        let file = path("diverge");
        fs::write(&file, short_run("diverge-run")).unwrap();
        let mut playback = world.to_playback(&file).unwrap();
        fs::remove_file(&file).unwrap();
        world.insert_resource(Score(2));
        while !playback.finished() {
            let world = world.replay(&mut playback);
            advance(world);
        }

        let divergence = playback.diverged().unwrap();
        assert_eq!(divergence.tick, 0);
        assert_eq!(divergence.resources, [Score::TYPE_UUID]);
    }

    #[test]
    fn a_corrupted_start_state_is_refused() {
        let mut bytes = short_run("bad-start");
        let at = MAGIC.len() + options().serialized_size(&raw_records(&bytes).0).unwrap() as usize;
        bytes[at..at + 8].fill(0xFF);
        let file = path("bad-start-read");
        fs::write(&file, &bytes).unwrap();
        let result = World::new().to_playback(&file);
        fs::remove_file(&file).unwrap();

        assert_eq!(result.err().unwrap().kind(), ErrorKind::InvalidData);
    }

    #[test]
    fn a_file_cut_anywhere_reads_up_to_the_cut() {
        let bytes = short_run("cut");
        let full = open("cut-full", &bytes).unwrap();
        assert!(!full.truncated);
        let (header, records) = raw_records(&bytes);
        assert_eq!(records.len(), full.sections[0].ticks.len() + 1);
        assert!(matches!(records.last(), Some(Record::End { tick: 5 })));
        assert_eq!(full.sections[0].end, 6);
        let start = bytes.len() - records.iter().map(|x| encoded_len(x)).sum::<usize>();
        assert!(start > header.state_len as usize);

        let mut truncated = 0;
        for cut in 0..bytes.len() {
            let Ok(part) = open("cut-part", &bytes[..cut]) else {
                assert!(cut < start, "cut at {cut}");
                continue;
            };
            assert!(cut >= start, "cut at {cut}");
            truncated += part.truncated as usize;
            let ticks = &part.sections[0].ticks;
            assert_eq!(ticks[..], full.sections[0].ticks[..ticks.len()]);
        }
        assert!(truncated > 0);
    }

    fn encoded_len(record: &Record) -> usize {
        options().serialized_size(record).unwrap() as usize
    }

    #[test]
    fn files_that_are_not_recordings_are_rejected() {
        let bytes = short_run("rejected");
        let message = |bytes: &[u8]| {
            let error = open("rejected-read", bytes).unwrap_err();
            assert_eq!(error.kind(), ErrorKind::InvalidData);
            error.to_string()
        };

        assert_eq!(message(b""), "this file is not a recording");
        assert_eq!(
            message(b"some other file entirely"),
            "this file is not a recording"
        );

        let mut other_build = bytes.clone();
        other_build[MAGIC.len()] ^= 1;
        assert_eq!(
            message(&other_build),
            "this recording was made by a different build"
        );

        let unknown_input = Record::Tick {
            gap: 1,
            hashes: None,
            inputs: vec![(u32::MAX, vec![vec![1]])],
        };
        let wrong_hash_count = Record::Tick {
            gap: 1,
            hashes: Some((None, vec![1; 1000])),
            inputs: Vec::new(),
        };
        let tick_overflow = Record::Tick {
            gap: u64::MAX,
            hashes: None,
            inputs: Vec::new(),
        };
        for record in [unknown_input, wrong_hash_count, tick_overflow] {
            assert_eq!(
                message(&with_record(&bytes, record)),
                "this recording is corrupted"
            );
        }
        let mut garbage = bytes.clone();
        garbage.extend_from_slice(&[0xFF; 3]);
        assert_eq!(message(&garbage), "this recording is corrupted");
    }

    #[test]
    fn a_recorded_input_can_be_sent_again() {
        let mut world = World::new();
        let press = Signals::<Press>::TYPE_UUID;
        assert!(send_recorded(&mut world, press, &encoded(Press(7))));
        assert!(!send_recorded(&mut world, press, &[]));
        assert!(!send_recorded(&mut world, 12345, &encoded(Press(7))));

        let signals = world.get_resource::<Signals<Press>>().unwrap();
        let sent: Vec<u32> = signals
            .transient_buffer
            .iter()
            .map(|x| x.payload.0)
            .collect();
        assert_eq!(sent, [7]);
    }

    #[test]
    fn a_read_recording_runs_again_to_the_same_hashes() {
        let recording = open("again", &short_run("again-run")).unwrap();
        let section = &recording.sections[0];
        let mut world = World::new();
        assert!(world.restore(&section.state).is_some());
        for recorded in &section.ticks {
            while current_tick(&world) < recorded.tick {
                advance(&mut world);
            }
            for inputs in &recorded.inputs {
                for payload in &inputs.payloads {
                    assert!(send_recorded(&mut world, inputs.signal, payload));
                }
            }
            if let Some(hashes) = &recorded.hashes {
                assert_eq!(*hashes, world.resource_hashes(), "tick {}", recorded.tick);
            }
        }
        assert_eq!(current_tick(&world), 5);
    }

    #[test]
    fn the_build_id_is_stable() {
        assert_eq!(build_id(), build_id());
    }

    #[test]
    #[should_panic(expected = "hash_every must be at least 1")]
    fn a_hash_interval_of_zero_is_refused() {
        let file = path("zero");
        let recorder = World::new().to_recorder(&file);
        fs::remove_file(&file).unwrap();
        let _ = recorder.unwrap().with_hash_every(0);
    }
}
