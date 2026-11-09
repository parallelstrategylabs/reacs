use std::{
    fs::File,
    io::{self, BufRead, BufReader, BufWriter, ErrorKind, Read, Write},
    path::Path,
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

const MAGIC: &[u8; 8] = b"REACSRC1";
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
        self.error.take().map_or_else(|| self.out.flush(), Err)
    }

    fn write_tick(&mut self, world: &World) -> io::Result<()> {
        let tick = current_tick(world);
        if self.last == Some(tick) {
            return Ok(());
        }
        if tick != self.last.map_or(self.written, |last| last + 1) {
            let state = world.save();
            let state_len = state.len() as u64;
            write(&mut self.out, &Record::Section { tick, state_len }, &state)?;
            self.written = tick;
            self.first_ids = next_ids(world);
        }
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
    pub state: Vec<u8>,
    pub ticks: Vec<RecordedTick>,
}

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
                } => {
                    let Some(section) = read_section(&mut *reader, start, state_len)? else {
                        return Ok(true);
                    };
                    tick = start;
                    self.sections.push(section);
                }
            }
        }
        Ok(false)
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

        assert_eq!(records.len(), 1);
        let Record::Tick { gap, inputs, .. } = &records[0] else {
            panic!("expected a tick record");
        };
        assert_eq!((*gap, inputs.len(), inputs[0].1.len()), (0, 1, 1));
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
    fn a_file_cut_anywhere_reads_up_to_the_cut() {
        let bytes = short_run("cut");
        let full = open("cut-full", &bytes).unwrap();
        assert!(!full.truncated);
        let (header, records) = raw_records(&bytes);
        assert_eq!(records.len(), full.sections[0].ticks.len());
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
