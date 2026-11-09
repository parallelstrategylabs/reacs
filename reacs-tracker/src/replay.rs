use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use reacs::{
    Tick,
    registry::get_registry_entry,
    replay::{RecordedHashes, RecordedTick, Recording, RecordingError, build_id, send_recorded},
    world::World,
};

use crate::snapshot::{SNAPSHOT_BUDGET_BYTES, SNAPSHOT_EVERY, SnapshotStore};

fn current_tick(world: &World) -> u64 {
    world.get_resource::<Tick>().map_or(0, |tick| tick.0)
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

impl From<RecordingError> for ReplayError {
    fn from(error: RecordingError) -> Self {
        match error {
            RecordingError::Io(error) => ReplayError::Io(error),
            RecordingError::NotARecording => ReplayError::NotAReplay,
            RecordingError::DifferentBuild => ReplayError::DifferentBuild,
            RecordingError::Corrupted => ReplayError::Corrupted,
        }
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

fn resource_name(id: u128) -> &'static str {
    get_registry_entry(id).map_or("unregistered", |entry| entry.type_name)
}

fn differences(expected: &RecordedHashes, found: &[(u128, u64)]) -> Vec<ResourceDifference> {
    let (ids, values) = (expected.ids(), expected.values());
    let mut out = Vec::new();
    let (mut expected_at, mut found_at) = (0, 0);
    while expected_at < ids.len() || found_at < found.len() {
        let expected_id = ids.get(expected_at).copied().unwrap_or(u128::MAX);
        let found_id = found.get(found_at).map_or(u128::MAX, |hash| hash.0);
        if expected_id == found_id {
            if values[expected_at] != found[found_at].1 {
                out.push(ResourceDifference {
                    name: resource_name(expected_id),
                    expected: Some(values[expected_at]),
                    found: Some(found[found_at].1),
                });
            }
            expected_at += 1;
            found_at += 1;
        } else if expected_id < found_id {
            out.push(ResourceDifference {
                name: resource_name(expected_id),
                expected: Some(values[expected_at]),
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

#[derive(Default)]
struct Progress {
    inputs: u64,
    hashes_checked: u64,
    last_match: Option<u64>,
}

fn apply_tick(
    world: &mut World,
    recorded: &RecordedTick,
    progress: &mut Progress,
) -> Result<Option<Divergence>, ReplayError> {
    for inputs in &recorded.inputs {
        for payload in &inputs.payloads {
            if !send_recorded(world, inputs.signal, payload) {
                return Err(ReplayError::Corrupted);
            }
            progress.inputs += 1;
        }
    }
    if let Some(hashes) = &recorded.hashes {
        progress.hashes_checked += 1;
        let resources = differences(hashes, &world.resource_hashes());
        if !resources.is_empty() {
            return Ok(Some(Divergence {
                last_match: progress.last_match,
                tick: recorded.tick,
                resources,
            }));
        }
        progress.last_match = Some(recorded.tick);
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
    recording: Recording,
    snapshots: Option<SnapshotSettings>,
}

impl Replay {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ReplayError> {
        let path = path.as_ref().to_path_buf();
        let recording = Recording::open(&path)?;
        Ok(Self {
            path,
            recording,
            snapshots: None,
        })
    }

    pub fn recording(&self) -> &Recording {
        &self.recording
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
        self.recording.sections.iter().rposition(|x| x.tick <= tick)
    }

    fn recorded_until(&self, section: usize) -> u64 {
        let section = &self.recording.sections[section];
        section.ticks.last().map_or(section.tick, |x| x.tick)
    }

    pub fn start_tick(&self) -> u64 {
        let sections = self.recording.sections.iter();
        sections.map(|x| x.tick).min().unwrap()
    }

    pub fn last_tick(&self) -> u64 {
        self.recorded_until(self.recording.sections.len() - 1)
    }

    pub fn verify(&self) -> Result<ReplayReport, ReplayError> {
        let mut world = World::new();
        let mut progress = Progress::default();
        let mut report = ReplayReport {
            last_tick: self.recording.sections[0].tick,
            inputs: 0,
            hashes_checked: 0,
            divergence: None,
            truncated: self.recording.truncated,
        };

        for section in &self.recording.sections {
            world
                .restore(&section.state)
                .ok_or(ReplayError::Corrupted)?;
            report.last_tick = section.tick;
            progress.last_match = None;

            for recorded in &section.ticks {
                while current_tick(&world) < recorded.tick {
                    world.step();
                }
                report.last_tick = recorded.tick;
                report.divergence = apply_tick(&mut world, recorded, &mut progress)?;
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
        let section = self.owner(tick).ok_or(ReplayError::BeforeStart {
            tick,
            start: self.start_tick(),
        })?;
        let store = self
            .snapshots
            .map(|_| SnapshotStore::open(self.snapshot_folder(), build_id()));
        let mut player = ReplayPlayer {
            replay: self,
            world: World::new(),
            section,
            next: 0,
            progress: Progress::default(),
            store,
            started_at: 0,
            last_snapshot: 0,
            snapshots_written: 0,
            snapshot_error: None,
        };
        if !player.enter_snapshot(section, tick) {
            player.enter(section)?;
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
    section: usize,
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
            past_recording: tick > self.replay.recorded_until(self.section),
            started_at: self.started_at,
            snapshots_written: self.snapshots_written,
        }
    }

    pub fn advance(&mut self) -> Result<(), ReplayError> {
        let tick = self.tick() + 1;
        let owner = self.replay.owner(tick).unwrap_or(self.section);
        if owner != self.section {
            return self.enter(owner);
        }
        self.world.step();
        self.apply()
    }

    fn enter(&mut self, section: usize) -> Result<(), ReplayError> {
        let entered = &self.replay.recording.sections[section];
        self.world
            .restore(&entered.state)
            .ok_or(ReplayError::Corrupted)?;
        self.section = section;
        self.next = 0;
        self.progress.last_match = None;
        self.last_snapshot = entered.tick;
        self.apply()
    }

    fn enter_snapshot(&mut self, section: usize, tick: u64) -> bool {
        let ticks = &self.replay.recording.sections[section].ticks;
        loop {
            let Some(store) = &mut self.store else {
                return false;
            };
            let Some(at) = store.latest(section as u64, tick) else {
                return false;
            };

            let index = ticks.partition_point(|x| x.tick < at);
            let hashes = ticks
                .get(index)
                .filter(|recorded| recorded.tick == at)
                .and_then(|recorded| recorded.hashes.as_ref());
            let restored = hashes.is_some_and(|hashes| {
                store.read(section as u64, at).is_some_and(|state| {
                    self.world.restore(&state).is_some()
                        && differences(hashes, &self.world.resource_hashes()).is_empty()
                })
            });
            if !restored {
                store.remove(section as u64, at);
                continue;
            }

            self.section = section;
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
        let section = self.section as u64;
        if store.contains(section, tick) {
            self.last_snapshot = tick;
            return;
        }
        if tick - self.last_snapshot < settings.every {
            return;
        }

        let sections = self.replay.recording.sections.iter();
        let starts: Vec<u64> = sections.map(|x| x.tick).collect();
        match store.write(section, tick, &self.world.save(), settings.budget, &starts) {
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
        let ticks = &self.replay.recording.sections[self.section].ticks;
        while let Some(recorded) = ticks.get(self.next).filter(|x| x.tick <= tick) {
            self.next += 1;
            if recorded.tick < tick {
                continue;
            }
            if let Some(divergence) = apply_tick(&mut self.world, recorded, &mut self.progress)? {
                return Err(ReplayError::Diverged(divergence));
            }
            if recorded.hashes.is_some() {
                self.write_snapshot(tick);
            }
        }
        Ok(())
    }
}
