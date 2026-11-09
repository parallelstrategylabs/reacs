use std::{
    fs, io,
    path::{Path, PathBuf},
};

use reacs::state_hash::hash_bytes;

const MAGIC: &[u8; 8] = b"REACSSN1";
const HEADER_BYTES: usize = 40;
const EXTENSION: &str = "snap";
pub const SNAPSHOT_EVERY: u64 = 256;
pub const SNAPSHOT_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    segment: u64,
    tick: u64,
    size: u64,
}

pub(crate) struct SnapshotStore {
    dir: PathBuf,
    build_id: u64,
    entries: Vec<Entry>,
    total: u64,
}

fn parse_name(path: &Path) -> Option<(u64, u64)> {
    if path.extension()?.to_str()? != EXTENSION {
        return None;
    }
    let (segment, tick) = path.file_stem()?.to_str()?.split_once('-')?;
    Some((segment.parse().ok()?, tick.parse().ok()?))
}

fn word(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

impl SnapshotStore {
    pub(crate) fn open(dir: PathBuf, build_id: u64) -> Self {
        let mut entries = Vec::new();
        for file in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let Some((segment, tick)) = parse_name(&file.path()) else {
                continue;
            };
            let Ok(meta) = file.metadata() else {
                continue;
            };
            entries.push(Entry {
                segment,
                tick,
                size: meta.len(),
            });
        }
        entries.sort_unstable_by_key(|x| (x.segment, x.tick));
        let total = entries.iter().map(|x| x.size).sum();
        Self {
            dir,
            build_id,
            entries,
            total,
        }
    }

    fn file(&self, segment: u64, tick: u64) -> PathBuf {
        self.dir.join(format!("{segment}-{tick}.{EXTENSION}"))
    }

    fn position(&self, segment: u64, tick: u64) -> Result<usize, usize> {
        self.entries
            .binary_search_by_key(&(segment, tick), |x| (x.segment, x.tick))
    }

    pub(crate) fn contains(&self, segment: u64, tick: u64) -> bool {
        self.position(segment, tick).is_ok()
    }

    pub(crate) fn latest(&self, segment: u64, tick: u64) -> Option<u64> {
        let end = self
            .entries
            .partition_point(|x| (x.segment, x.tick) <= (segment, tick));
        let entry = self.entries[..end].last()?;
        (entry.segment == segment).then_some(entry.tick)
    }

    pub(crate) fn read(&self, segment: u64, tick: u64) -> Option<Vec<u8>> {
        let mut bytes = fs::read(self.file(segment, tick)).ok()?;
        if bytes.len() < HEADER_BYTES || &bytes[..8] != MAGIC {
            return None;
        }
        let header = [
            word(&bytes, 8),
            word(&bytes, 16),
            word(&bytes, 24),
            word(&bytes, 32),
        ];
        bytes.drain(..HEADER_BYTES);
        (header == [self.build_id, segment, tick, hash_bytes(&bytes)]).then_some(bytes)
    }

    pub(crate) fn remove(&mut self, segment: u64, tick: u64) {
        if let Ok(at) = self.position(segment, tick) {
            self.remove_at(at);
        }
    }

    fn remove_at(&mut self, at: usize) {
        let entry = self.entries.remove(at);
        self.total -= entry.size;
        let _ = fs::remove_file(self.file(entry.segment, entry.tick));
    }

    fn least_useful(&self, around: u64, starts: &[u64]) -> Option<usize> {
        let mut best: Option<(usize, u64, u64)> = None;
        for (i, entry) in self.entries.iter().enumerate() {
            let same = |other: &&Entry| other.segment == entry.segment;
            let before = i.checked_sub(1).and_then(|i| self.entries.get(i));
            let previous = before.filter(same).map_or_else(
                || {
                    let start = starts.get(entry.segment as usize);
                    start.map_or(0, |start| (*start).min(entry.tick))
                },
                |x| x.tick,
            );
            let after = self.entries.get(i + 1).filter(same);
            let gap = after.map_or(entry.tick, |x| x.tick) - previous;
            let distance = entry.tick.abs_diff(around).saturating_add(gap).max(1);

            let better = best.is_none_or(|(_, best_gap, best_distance)| {
                (gap as u128) * (best_distance as u128) < (best_gap as u128) * (distance as u128)
            });
            if better {
                best = Some((i, gap, distance));
            }
        }
        best.map(|x| x.0)
    }

    pub(crate) fn write(
        &mut self,
        segment: u64,
        tick: u64,
        state: &[u8],
        budget: u64,
        starts: &[u64],
    ) -> io::Result<bool> {
        let size = (HEADER_BYTES + state.len()) as u64;
        if size > budget {
            return Ok(false);
        }
        self.remove(segment, tick);
        while self.total + size > budget {
            let Some(at) = self.least_useful(tick, starts) else {
                break;
            };
            self.remove_at(at);
        }

        let mut bytes = Vec::with_capacity(size as usize);
        bytes.extend_from_slice(MAGIC);
        for value in [self.build_id, segment, tick, hash_bytes(state)] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(state);

        fs::create_dir_all(&self.dir)?;
        let file = self.file(segment, tick);
        let temporary = file.with_extension("tmp");
        fs::write(&temporary, &bytes)?;
        fs::rename(&temporary, &file)?;

        let at = self.position(segment, tick).unwrap_or_else(|at| at);
        self.entries.insert(
            at,
            Entry {
                segment,
                tick,
                size,
            },
        );
        self.total += size;
        Ok(true)
    }

    #[cfg(test)]
    fn ticks(&self, segment: u64) -> Vec<u64> {
        let kept = self.entries.iter().filter(|x| x.segment == segment);
        kept.map(|x| x.tick).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("reacs-snapshots-{}-{name}", std::process::id()))
    }

    fn clear(dir: &Path) {
        for file in fs::read_dir(dir).into_iter().flatten().flatten() {
            fs::remove_file(file.path()).unwrap();
        }
        let _ = fs::remove_dir(dir);
    }

    #[test]
    fn a_written_snapshot_reads_back_and_survives_reopening() {
        let dir = folder("roundtrip");
        let mut store = SnapshotStore::open(dir.clone(), 7);
        assert!(store.write(0, 64, b"state at 64", 1 << 20, &[0]).unwrap());
        assert!(
            store
                .write(1, 32, b"another run", 1 << 20, &[0, 16])
                .unwrap()
        );

        let store = SnapshotStore::open(dir.clone(), 7);
        assert_eq!(store.read(0, 64).as_deref(), Some(&b"state at 64"[..]));
        assert_eq!(store.read(1, 32).as_deref(), Some(&b"another run"[..]));
        assert_eq!(store.read(0, 32), None);
        assert_eq!(store.latest(0, 63), None);
        assert_eq!(store.latest(0, 500), Some(64));
        assert_eq!(store.latest(1, 500), Some(32));
        assert_eq!(store.latest(2, 500), None);
        clear(&dir);
    }

    #[test]
    fn a_snapshot_from_another_build_or_with_changed_bytes_is_not_read() {
        let dir = folder("damaged");
        let mut store = SnapshotStore::open(dir.clone(), 7);
        store.write(0, 64, b"state at 64", 1 << 20, &[0]).unwrap();
        store.write(0, 128, b"state at 128", 1 << 20, &[0]).unwrap();
        assert_eq!(SnapshotStore::open(dir.clone(), 8).read(0, 64), None);

        let file = store.file(0, 64);
        let mut bytes = fs::read(&file).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&file, &bytes).unwrap();
        assert_eq!(store.read(0, 64), None);

        fs::write(store.file(0, 128), b"REACSSN1 cut").unwrap();
        assert_eq!(store.read(0, 128), None);

        fs::rename(&file, store.file(0, 96)).unwrap();
        assert_eq!(SnapshotStore::open(dir.clone(), 7).read(0, 96), None);
        clear(&dir);
    }

    #[test]
    fn other_files_in_the_folder_are_left_alone() {
        let dir = folder("strangers");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("notes.txt"), b"mine").unwrap();
        fs::write(dir.join("x-y.snap"), b"mine").unwrap();

        let mut store = SnapshotStore::open(dir.clone(), 7);
        assert_eq!(store.entries, []);
        for tick in 1..=8 {
            store.write(0, tick * 10, &[0; 60], 300, &[0]).unwrap();
        }
        assert_eq!(fs::read(dir.join("notes.txt")).unwrap(), b"mine");
        assert_eq!(fs::read(dir.join("x-y.snap")).unwrap(), b"mine");
        clear(&dir);
    }

    #[test]
    fn the_budget_keeps_snapshots_dense_near_the_newest_one() {
        let dir = folder("budget");
        let mut store = SnapshotStore::open(dir.clone(), 7);
        let budget = 8 * (HEADER_BYTES as u64 + 60);
        for tick in 1..=64 {
            assert!(store.write(0, tick * 100, &[0; 60], budget, &[0]).unwrap());
            assert!(store.total <= budget);
        }

        let kept = store.ticks(0);
        assert_eq!(kept.len(), 8);
        assert_eq!(kept[5..], [6200, 6300, 6400]);
        let gaps: Vec<u64> = kept.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(gaps.windows(2).all(|pair| pair[0] >= pair[1]), "{kept:?}");
        assert!(kept[0] <= 3200, "{kept:?}");

        let reopened = SnapshotStore::open(dir.clone(), 7);
        assert_eq!(reopened.ticks(0), kept);
        assert_eq!(reopened.total, store.total);
        clear(&dir);
    }

    #[test]
    fn going_back_makes_room_near_the_tick_being_written() {
        let dir = folder("back");
        let mut store = SnapshotStore::open(dir.clone(), 7);
        let budget = 6 * (HEADER_BYTES as u64 + 60);
        for tick in 1..=40 {
            store.write(0, tick * 100, &[0; 60], budget, &[0]).unwrap();
        }
        for tick in [150, 250, 350] {
            store.write(0, tick, &[0; 60], budget, &[0]).unwrap();
        }

        let kept = store.ticks(0);
        assert_eq!(kept.len(), 6);
        assert!([150, 250, 350].iter().all(|x| kept.contains(x)), "{kept:?}");
        assert!(kept[3..].iter().all(|x| *x >= 1000), "{kept:?}");
        clear(&dir);
    }

    #[test]
    fn a_snapshot_bigger_than_the_budget_is_not_written() {
        let dir = folder("too-big");
        let mut store = SnapshotStore::open(dir.clone(), 7);
        store.write(0, 64, &[0; 10], 100, &[0]).unwrap();
        assert!(!store.write(0, 128, &[0; 200], 100, &[0]).unwrap());
        assert_eq!(store.ticks(0), [64]);
        clear(&dir);
    }
}
