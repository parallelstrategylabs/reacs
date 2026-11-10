use std::{any::TypeId, marker::PhantomData, ops::RangeFull, ptr::addr_of_mut};

use reacs_derive::{param, resource};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    Tick,
    bit_set::BitSet,
    hashed::Hashed,
    registry::get_registry_entry,
    resource::Resource,
    schedule::{CurrentSystem, Mut, WorldAccess, Write},
    world::World,
};

pub const SWEEP_BUDGET: u64 = 65536;

pub trait SignalFilter<S>: 'static {
    const ACTIVE: bool;
    type Context<'w>;

    fn fetch<'w>(world: &'w World) -> Self::Context<'w>;
    fn keep(context: &Self::Context<'_>, signal: &S) -> bool;
    fn stamp(world: &World) -> u64;
}

pub struct NoFilter;

impl<S> SignalFilter<S> for NoFilter {
    const ACTIVE: bool = false;
    type Context<'w> = ();

    #[inline]
    fn fetch<'w>(_world: &'w World) {}

    #[inline]
    fn keep(_context: &(), _signal: &S) -> bool {
        true
    }

    #[inline]
    fn stamp(_world: &World) -> u64 {
        0
    }
}

pub trait SignalType: 'static + Copy + Default + Hashed + Serialize + DeserializeOwned {
    type Filter: SignalFilter<Self>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalWindow {
    pub start: u64,
    pub end: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct In(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct At(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct For(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct To(pub u64);

impl In {
    #[inline]
    fn tick(self, now: u64) -> u64 {
        now.saturating_add(self.0)
    }
}

impl At {
    #[inline]
    fn tick(self, _now: u64) -> u64 {
        self.0
    }
}

impl For {
    #[inline]
    fn end(self, start: u64) -> u64 {
        start.saturating_add(self.0)
    }
}

impl To {
    #[inline]
    fn end(self, _start: u64) -> u64 {
        self.0.saturating_add(1)
    }
}

#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a signal window",
    label = "a window is a start (`In(n)`, `At(tick)`), an end (`For(n)`, `To(tick)`), or a (start, end) pair",
    note = "for example `In(2)` (one tick, 2 from now), `For(5)`, `(In(2), For(3))`, `(At(40), To(50))`, `(In(2), ..)` or `..`"
)]
pub trait IntoWindow {
    fn into_window(self, now: u64) -> SignalWindow;
}

macro_rules! window_start {
    ($($start:ident),*) => {$(
        impl IntoWindow for $start {
            #[inline]
            fn into_window(self, now: u64) -> SignalWindow {
                let start = self.tick(now);
                SignalWindow { start, end: start.saturating_add(1) }
            }
        }

        impl IntoWindow for ($start, For) {
            #[inline]
            fn into_window(self, now: u64) -> SignalWindow {
                let start = self.0.tick(now);
                SignalWindow { start, end: self.1.end(start) }
            }
        }

        impl IntoWindow for ($start, To) {
            #[inline]
            fn into_window(self, now: u64) -> SignalWindow {
                let start = self.0.tick(now);
                SignalWindow { start, end: self.1.end(start) }
            }
        }

        impl IntoWindow for ($start, RangeFull) {
            #[inline]
            fn into_window(self, now: u64) -> SignalWindow {
                SignalWindow { start: self.0.tick(now), end: u64::MAX }
            }
        }
    )*};
}

window_start!(In, At);

macro_rules! window_end {
    ($($end:ident),*) => {$(
        impl IntoWindow for $end {
            #[inline]
            fn into_window(self, now: u64) -> SignalWindow {
                SignalWindow { start: now, end: self.end(now) }
            }
        }
    )*};
}

window_end!(For, To);

impl IntoWindow for RangeFull {
    #[inline]
    fn into_window(self, now: u64) -> SignalWindow {
        SignalWindow {
            start: now,
            end: u64::MAX,
        }
    }
}

impl SignalWindow {
    #[inline]
    pub fn contains(&self, tick: u64) -> bool {
        self.start <= tick && tick < self.end
    }

    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.start == u64::MAX
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(bound = "S: SignalType")]
pub struct SignalInstance<S> {
    pub id: u64,
    pub payload: S,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ReceiveCursor {
    pub system: u128,
    pub next: u64,
    pub windows_tick: u64,
}

#[derive(Default, Hashed, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct SignalData<S: SignalType> {
    pub cursors: Vec<ReceiveCursor>,
    pub next: u64,
    phantom: PhantomData<S>,
}

pub const WINDOW_CHUNK_TICKS: u64 = 64;
const _: () = assert!(WINDOW_CHUNK_TICKS > 0 && WINDOW_CHUNK_TICKS <= 64);

pub type WindowEntry<S> = (SignalWindow, SignalInstance<S>);

#[inline]
fn chunk_key(tick: u64) -> u64 {
    tick / WINDOW_CHUNK_TICKS
}

#[inline]
fn chunk_bit(tick: u64) -> u64 {
    1 << (tick % WINDOW_CHUNK_TICKS)
}

#[inline]
fn chunk_bits_through(tick: u64) -> u64 {
    u64::MAX >> (63 - tick % WINDOW_CHUNK_TICKS)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(bound = "S: SignalType")]
pub struct WindowChunk<S> {
    pub key: u64,
    pub ticks: u64,
    pub cancelled: u64,
    pub head: usize,
    pub by_end: bool,
    pub due: bool,
    pub entries: Vec<WindowEntry<S>>,
}

impl<S: Copy> WindowChunk<S> {
    #[inline]
    pub fn live(&self) -> &[WindowEntry<S>] {
        &self.entries[self.head..]
    }

    fn is_empty(&self) -> bool {
        self.head == self.entries.len()
    }

    fn retain_live(&mut self, mut f: impl FnMut(&WindowEntry<S>) -> bool) {
        let head = self.head;
        let mut index = 0;
        let mut last_end = 0;
        let mut by_end = true;
        let mut cancelled = 0;
        self.entries.retain(|entry| {
            let keep = index >= head && f(entry);
            index += 1;
            if keep {
                by_end &= last_end <= entry.0.end;
                last_end = entry.0.end;
                cancelled += entry.0.is_cancelled() as u64;
            }
            keep
        });
        self.head = 0;
        self.by_end = by_end;
        self.cancelled = cancelled;
    }

    fn join(&mut self, joining: &[WindowEntry<S>]) {
        let Some(first) = joining.first() else {
            return;
        };
        let appends = self.live().last().is_none_or(|last| last.1.id < first.1.id);
        if appends {
            self.by_end &= self
                .live()
                .last()
                .is_none_or(|last| last.0.end <= first.0.end)
                && joining.is_sorted_by_key(|entry| entry.0.end);
            self.entries.extend_from_slice(joining);
            return;
        }

        self.entries.drain(..self.head);
        self.head = 0;
        self.by_end = false;
        merge_by_id(&mut self.entries, joining);
    }
}

const SPARE_CHUNKS: usize = 16;

fn recycle<S>(spare: &mut Vec<Vec<WindowEntry<S>>>, mut entries: Vec<WindowEntry<S>>) {
    if spare.len() < SPARE_CHUNKS {
        entries.clear();
        spare.push(entries);
    }
}

fn chunk_mut<'a, S>(
    chunks: &'a mut Vec<WindowChunk<S>>,
    spare: &mut Vec<Vec<WindowEntry<S>>>,
    key: u64,
) -> &'a mut WindowChunk<S> {
    let i = match chunks.binary_search_by_key(&key, |chunk| chunk.key) {
        Ok(i) => i,
        Err(i) => {
            let chunk = WindowChunk {
                key,
                ticks: 0,
                cancelled: 0,
                head: 0,
                by_end: true,
                due: false,
                entries: spare.pop().unwrap_or_default(),
            };
            chunks.insert(i, chunk);
            i
        }
    };
    &mut chunks[i]
}

fn find_in_chunks<S: Copy>(chunks: &[WindowChunk<S>], id: u64) -> Option<(usize, usize)> {
    chunks.iter().enumerate().find_map(|(c, chunk)| {
        let live = chunk.live();
        let (first, last) = (live.first()?, live.last()?);
        if id < first.1.id || id > last.1.id {
            return None;
        }
        let i = live.binary_search_by(|x| x.1.id.cmp(&id)).ok()?;
        Some((c, chunk.head + i))
    })
}

fn merge_by_id<S: Copy>(entries: &mut Vec<WindowEntry<S>>, joining: &[WindowEntry<S>]) {
    let Some(first) = joining.first() else {
        return;
    };
    if entries.last().is_none_or(|last| last.1.id < first.1.id) {
        entries.extend_from_slice(joining);
        return;
    }

    let mut i = entries.len();
    let mut j = joining.len();
    entries.extend_from_slice(joining);
    let mut write = entries.len();
    while j > 0 {
        write -= 1;
        if i > 0 && entries[i - 1].1.id > joining[j - 1].1.id {
            i -= 1;
            entries[write] = entries[i];
        } else {
            j -= 1;
            entries[write] = joining[j];
        }
    }
}

enum WindowSlot {
    Pending(usize),
    Joining(usize),
    Active(usize, usize),
    Inactive(usize, usize),
}

#[derive(Default, Hashed, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Signals<S: SignalType> {
    pub transient: Vec<SignalInstance<S>>,
    pub transient_buffer: Vec<SignalInstance<S>>,

    pub transitory_pending: Vec<WindowEntry<S>>,
    pub transitory_active: Vec<WindowChunk<S>>,
    pub transitory_inactive: Vec<WindowChunk<S>>,
    pub transitory_compact: Vec<(bool, u64)>,
    pub transitory_wake: u64,

    pub permanent_joining: Vec<WindowEntry<S>>,
    #[serde(skip)]
    pub window_spare: Vec<Vec<WindowEntry<S>>>,
    #[serde(skip)]
    pub window_started: Vec<WindowEntry<S>>,
    pub permanent: Vec<SignalInstance<S>>,
    pub permanent_write: usize,
    pub permanent_read: usize,
    pub permanent_cancelled: u64,
    pub permanent_cancelled_bits: BitSet,
}

static EMPTY_BITS: BitSet = BitSet { words: Vec::new() };

#[inline(never)]
fn fold_instances<'a, S, B, F, K>(
    signals: &'a [SignalInstance<S>],
    init: B,
    f: &mut F,
    keep: &K,
) -> B
where
    F: FnMut(B, &'a S) -> B,
    K: Fn(&S) -> bool,
{
    signals.iter().fold(init, |acc, signal| {
        if keep(&signal.payload) {
            f(acc, &signal.payload)
        } else {
            acc
        }
    })
}

#[inline(never)]
fn fold_entries<'a, S, B, F, K>(
    entries: &'a [(SignalWindow, SignalInstance<S>)],
    init: B,
    f: &mut F,
    keep: &K,
) -> B
where
    F: FnMut(B, &'a S) -> B,
    K: Fn(&S) -> bool,
{
    entries.iter().fold(init, |acc, entry| {
        if !entry.0.is_cancelled() && keep(&entry.1.payload) {
            f(acc, &entry.1.payload)
        } else {
            acc
        }
    })
}

enum ReceiveStage {
    TransientBuffer,
    Active,
    PermanentLeft,
    PermanentRight,
    PermanentCancelled,
    Done,
}

struct ReceiveIter<'a, S: SignalType, K> {
    run: std::slice::Iter<'a, SignalInstance<S>>,
    entries: std::slice::Iter<'a, (SignalWindow, SignalInstance<S>)>,
    stage: ReceiveStage,
    transient_buffer: &'a [SignalInstance<S>],
    active: &'a [WindowChunk<S>],
    joining: &'a [WindowEntry<S>],
    permanent: &'a [SignalInstance<S>],
    write: usize,
    read: usize,
    position: usize,
    cancelled: Option<&'a BitSet>,
    keep: K,
}

impl<'a, S: SignalType, K: Fn(&S) -> bool> ReceiveIter<'a, S, K> {
    #[inline]
    fn next_entry(&mut self) -> Option<&'a S> {
        for entry in self.entries.by_ref() {
            if !entry.0.is_cancelled() {
                return Some(&entry.1.payload);
            }
        }
        None
    }

    #[inline]
    fn next_raw(&mut self) -> Option<&'a S> {
        if let Some(signal) = self.run.next() {
            return Some(&signal.payload);
        }
        if let Some(signal) = self.next_entry() {
            return Some(signal);
        }
        self.next_slow()
    }

    fn next_slow(&mut self) -> Option<&'a S> {
        loop {
            if let Some(signal) = self.run.next() {
                return Some(&signal.payload);
            }
            if let Some(signal) = self.next_entry() {
                return Some(signal);
            }

            match self.stage {
                ReceiveStage::TransientBuffer => {
                    self.run = self.transient_buffer.iter();
                    self.stage = ReceiveStage::Active;
                }
                ReceiveStage::Active => {
                    if let Some((chunk, rest)) = self.active.split_first() {
                        self.entries = chunk.live().iter();
                        self.active = rest;
                        continue;
                    }
                    self.entries = self.joining.iter();
                    self.stage = if self.cancelled.is_some() {
                        ReceiveStage::PermanentCancelled
                    } else {
                        ReceiveStage::PermanentLeft
                    };
                }
                ReceiveStage::PermanentLeft => {
                    self.run = self.permanent[..self.write].iter();
                    self.stage = ReceiveStage::PermanentRight;
                }
                ReceiveStage::PermanentRight => {
                    self.run = self.permanent[self.read..].iter();
                    self.stage = ReceiveStage::Done;
                }
                ReceiveStage::PermanentCancelled => {
                    if self.position == self.write {
                        self.position = self.read;
                    }
                    if self.position >= self.permanent.len() {
                        self.stage = ReceiveStage::Done;
                        continue;
                    }

                    let limit = if self.position < self.write {
                        self.write
                    } else {
                        self.permanent.len()
                    };
                    let next = self
                        .cancelled
                        .map_or(limit, |bits| bits.next_set(self.position, limit));
                    self.run = self.permanent[self.position..next].iter();
                    self.position = (next + 1).min(limit);
                }
                ReceiveStage::Done => return None,
            }
        }
    }
}

impl<'a, S: SignalType, K: Fn(&S) -> bool> Iterator for ReceiveIter<'a, S, K> {
    type Item = &'a S;

    #[inline]
    fn next(&mut self) -> Option<&'a S> {
        if !S::Filter::ACTIVE {
            return self.next_raw();
        }
        loop {
            let signal = self.next_raw()?;
            if (self.keep)(signal) {
                return Some(signal);
            }
        }
    }

    fn fold<B, F>(mut self, init: B, mut f: F) -> B
    where
        F: FnMut(B, &'a S) -> B,
    {
        let mut acc = fold_instances(self.run.as_slice(), init, &mut f, &self.keep);
        acc = fold_entries(self.entries.as_slice(), acc, &mut f, &self.keep);

        loop {
            match self.stage {
                ReceiveStage::TransientBuffer => {
                    acc = fold_instances(self.transient_buffer, acc, &mut f, &self.keep);
                    self.stage = ReceiveStage::Active;
                }
                ReceiveStage::Active => {
                    for chunk in self.active {
                        acc = fold_entries(chunk.live(), acc, &mut f, &self.keep);
                    }
                    acc = fold_entries(self.joining, acc, &mut f, &self.keep);
                    self.stage = if self.cancelled.is_some() {
                        ReceiveStage::PermanentCancelled
                    } else {
                        ReceiveStage::PermanentLeft
                    };
                }
                ReceiveStage::PermanentLeft => {
                    acc = fold_instances(&self.permanent[..self.write], acc, &mut f, &self.keep);
                    self.stage = ReceiveStage::PermanentRight;
                }
                ReceiveStage::PermanentRight => {
                    return fold_instances(&self.permanent[self.read..], acc, &mut f, &self.keep);
                }
                ReceiveStage::PermanentCancelled => {
                    let bits = self.cancelled.unwrap_or(&EMPTY_BITS);
                    let mut position = self.position;
                    loop {
                        if position == self.write {
                            position = self.read;
                        }
                        if position >= self.permanent.len() {
                            return acc;
                        }
                        let limit = if position < self.write {
                            self.write
                        } else {
                            self.permanent.len()
                        };
                        let next = bits.next_set(position, limit);
                        acc = fold_instances(
                            &self.permanent[position..next],
                            acc,
                            &mut f,
                            &self.keep,
                        );
                        position = (next + 1).min(limit);
                    }
                }
                ReceiveStage::Done => return acc,
            }
        }
    }
}

impl<S: SignalType> Signals<S> {
    pub fn send(
        &mut self,
        data: &mut SignalData<S>,
        tick: u64,
        signal: S,
        window: SignalWindow,
    ) -> Option<u64> {
        if window.end <= window.start || window.end <= tick {
            return None;
        }

        let instance = SignalInstance {
            id: data.next,
            payload: signal,
        };
        data.next += 1;

        if window.start <= tick {
            self.transient_buffer.push(instance);
        }

        if window.end > tick + 1 {
            self.transitory_pending.push((window, instance));
        }

        Some(instance.id)
    }

    #[inline]
    pub fn emit(&mut self, data: &mut SignalData<S>, signal: S) -> u64 {
        let id = data.next;
        data.next += 1;
        self.transient_buffer.push(SignalInstance {
            id,
            payload: signal,
        });
        id
    }

    fn find_windowed(&self, id: u64) -> Option<WindowSlot> {
        let search = |list: &[WindowEntry<S>]| list.binary_search_by(|x| x.1.id.cmp(&id)).ok();
        search(&self.transitory_pending)
            .map(WindowSlot::Pending)
            .or_else(|| search(&self.permanent_joining).map(WindowSlot::Joining))
            .or_else(|| {
                let (c, i) = find_in_chunks(&self.transitory_active, id)?;
                Some(WindowSlot::Active(c, i))
            })
            .or_else(|| {
                let (c, i) = find_in_chunks(&self.transitory_inactive, id)?;
                Some(WindowSlot::Inactive(c, i))
            })
    }

    fn windowed_mut(&mut self, slot: &WindowSlot) -> &mut WindowEntry<S> {
        match *slot {
            WindowSlot::Pending(i) => &mut self.transitory_pending[i],
            WindowSlot::Joining(i) => &mut self.permanent_joining[i],
            WindowSlot::Active(c, i) => &mut self.transitory_active[c].entries[i],
            WindowSlot::Inactive(c, i) => &mut self.transitory_inactive[c].entries[i],
        }
    }

    fn find_permanent(&self, id: u64) -> Option<usize> {
        let search = |list: &[SignalInstance<S>]| list.binary_search_by(|x| x.id.cmp(&id)).ok();
        search(&self.permanent[..self.permanent_write])
            .or_else(|| Some(search(&self.permanent[self.permanent_read..])? + self.permanent_read))
    }

    pub fn modify(&mut self, id: u64, f: impl FnOnce(&mut S)) -> bool {
        if let Some(i) = self.find_permanent(id) {
            if self.permanent_cancelled_bits.get(i) {
                return false;
            }
            f(&mut self.permanent[i].payload);
            return true;
        }

        if let Some(slot) = self.find_windowed(id) {
            let entry = self.windowed_mut(&slot);
            if entry.0.is_cancelled() {
                return false;
            }
            f(&mut entry.1.payload);
            let payload = entry.1.payload;
            for list in [&mut self.transient, &mut self.transient_buffer] {
                if let Ok(i) = list.binary_search_by(|x| x.id.cmp(&id)) {
                    list[i].payload = payload;
                }
            }
            return true;
        }

        for list in [&mut self.transient, &mut self.transient_buffer] {
            if let Ok(i) = list.binary_search_by(|x| x.id.cmp(&id)) {
                f(&mut list[i].payload);
                return true;
            }
        }
        false
    }

    pub fn cancel(&mut self, id: u64) -> bool {
        if let Some(i) = self.find_permanent(id) {
            if self.permanent_cancelled_bits.get(i) {
                return false;
            }
            self.permanent_cancelled_bits.set(i, true);
            self.permanent_cancelled += 1;
            return true;
        }

        let mut found = false;
        if let Some(slot) = self.find_windowed(id) {
            let window = &mut self.windowed_mut(&slot).0;
            if window.is_cancelled() {
                return false;
            }
            window.start = u64::MAX;
            found = true;

            if let WindowSlot::Active(c, _) | WindowSlot::Inactive(c, _) = slot {
                let active = matches!(slot, WindowSlot::Active(..));
                let chunks = match active {
                    true => &mut self.transitory_active,
                    false => &mut self.transitory_inactive,
                };
                let chunk = &mut chunks[c];
                chunk.cancelled += 1;
                if !chunk.due && chunk.cancelled * 2 > chunk.live().len() as u64 {
                    chunk.due = true;
                    self.transitory_compact.push((active, chunk.key));
                }
            }
        }

        for list in [&mut self.transient, &mut self.transient_buffer] {
            if let Ok(i) = list.binary_search_by(|x| x.id.cmp(&id)) {
                list.remove(i);
                return true;
            }
        }

        found
    }

    #[inline(always)]
    pub fn receive<'a, K: Fn(&S) -> bool>(
        &'a self,
        data: &mut SignalData<S>,
        system: u128,
        tick: u64,
        keep: K,
    ) -> impl Iterator<Item = &'a S> + use<'a, S, K> {
        let slot = data.cursors.binary_search_by_key(&system, |x| x.system);
        let (cursor, windows) = match slot {
            Ok(i) => (data.cursors[i].next, data.cursors[i].windows_tick != tick),
            Err(_) => (0, true),
        };
        let unread =
            |list: &'a [SignalInstance<S>]| &list[list.partition_point(|x| x.id < cursor)..];

        let transient = unread(&self.transient);
        let transient_buffer = unread(&self.transient_buffer);

        let next = transient_buffer
            .last()
            .or_else(|| transient.last())
            .map_or(cursor, |last| last.id + 1);
        match slot {
            Ok(i) => {
                data.cursors[i].next = next;
                data.cursors[i].windows_tick = tick;
            }
            Err(i) => data.cursors.insert(
                i,
                ReceiveCursor {
                    system,
                    next,
                    windows_tick: tick,
                },
            ),
        }

        let mut iter = ReceiveIter {
            run: transient.iter(),
            stage: ReceiveStage::TransientBuffer,
            transient_buffer,
            entries: [].iter(),
            active: &self.transitory_active,
            joining: &self.permanent_joining,
            permanent: &self.permanent,
            write: self.permanent_write,
            read: self.permanent_read,
            position: 0,
            cancelled: (self.permanent_cancelled > 0).then_some(&self.permanent_cancelled_bits),
            keep,
        };
        if !windows {
            iter.active = &[];
            iter.joining = &[];
            iter.permanent = &[];
            iter.write = 0;
            iter.read = 0;
            iter.cancelled = None;
        }
        iter
    }

    pub(crate) fn sweep(&mut self, budget: u64, mut keep: impl FnMut(&S) -> bool) -> (u64, bool) {
        let mut used = 0;

        while used < budget && self.permanent_read < self.permanent.len() {
            let read = self.permanent_read;
            let signal = self.permanent[read];
            self.permanent_read += 1;
            used += 1;

            if self.permanent_cancelled > 0 && self.permanent_cancelled_bits.get(read) {
                self.permanent_cancelled_bits.set(read, false);
                self.permanent_cancelled -= 1;
            } else if keep(&signal.payload) {
                self.permanent[self.permanent_write] = signal;
                self.permanent_write += 1;
            }
        }

        let finished = self.permanent_read == self.permanent.len();
        if finished {
            self.permanent.truncate(self.permanent_write);
            self.permanent_write = 0;
            self.permanent_read = 0;
        }

        (used, finished)
    }

    fn merge_late_permanent(&mut self, first_new: usize) {
        let joined = self.permanent[first_new..].to_vec();
        let mut end = first_new;

        for (i, signal) in joined.iter().enumerate().rev() {
            let read = self.permanent_read;
            let start = read + self.permanent[read..end].partition_point(|x| x.id < signal.id);
            let shift = i + 1;

            self.shift_cancelled_bits(start, end, shift);
            self.permanent.copy_within(start..end, start + shift);
            self.permanent[start + i] = *signal;
            end = start;
        }
    }

    fn shift_cancelled_bits(&mut self, start: usize, end: usize, shift: usize) {
        if self.permanent_cancelled == 0 {
            return;
        }

        let bits = &mut self.permanent_cancelled_bits;
        let mut moved = Vec::new();
        let mut position = bits.next_set(start, end);
        while position < end {
            moved.push(position);
            position = bits.next_set(position + 1, end);
        }

        for &position in &moved {
            bits.set(position, false);
        }
        for &position in &moved {
            bits.set(position + shift, true);
        }
    }

    fn drop_transient_copies(&mut self) {
        let pending = &self.transitory_pending;
        if pending.is_empty() || self.transient.is_empty() {
            return;
        }

        let mut p = 0;
        self.transient.retain(|signal| {
            while p < pending.len() && pending[p].1.id < signal.id {
                p += 1;
            }
            p == pending.len() || pending[p].1.id != signal.id
        });
    }

    fn expire_active(&mut self, next_tick: u64) {
        let key = chunk_key(next_tick);
        let expired = self
            .transitory_active
            .partition_point(|chunk| chunk.key < key);
        for chunk in self.transitory_active.drain(..expired) {
            recycle(&mut self.window_spare, chunk.entries);
        }

        let Some(chunk) = self.transitory_active.first_mut() else {
            return;
        };
        let through = chunk_bits_through(next_tick);
        if chunk.key != key || chunk.ticks & through == 0 {
            return;
        }

        if chunk.by_end {
            let ended = chunk
                .live()
                .partition_point(|entry| entry.0.end <= next_tick);
            if chunk.cancelled > 0 {
                let live = chunk.live();
                chunk.cancelled -= live[..ended]
                    .iter()
                    .filter(|entry| entry.0.is_cancelled())
                    .count() as u64;
            }
            chunk.head += ended;
        } else {
            chunk.retain_live(|entry| entry.0.end > next_tick);
        }
        chunk.ticks &= !through;
        if chunk.is_empty() {
            let chunk = self.transitory_active.remove(0);
            recycle(&mut self.window_spare, chunk.entries);
        }
    }

    fn compact_cancelled(&mut self) {
        for (active, key) in std::mem::take(&mut self.transitory_compact) {
            let chunks = match active {
                true => &mut self.transitory_active,
                false => &mut self.transitory_inactive,
            };
            let Ok(c) = chunks.binary_search_by_key(&key, |chunk| chunk.key) else {
                continue;
            };
            let chunk = &mut chunks[c];
            if !chunk.due {
                continue;
            }
            chunk.due = false;
            chunk.retain_live(|entry| !entry.0.is_cancelled());
            if chunk.is_empty() {
                let chunk = chunks.remove(c);
                recycle(&mut self.window_spare, chunk.entries);
            }
        }
    }

    fn start_inactive(&mut self, next_tick: u64, started: &mut Vec<WindowEntry<S>>) {
        let key = chunk_key(next_tick);
        let through = chunk_bits_through(next_tick);

        while let Some(chunk) = self.transitory_inactive.first_mut() {
            if chunk.key > key || (chunk.key == key && chunk.ticks & through == 0) {
                break;
            }

            chunk.retain_live(|entry| {
                if entry.0.is_cancelled() || entry.0.end <= next_tick {
                    return false;
                }
                if entry.0.start <= next_tick {
                    started.push(*entry);
                    return false;
                }
                true
            });
            chunk.ticks &= !through;

            if !chunk.is_empty() {
                break;
            }
            let chunk = self.transitory_inactive.remove(0);
            recycle(&mut self.window_spare, chunk.entries);
        }
    }

    fn place_pending(&mut self, next_tick: u64, started: &mut Vec<WindowEntry<S>>) {
        for entry in self.transitory_pending.drain(..) {
            let window = entry.0;
            if window.is_cancelled() {
                continue;
            }
            if window.start <= next_tick {
                started.push(entry);
                continue;
            }

            let key = chunk_key(window.start);
            let chunk = chunk_mut(&mut self.transitory_inactive, &mut self.window_spare, key);
            chunk.ticks |= chunk_bit(window.start);
            chunk.entries.push(entry);
        }
    }

    fn place_started(&mut self, started: &mut [WindowEntry<S>]) {
        let order = |entry: &WindowEntry<S>| (chunk_key(entry.0.end), entry.1.id);
        if !started.is_sorted_by_key(order) {
            started.sort_unstable_by_key(order);
        }

        for group in started.chunk_by(|a, b| chunk_key(a.0.end) == chunk_key(b.0.end)) {
            let key = chunk_key(group[0].0.end);
            let chunk = chunk_mut(&mut self.transitory_active, &mut self.window_spare, key);
            for entry in group {
                chunk.ticks |= chunk_bit(entry.0.end);
            }
            chunk.join(group);
        }
    }

    fn first_bit_tick(chunks: &[WindowChunk<S>]) -> u64 {
        chunks.first().map_or(u64::MAX, |chunk| {
            let offset = (chunk.ticks.trailing_zeros() as u64).min(WINDOW_CHUNK_TICKS);
            (chunk.key * WINDOW_CHUNK_TICKS).saturating_add(offset)
        })
    }

    pub fn cleanup(&mut self, tick: u64, keep: impl Fn(&S) -> bool) {
        std::mem::swap(&mut self.transient, &mut self.transient_buffer);
        self.transient_buffer.clear();

        let next_tick = tick + 1;
        let windows_idle = self.transitory_pending.is_empty()
            && self.transitory_compact.is_empty()
            && self.permanent_joining.is_empty()
            && next_tick < self.transitory_wake;
        if !windows_idle {
            self.cleanup_windows(next_tick, keep);
        }

        if !S::Filter::ACTIVE && (self.permanent_cancelled > 0 || self.permanent_read > 0) {
            self.sweep(SWEEP_BUDGET, |_| true);
        }
    }

    fn cleanup_windows(&mut self, next_tick: u64, keep: impl Fn(&S) -> bool) {
        self.drop_transient_copies();
        if !self.transitory_compact.is_empty() {
            self.compact_cancelled();
        }
        self.expire_active(next_tick);

        let mut started = std::mem::take(&mut self.window_started);
        self.start_inactive(next_tick, &mut started);
        self.place_pending(next_tick, &mut started);

        let mut open = Vec::new();
        started.retain(|entry| {
            if entry.0.end == u64::MAX {
                open.push(*entry);
                return false;
            }
            true
        });
        self.place_started(&mut started);
        started.clear();
        self.window_started = started;

        let first_new = self.permanent.len();
        let newest = self.permanent.last().map(|x| x.id);
        let newest_checked = self.permanent[..self.permanent_write].last().map(|x| x.id);
        let behind_sweep = |id: u64| newest_checked.is_some_and(|checked| id < checked);

        let permanent = &mut self.permanent;
        self.permanent_joining.retain(|&(window, signal)| {
            if window.is_cancelled() || !keep(&signal.payload) {
                return false;
            }
            if behind_sweep(signal.id) {
                return true;
            }
            permanent.push(signal);
            false
        });
        if !open.is_sorted_by_key(|entry| entry.1.id) {
            open.sort_unstable_by_key(|entry| entry.1.id);
        }
        merge_by_id(&mut self.permanent_joining, &open);

        let joined_late = self
            .permanent
            .get(first_new)
            .zip(newest)
            .is_some_and(|(joined, newest)| joined.id < newest);
        if joined_late {
            self.merge_late_permanent(first_new);
        }

        self.transitory_wake = Self::first_bit_tick(&self.transitory_active)
            .min(Self::first_bit_tick(&self.transitory_inactive));
    }
}

#[param(write)]
pub struct Signal<S: SignalType> {
    pub signals: Resource<Signals<S>>,
    pub data: Resource<SignalData<S>>,
    #[read]
    pub tick: Resource<Tick>,
    #[read]
    pub current: Resource<CurrentSystem>,
    #[read]
    pub world: WorldAccess,
}

impl<'w, S: SignalType> Signal<'w, S, Write> {
    #[inline]
    pub fn send(&mut self, signal: S, window: impl IntoWindow) -> Option<u64> {
        let tick = self.tick();
        self.signals
            .send(&mut self.data, tick, signal, window.into_window(tick))
    }

    #[inline]
    pub fn emit(&mut self, signal: S) -> u64 {
        self.signals.emit(&mut self.data, signal)
    }

    #[inline]
    pub fn modify(&mut self, id: u64, payload: S) -> bool {
        self.signals.modify(id, |current| *current = payload)
    }

    #[inline]
    pub fn cancel(&mut self, id: u64) -> bool {
        self.signals.cancel(id)
    }

    #[inline]
    pub fn receive(&mut self) -> impl Iterator<Item = &S> {
        let context = S::Filter::fetch(&self.world);
        self.signals.receive(
            &mut self.data,
            (*self.current).0,
            (*self.tick).0,
            move |signal| S::Filter::keep(&context, signal),
        )
    }

    #[inline]
    pub fn tick(&self) -> u64 {
        (*self.tick).0
    }
}

pub(crate) static mut SIGNAL_REGISTRY: Option<Vec<fn(&World)>> = None;

pub(crate) static mut SWEEP_REGISTRY: Option<Vec<(u128, fn(&World, u64) -> (u64, bool))>> = None;

pub(crate) static mut SWEEP_STAMPS: Option<Vec<(TypeId, fn(&World) -> u64)>> = None;

#[resource]
pub struct SweepCursor {
    pub type_index: usize,
    pub running: bool,
    pub woken: bool,
    pub seen_stamp: u64,
}

pub fn register_signal<S: SignalType>() {
    unsafe {
        let ptr = addr_of_mut!(SIGNAL_REGISTRY);
        (*ptr)
            .get_or_insert_with(Vec::new)
            .push(cleanup_in_world::<S>);

        if S::Filter::ACTIVE {
            let ptr = addr_of_mut!(SWEEP_REGISTRY);
            let sweeps = (*ptr).get_or_insert_with(Vec::new);
            let uuid = Signals::<S>::TYPE_UUID;
            let i = sweeps.partition_point(|x| x.0 < uuid);
            sweeps.insert(i, (uuid, sweep_in_world::<S>));

            let ptr = addr_of_mut!(SWEEP_STAMPS);
            let stamps = (*ptr).get_or_insert_with(Vec::new);
            let filter = TypeId::of::<S::Filter>();
            if !stamps.iter().any(|x| x.0 == filter) {
                stamps.push((filter, S::Filter::stamp));
            }
        }
    }
}

#[diagnostic::on_unimplemented(
    message = "`{Self}` is not an input signal",
    note = "declare it with #[signal(input)]; other signals are sent from systems or World::build"
)]
pub trait InputSignal: SignalType {}

fn get_registered_sweeps() -> &'static [(u128, fn(&World, u64) -> (u64, bool))] {
    unsafe {
        let ptr = addr_of_mut!(SWEEP_REGISTRY);
        (*ptr).as_deref().unwrap_or(&[])
    }
}

fn get_registered_stamps() -> &'static [(TypeId, fn(&World) -> u64)] {
    unsafe {
        let ptr = addr_of_mut!(SWEEP_STAMPS);
        (*ptr).as_deref().unwrap_or(&[])
    }
}

fn sweep_in_world<S: SignalType>(world: &World, budget: u64) -> (u64, bool) {
    let Some(mut signals) = world.get_resource_mut::<Signals<S>>() else {
        return (0, true);
    };
    let context = S::Filter::fetch(world);
    signals.sweep(budget, |signal| S::Filter::keep(&context, signal))
}

fn sweep_signals(world: &World) {
    let sweeps = get_registered_sweeps();
    if sweeps.is_empty() {
        return;
    }
    let Some(mut cursor) = world.get_resource_mut::<SweepCursor>() else {
        return;
    };
    let stamp: u64 = get_registered_stamps()
        .iter()
        .map(|(_, stamp)| stamp(world))
        .sum();

    if !cursor.running {
        if !cursor.woken && stamp == cursor.seen_stamp {
            return;
        }
        cursor.running = true;
        cursor.woken = false;
        cursor.seen_stamp = stamp;
        cursor.type_index = 0;
    }

    let mut budget = SWEEP_BUDGET;
    while budget > 0 && cursor.running {
        budget -= 1;

        let (used, finished) = (sweeps[cursor.type_index].1)(world, budget);
        budget -= used;

        if finished {
            cursor.type_index += 1;
            if cursor.type_index == sweeps.len() {
                cursor.type_index = 0;
                cursor.running = false;
            }
        }
    }
}

fn get_registered_signals() -> &'static [fn(&World)] {
    unsafe {
        let ptr = addr_of_mut!(SIGNAL_REGISTRY);
        (*ptr).as_deref().unwrap_or(&[])
    }
}

fn cleanup_in_world<S: SignalType>(world: &World) {
    let (Some(mut signals), Some(tick)) = (
        world.get_resource_mut::<Signals<S>>(),
        world.get_resource::<Tick>(),
    ) else {
        return;
    };
    let context = (!signals.permanent_joining.is_empty()).then(|| S::Filter::fetch(world));
    signals.cleanup(tick.0, |signal| {
        context
            .as_ref()
            .is_none_or(|context| S::Filter::keep(context, signal))
    });

    if S::Filter::ACTIVE && signals.permanent_cancelled > 0 {
        if let Some(mut cursor) = world.get_resource_mut::<SweepCursor>() {
            cursor.woken = true;
        }
    }
}

impl World {
    #[inline]
    pub(crate) fn init_signal<S: SignalType>(&mut self) {
        get_registry_entry(Signals::<S>::TYPE_UUID).unwrap_or_else(|| {
            let type_name = std::any::type_name::<S>();
            panic!("Signal `{type_name}` not registered, try using #[signal] {type_name}!")
        });

        self.init_resource::<Signals<S>>();
        self.init_resource::<SignalData<S>>();
        self.init_resource::<Tick>();
    }

    #[inline]
    pub(crate) fn signal<S: SignalType>(&self) -> Mut<Signal<'_, S>> {
        self.fetch_param::<Mut<Signal<S>>>()
    }

    #[inline]
    pub fn input<S: InputSignal>(&mut self, signal: S) {
        self.signal::<S>().emit(signal);
    }

    #[inline]
    pub(crate) fn cleanup_signals(&self) {
        sweep_signals(self);

        for cleanup in get_registered_signals() {
            cleanup(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{IntoWindow, SWEEP_BUDGET, SignalData, Signals, SweepCursor};
    use crate::{
        Tick, entity::Entity, prelude::*, registry::get_registry_entry, schedule::CurrentSystem,
    };

    #[signal]
    pub struct TestSignal(pub Entity);

    #[signal]
    #[derive(Debug, PartialEq)]
    pub struct NumSignal(pub u64);

    #[system]
    pub fn example_system(_signal: Mut<Signal<TestSignal>>) {}

    fn world() -> World {
        let mut world = World::new();
        world.init_signal::<NumSignal>();
        world
    }

    fn end_tick(world: &World) {
        world.cleanup_signals();
        world.get_resource_mut::<Tick>().unwrap().0 += 1;
    }

    fn receive(world: &World) -> Vec<u64> {
        world.signal::<NumSignal>().receive().map(|s| s.0).collect()
    }

    fn peek(world: &World) -> Vec<u64> {
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        let mut data = world.get_resource_mut::<SignalData<NumSignal>>().unwrap();
        let tick = world.get_resource::<Tick>().unwrap().0;
        let fresh = u128::MAX - data.cursors.len() as u128;
        signals
            .receive(&mut data, fresh, tick, |_| true)
            .map(|s| s.0)
            .collect()
    }

    fn run_ticks(world: &World, ticks: u64) -> Vec<Vec<u64>> {
        (0..ticks)
            .map(|_| {
                let r = receive(world);
                end_tick(&world);
                r
            })
            .collect()
    }

    #[test]
    fn signal_registers_storage() {
        assert!(get_registry_entry(Signals::<TestSignal>::TYPE_UUID).is_some());
        assert!(get_registry_entry(SignalData::<TestSignal>::TYPE_UUID).is_some());
    }

    #[test]
    fn system_param_initializes_signal() {
        let world = World::new();
        assert!(world.has_resource::<Signals<TestSignal>>());
        assert!(world.has_resource::<SignalData<TestSignal>>());
    }

    #[test]
    fn emit_is_received_same_tick() {
        let world = world();
        world.signal::<NumSignal>().emit(NumSignal(7));
        assert_eq!(receive(&world), vec![7]);
    }

    #[test]
    fn signal_is_received_once_per_tick() {
        let world = world();
        world.signal::<NumSignal>().emit(NumSignal(1));
        assert_eq!(receive(&world), vec![1]);
        assert_eq!(receive(&world), Vec::<u64>::new());

        end_tick(&world);
        assert_eq!(receive(&world), Vec::<u64>::new());
    }

    #[test]
    fn transient_survives_one_tick_for_late_receivers() {
        let world = world();
        assert_eq!(receive(&world), Vec::<u64>::new());
        world.signal::<NumSignal>().emit(NumSignal(3));
        end_tick(&world);

        assert_eq!(receive(&world), vec![3]);
    }

    #[test]
    fn transient_expires_after_double_buffer() {
        let world = world();
        world.signal::<NumSignal>().emit(NumSignal(3));
        end_tick(&world);
        end_tick(&world);

        assert_eq!(receive(&world), Vec::<u64>::new());
    }

    #[test]
    fn ids_are_monotonic() {
        let world = world();
        let mut s = world.signal::<NumSignal>();
        assert_eq!(s.emit(NumSignal(0)), 0);
        assert_eq!(s.send(NumSignal(0), (At(0), To(4))), Some(1));
        assert_eq!(s.send(NumSignal(0), (At(3), To(4))), Some(2));
    }

    #[test]
    fn future_window_is_delivered_exactly_inside_window() {
        let world = world();
        world
            .signal::<NumSignal>()
            .send(NumSignal(9), (At(2), To(3)));

        assert_eq!(
            run_ticks(&world, 6),
            vec![vec![], vec![], vec![9], vec![9], vec![], vec![]]
        );
    }

    #[test]
    fn window_starting_now_is_delivered_once_per_tick() {
        let world = world();
        world
            .signal::<NumSignal>()
            .send(NumSignal(5), (At(0), To(2)));

        assert_eq!(
            run_ticks(&world, 4),
            vec![vec![5], vec![5], vec![5], vec![]]
        );
    }

    #[test]
    fn inclusive_window() {
        let world = world();
        world
            .signal::<NumSignal>()
            .send(NumSignal(5), (At(1), To(2)));

        assert_eq!(run_ticks(&world, 4), vec![vec![], vec![5], vec![5], vec![]]);
    }

    #[test]
    fn windows_resolve_against_the_current_tick() {
        fn window(window: impl IntoWindow) -> (u64, u64) {
            let w = window.into_window(10);
            (w.start, w.end)
        }
        const MAX: u64 = u64::MAX;

        assert_eq!(window(In(0)), (10, 11));
        assert_eq!(window(In(2)), (12, 13));
        assert_eq!(window(At(4)), (4, 5));
        assert_eq!(window(For(5)), (10, 15));
        assert_eq!(window(To(12)), (10, 13));
        assert_eq!(window((In(2), For(3))), (12, 15));
        assert_eq!(window((In(2), To(20))), (12, 21));
        assert_eq!(window((At(40), For(10))), (40, 50));
        assert_eq!(window((At(40), To(50))), (40, 51));
        assert_eq!(window((In(2), ..)), (12, MAX));
        assert_eq!(window((At(40), ..)), (40, MAX));
        assert_eq!(window(..), (10, MAX));
        assert_eq!(window(For(MAX)), window(..));
        assert_eq!(window((In(2), For(MAX))), window((In(2), ..)));
        assert_eq!(window(To(MAX)), window(..));
        assert_eq!(window(In(MAX)), (MAX, MAX));
        assert_eq!(window((At(MAX), ..)), (MAX, MAX));
    }

    #[test]
    fn windows_are_delivered_relative_to_the_send_tick() {
        let world = world();
        run_ticks(&world, 3);
        {
            let mut s = world.signal::<NumSignal>();
            s.send(NumSignal(1), For(2));
            s.send(NumSignal(2), (In(2), For(2)));
            s.send(NumSignal(3), ..);
            s.send(NumSignal(4), At(4));
            s.send(NumSignal(5), (In(5), ..));
            s.send(NumSignal(6), In(0));
            s.send(NumSignal(7), In(3));
            s.send(NumSignal(8), To(4));
            s.send(NumSignal(9), (At(7), To(8)));
            s.send(NumSignal(10), For(u64::MAX));
        }

        let mut ticks = run_ticks(&world, 7);
        ticks.iter_mut().for_each(|t| t.sort_unstable());
        assert_eq!(
            ticks,
            vec![
                vec![1, 3, 6, 8, 10],
                vec![1, 3, 4, 8, 10],
                vec![2, 3, 10],
                vec![2, 3, 7, 10],
                vec![3, 9, 10],
                vec![3, 5, 9, 10],
                vec![3, 5, 10],
            ]
        );
    }

    #[test]
    fn ended_or_empty_windows_are_rejected() {
        let world = world();
        run_ticks(&world, 3);

        let mut s = world.signal::<NumSignal>();
        assert_eq!(s.send(NumSignal(0), For(0)), None);
        assert_eq!(s.send(NumSignal(0), (In(2), For(0))), None);
        assert_eq!(s.send(NumSignal(0), To(2)), None);
        assert_eq!(s.send(NumSignal(0), At(2)), None);
        assert_eq!(s.send(NumSignal(0), (At(1), To(2))), None);
        assert_eq!(s.send(NumSignal(0), (At(1), For(2))), None);
        assert_eq!(s.send(NumSignal(0), (In(5), To(6))), None);
        assert_eq!(s.send(NumSignal(0), In(u64::MAX)), None);
        assert_eq!(s.send(NumSignal(0), At(u64::MAX)), None);
        assert_eq!(s.send(NumSignal(0), (In(u64::MAX), ..)), None);
        assert_eq!(s.send(NumSignal(0), (At(u64::MAX), To(u64::MAX))), None);
        assert_eq!(s.send(NumSignal(0), (At(u64::MAX), For(5))), None);
        assert_eq!(s.send(NumSignal(0), At(3)), Some(0));
    }

    #[test]
    fn unbounded_window_never_expires() {
        let world = world();
        world.signal::<NumSignal>().send(NumSignal(5), (At(2), ..));

        let ticks = run_ticks(&world, 50);
        assert!(ticks[..2].iter().all(|t| t.is_empty()));
        assert!(ticks[2..].iter().all(|t| t == &vec![5]));
    }

    #[test]
    fn past_or_empty_window_is_rejected() {
        let world = world();
        run_ticks(&world, 5);

        let mut s = world.signal::<NumSignal>();
        assert_eq!(s.tick(), 5);
        assert_eq!(s.send(NumSignal(0), (At(0), To(4))), None);
        assert_eq!(s.send(NumSignal(0), (At(7), To(6))), None);
        assert_eq!(s.send(NumSignal(0), (At(8), To(6))), None);
        assert_eq!(s.send(NumSignal(0), (At(0), To(5))), Some(0));
    }

    #[test]
    fn modify_changes_transitory_payload() {
        let world = world();
        let id = world
            .signal::<NumSignal>()
            .send(NumSignal(1), (At(1), To(2)))
            .unwrap();

        assert!(world.signal::<NumSignal>().modify(id, NumSignal(2)));
        end_tick(&world);
        assert_eq!(receive(&world), vec![2]);

        assert!(world.signal::<NumSignal>().modify(id, NumSignal(3)));
        assert_eq!(peek(&world), vec![3]);

        end_tick(&world);
        end_tick(&world);
        assert!(!world.signal::<NumSignal>().modify(id, NumSignal(0)));
    }

    #[test]
    fn merge_keeps_transitory_sorted_by_id() {
        let world = world();
        {
            let mut s = world.signal::<NumSignal>();
            s.send(NumSignal(0), (At(1), To(9)));
            s.send(NumSignal(1), (At(3), To(9)));
            s.send(NumSignal(2), (At(1), To(1)));
            s.send(NumSignal(3), (At(2), To(9)));
        }

        for _ in 0..4 {
            end_tick(&world);
            {
                let mut s = world.signal::<NumSignal>();
                let tick = s.tick();
                s.send(NumSignal(100 + tick), (In(1), For(2)));
            }

            let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
            for chunks in [&signals.transitory_active, &signals.transitory_inactive] {
                assert!(chunks.windows(2).all(|w| w[0].key < w[1].key));
                for chunk in chunks {
                    assert!(chunk.live().windows(2).all(|w| w[0].1.id < w[1].1.id));
                }
            }
        }

        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        let active: Vec<u64> = signals
            .transitory_active
            .iter()
            .flat_map(|chunk| chunk.live().iter().map(|x| x.1.payload.0))
            .collect();
        assert_eq!(active, vec![0, 1, 3, 102, 103]);
    }

    #[test]
    fn windows_across_chunks_match_a_simple_model() {
        windows_model_run(false);
    }

    #[test]
    fn same_length_windows_expire_by_head_and_match_a_simple_model() {
        assert!(windows_model_run(true) > 0);
    }

    fn windows_model_run(same_length: bool) -> usize {
        struct Sent {
            id: u64,
            sent: u64,
            start: u64,
            end: u64,
            payload: u64,
            stopped: u64,
        }

        let mut world = world();
        let mut rng = 0x2545F4914F6CDD1Du64;
        let mut random = move |below: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % below
        };
        let mut model: Vec<Sent> = Vec::new();
        let mut payloads = 0;
        let mut head_moves = 0;

        for tick in 0..600u64 {
            if tick == 300 {
                let bytes = world.save();
                world = super::tests::world();
                world.load(&bytes);
            }

            let mut expected: Vec<u64> = model
                .iter()
                .filter(|s| {
                    s.sent < tick
                        && tick < s.stopped
                        && ((s.start <= tick && tick < s.end)
                            || (s.start <= s.sent && tick == s.sent + 1))
                })
                .map(|s| s.payload)
                .collect();
            let mut received = receive(&world);
            expected.sort();
            received.sort();
            assert_eq!(received, expected, "tick {tick}");

            let mut s = world.signal::<NumSignal>();
            for _ in 0..random(6) {
                let start = if same_length || random(4) == 0 {
                    tick.saturating_sub(random(3))
                } else {
                    tick + random(200)
                };
                let end = if same_length {
                    tick + 90
                } else if random(10) == 0 {
                    u64::MAX
                } else {
                    start.max(tick) + 1 + random(300)
                };
                payloads += 1;
                let id = if end == u64::MAX {
                    s.send(NumSignal(payloads), (At(start), ..))
                } else {
                    s.send(NumSignal(payloads), (At(start), To(end - 1)))
                };
                model.push(Sent {
                    id: id.unwrap(),
                    sent: tick,
                    start,
                    end,
                    payload: payloads,
                    stopped: u64::MAX,
                });
            }

            for _ in 0..random(4) {
                let i = random(model.len().max(1) as u64) as usize;
                let Some(sent) = model.get_mut(i) else {
                    break;
                };
                if sent.end <= tick {
                    continue;
                }
                let live = sent.stopped == u64::MAX;
                if random(2) == 0 {
                    payloads += 1;
                    assert_eq!(s.modify(sent.id, NumSignal(payloads)), live);
                    if live {
                        sent.payload = payloads;
                    }
                } else {
                    assert_eq!(s.cancel(sent.id), live);
                    if live {
                        sent.stopped = tick + 1;
                    }
                }
            }
            drop(s);
            end_tick(&world);

            let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
            if signals.transitory_active.iter().any(|chunk| chunk.head > 0) {
                head_moves += 1;
            }
        }

        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert!(signals.transitory_active.len() + signals.transitory_inactive.len() > 1);
        head_moves
    }

    #[test]
    fn cleanup_signals_skips_uninitialized() {
        let world = World::new();
        world.cleanup_signals();
        assert!(!world.has_resource::<Signals<NumSignal>>());
    }

    #[test]
    fn systems_send_and_receive() {
        #[resource]
        struct Received(pub Vec<Entity>);

        #[system]
        fn sender(mut signal: Mut<Signal<TestSignal>>) {
            signal.emit(TestSignal(Entity::new(4, 1)));
        }

        #[system]
        fn receiver(mut signal: Mut<Signal<TestSignal>>, mut received: Mut<Resource<Received>>) {
            (*received).0.extend(signal.receive().map(|s| s.0));
        }

        let mut world = World::new();
        let sender = sender.into_system();
        let receiver = receiver.into_system();

        sender.run(&mut world);
        receiver.run(&mut world);
        end_tick(&world);
        receiver.run(&mut world);

        let received = world.get_resource::<Received>().unwrap();
        assert_eq!(received.0, vec![Entity::new(4, 1)]);
    }

    #[resource]
    struct ReceivedA(pub Vec<u64>);

    #[resource]
    struct ReceivedB(pub Vec<u64>);

    fn receiver_a(mut signal: Mut<Signal<NumSignal>>, mut received: Mut<Resource<ReceivedA>>) {
        (*received).0.extend(signal.receive().map(|s| s.0));
    }

    fn receiver_b(mut signal: Mut<Signal<NumSignal>>, mut received: Mut<Resource<ReceivedB>>) {
        (*received).0.extend(signal.receive().map(|s| s.0));
    }

    fn receivers(world: &mut World) -> (SystemEntry, SystemEntry) {
        world.init_resource::<ReceivedA>();
        world.init_resource::<ReceivedB>();
        (
            receiver_a.into_system().named(module_path!(), "receiver_a"),
            receiver_b.into_system().named(module_path!(), "receiver_b"),
        )
    }

    fn take_a(world: &World) -> Vec<u64> {
        std::mem::take(&mut world.get_resource_mut::<ReceivedA>().unwrap().0)
    }

    fn take_b(world: &World) -> Vec<u64> {
        std::mem::take(&mut world.get_resource_mut::<ReceivedB>().unwrap().0)
    }

    #[test]
    fn every_receiver_sees_transient_signal_once() {
        let mut world = world();
        let (a, b) = receivers(&mut world);

        world.signal::<NumSignal>().emit(NumSignal(7));
        a.run(&mut world);
        b.run(&mut world);
        assert_eq!(take_a(&world), vec![7]);
        assert_eq!(take_b(&world), vec![7]);

        a.run(&mut world);
        end_tick(&world);
        a.run(&mut world);
        b.run(&mut world);
        assert_eq!(take_a(&world), Vec::<u64>::new());
        assert_eq!(take_b(&world), Vec::<u64>::new());
    }

    #[test]
    fn receiver_before_sender_gets_signal_next_tick() {
        let mut world = world();
        let (a, b) = receivers(&mut world);

        a.run(&mut world);
        world.signal::<NumSignal>().emit(NumSignal(3));
        b.run(&mut world);
        assert_eq!(take_a(&world), Vec::<u64>::new());
        assert_eq!(take_b(&world), vec![3]);

        end_tick(&world);
        a.run(&mut world);
        b.run(&mut world);
        assert_eq!(take_a(&world), vec![3]);
        assert_eq!(take_b(&world), Vec::<u64>::new());
    }

    #[test]
    fn calls_outside_systems_keep_their_own_cursor() {
        let mut world = world();
        let (a, _) = receivers(&mut world);

        world.signal::<NumSignal>().emit(NumSignal(1));
        assert_eq!(receive(&world), vec![1]);

        a.run(&mut world);
        assert_eq!(take_a(&world), vec![1]);
        assert_eq!((*world.get_resource::<CurrentSystem>().unwrap()).0, 0);

        world.signal::<NumSignal>().emit(NumSignal(2));
        assert_eq!(receive(&world), vec![2]);
        a.run(&mut world);
        assert_eq!(take_a(&world), vec![2]);
    }

    #[test]
    fn cursors_stay_sorted_and_skip_senders() {
        let mut world = world();
        let (a, b) = receivers(&mut world);

        world.signal::<NumSignal>().emit(NumSignal(1));
        assert!(
            world
                .get_resource::<SignalData<NumSignal>>()
                .unwrap()
                .cursors
                .is_empty()
        );

        b.run(&mut world);
        a.run(&mut world);
        receive(&world);

        let data = world.get_resource::<SignalData<NumSignal>>().unwrap();
        assert_eq!(data.cursors.len(), 3);
        assert!(data.cursors.windows(2).all(|w| w[0].system < w[1].system));
    }

    #[test]
    fn second_receive_in_a_tick_only_gets_new_signals() {
        let world = world();
        world
            .signal::<NumSignal>()
            .send(NumSignal(1), (At(0), To(2)));
        world.signal::<NumSignal>().send(NumSignal(2), ..);

        let mut first = Vec::new();
        let mut second = Vec::new();
        for tick in 0..4 {
            let mut received = receive(&world);
            received.sort_unstable();
            first.push(received);
            world.signal::<NumSignal>().emit(NumSignal(100 + tick));
            world
                .signal::<NumSignal>()
                .send(NumSignal(200 + tick), For(2));
            second.push(receive(&world));
            end_tick(&world);
        }

        assert_eq!(
            first,
            vec![vec![1, 2], vec![1, 2, 200], vec![1, 2, 201], vec![2, 202]]
        );
        assert_eq!(
            second,
            vec![
                vec![100, 200],
                vec![101, 201],
                vec![102, 202],
                vec![103, 203]
            ]
        );
    }

    #[test]
    fn repeat_receive_skips_permanent_during_a_sweep_with_cancels() {
        let world = permanent_world(8);
        assert_eq!(sweep(&world, 4, |s| s.0 % 2 == 0), (4, false));
        assert!(cancel(&world, 6));

        assert_eq!(receive(&world), vec![0, 2, 4, 5, 7]);
        assert_eq!(receive(&world), Vec::<u64>::new());
        assert_eq!(receive_by_next(&world), Vec::<u64>::new());

        world.signal::<NumSignal>().emit(NumSignal(100));
        assert_eq!(receive(&world), vec![100]);

        end_tick(&world);
        assert_eq!(receive(&world), vec![0, 2, 4, 5, 7]);
    }

    #[test]
    fn window_sent_after_receiver_ran_is_not_delivered_twice() {
        let mut world = world();
        let (a, b) = receivers(&mut world);

        a.run(&mut world);
        world
            .signal::<NumSignal>()
            .send(NumSignal(5), (At(0), To(2)));
        b.run(&mut world);

        let mut late = vec![take_a(&world)];
        let mut early = vec![take_b(&world)];
        for _ in 0..3 {
            end_tick(&world);
            a.run(&mut world);
            b.run(&mut world);
            late.push(take_a(&world));
            early.push(take_b(&world));
        }

        assert_eq!(late, vec![vec![], vec![5], vec![5], vec![]]);
        assert_eq!(early, vec![vec![5], vec![5], vec![5], vec![]]);
    }

    #[test]
    fn identical_signals_sent_after_receiver_ran_all_arrive_next_tick() {
        let mut world = world();
        let (a, _) = receivers(&mut world);

        a.run(&mut world);
        {
            let mut s = world.signal::<NumSignal>();
            s.emit(NumSignal(5));
            s.emit(NumSignal(5));
            s.send(NumSignal(5), (At(0), To(1)));
            s.send(NumSignal(5), (At(0), To(1)));
        }
        end_tick(&world);

        a.run(&mut world);
        assert_eq!(take_a(&world), vec![5, 5, 5, 5]);
    }

    #[test]
    fn dedup_only_skips_signals_in_both_lists() {
        let world = world();
        world.signal::<NumSignal>().send(NumSignal(10), (At(0), ..));
        receive(&world);
        end_tick(&world);

        receive(&world);
        {
            let mut s = world.signal::<NumSignal>();
            s.emit(NumSignal(1));
            s.send(NumSignal(2), (At(1), To(3)));
            s.emit(NumSignal(3));
        }
        end_tick(&world);

        assert_eq!(receive(&world), vec![1, 3, 2, 10]);
    }

    #[test]
    fn dedup_delivers_modified_payload() {
        let world = world();
        receive(&world);
        let id = world
            .signal::<NumSignal>()
            .send(NumSignal(1), (At(0), To(2)))
            .unwrap();
        end_tick(&world);

        assert!(world.signal::<NumSignal>().modify(id, NumSignal(2)));
        assert_eq!(receive(&world), vec![2]);
    }

    #[test]
    fn modify_on_send_tick_reaches_later_receiver() {
        let world = world();
        let id = world
            .signal::<NumSignal>()
            .send(NumSignal(1), (At(0), To(2)))
            .unwrap();

        assert!(world.signal::<NumSignal>().modify(id, NumSignal(2)));
        assert_eq!(receive(&world), vec![2]);
    }

    #[test]
    fn modify_changes_one_tick_signal() {
        let world = world();
        let id = world.signal::<NumSignal>().emit(NumSignal(1));

        assert!(world.signal::<NumSignal>().modify(id, NumSignal(2)));
        assert_eq!(receive(&world), vec![2]);
    }

    #[test]
    fn modify_changes_one_tick_signal_for_late_receiver() {
        let world = world();
        receive(&world);
        let id = world.signal::<NumSignal>().emit(NumSignal(1));
        end_tick(&world);

        assert!(world.signal::<NumSignal>().modify(id, NumSignal(2)));
        assert_eq!(receive(&world), vec![2]);
    }

    #[test]
    fn modify_fails_after_one_tick_signal_expires() {
        let world = world();
        let id = world.signal::<NumSignal>().emit(NumSignal(1));
        end_tick(&world);
        end_tick(&world);

        assert!(!world.signal::<NumSignal>().modify(id, NumSignal(2)));
    }

    fn send(world: &World, payload: u64, window: std::ops::Range<u64>) -> u64 {
        world
            .signal::<NumSignal>()
            .send(NumSignal(payload), (At(window.start), To(window.end - 1)))
            .unwrap()
    }

    fn cancel(world: &World, id: u64) -> bool {
        world.signal::<NumSignal>().cancel(id)
    }

    #[test]
    fn cancel_stops_active_signal_for_the_next_receiver() {
        let world = world();
        let id = send(&world, 1, 0..5);
        assert_eq!(run_ticks(&world, 2), vec![vec![1], vec![1]]);

        assert_eq!(receive(&world), vec![1]);
        assert!(cancel(&world, id));
        assert_eq!(peek(&world), Vec::<u64>::new());
    }

    #[test]
    fn cancel_stops_future_signal() {
        let world = world();
        let id = send(&world, 1, 2..4);
        end_tick(&world);

        assert!(cancel(&world, id));
        assert_eq!(run_ticks(&world, 5), vec![Vec::<u64>::new(); 5]);
    }

    #[test]
    fn cancel_stops_future_signal_while_merging_with_active() {
        let world = world();
        send(&world, 1, 0..9);
        let id = send(&world, 2, 2..4);
        send(&world, 3, 2..4);
        end_tick(&world);

        assert!(cancel(&world, id));
        assert_eq!(run_ticks(&world, 3), vec![vec![1], vec![1, 3], vec![1, 3]]);
    }

    #[test]
    fn cancel_on_send_tick_stops_later_receiver() {
        let world = world();
        let id = send(&world, 1, 0..5);

        assert!(cancel(&world, id));
        assert_eq!(run_ticks(&world, 3), vec![Vec::<u64>::new(); 3]);
    }

    #[test]
    fn cancel_on_send_tick_stops_receiver_that_ran_before_sender() {
        let world = world();
        receive(&world);
        let id = send(&world, 1, 0..5);

        assert!(cancel(&world, id));
        end_tick(&world);
        assert_eq!(receive(&world), Vec::<u64>::new());
    }

    #[test]
    fn cancel_on_next_tick_stops_receiver_that_ran_before_sender() {
        let world = world();
        receive(&world);
        let id = send(&world, 1, 0..5);
        end_tick(&world);

        assert!(cancel(&world, id));
        assert_eq!(receive(&world), Vec::<u64>::new());
    }

    #[test]
    fn cancel_stops_one_tick_signal() {
        let world = world();
        let id = world.signal::<NumSignal>().emit(NumSignal(1));

        assert!(cancel(&world, id));
        assert_eq!(receive(&world), Vec::<u64>::new());
    }

    #[test]
    fn cancel_stops_one_tick_signal_for_late_receiver() {
        let world = world();
        receive(&world);
        let id = world.signal::<NumSignal>().emit(NumSignal(1));
        end_tick(&world);

        assert!(cancel(&world, id));
        assert_eq!(receive(&world), Vec::<u64>::new());
    }

    #[test]
    fn cancel_leaves_other_signals_in_order() {
        let world = world();
        send(&world, 1, 0..3);
        let id = send(&world, 2, 0..3);
        send(&world, 3, 0..3);
        world.signal::<NumSignal>().emit(NumSignal(4));
        end_tick(&world);

        assert!(cancel(&world, id));
        assert_eq!(run_ticks(&world, 2), vec![vec![4, 1, 3], vec![1, 3]]);
    }

    #[test]
    fn cancel_returns_false_for_unknown_id() {
        let world = world();
        send(&world, 1, 0..5);

        assert!(!cancel(&world, 99));
    }

    #[test]
    fn cancel_returns_false_when_already_cancelled() {
        let world = world();
        let windowed = send(&world, 1, 0..5);
        let one_tick = world.signal::<NumSignal>().emit(NumSignal(2));

        assert!(cancel(&world, windowed));
        assert!(cancel(&world, one_tick));
        assert!(!cancel(&world, windowed));
        assert!(!cancel(&world, one_tick));

        end_tick(&world);
        assert!(!cancel(&world, windowed));
    }

    #[test]
    fn cancel_returns_false_after_signal_expires() {
        let world = world();
        let id = send(&world, 1, 0..2);
        run_ticks(&world, 3);

        assert!(!cancel(&world, id));
    }

    #[test]
    fn modify_fails_on_cancelled_signal() {
        let world = world();
        let id = send(&world, 1, 0..5);
        end_tick(&world);
        end_tick(&world);

        assert!(cancel(&world, id));
        assert!(!world.signal::<NumSignal>().modify(id, NumSignal(2)));
    }

    #[test]
    fn cleanup_drops_cancelled_signals() {
        let world = world();
        let started = send(&world, 1, 0..5);
        let future = send(&world, 2, 3..5);
        end_tick(&world);
        assert!(cancel(&world, started));
        assert!(cancel(&world, future));
        end_tick(&world);

        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert!(signals.transitory_active.is_empty());
        assert!(signals.transitory_inactive.is_empty());
    }

    #[test]
    fn cancelled_windows_stay_until_half_their_chunk_is_cancelled() {
        let world = world();
        let ids: Vec<u64> = (0..10).map(|i| send(&world, i, 0..50)).collect();
        end_tick(&world);
        for &id in &ids[..5] {
            assert!(cancel(&world, id));
        }
        end_tick(&world);

        let live = |world: &World| {
            let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
            signals.transitory_active[0].live().len()
        };
        assert_eq!(live(&world), 10);
        assert_eq!(receive(&world), vec![5, 6, 7, 8, 9]);

        assert!(cancel(&world, ids[5]));
        assert_eq!(peek(&world), vec![6, 7, 8, 9]);
        end_tick(&world);
        assert_eq!(live(&world), 4);
        assert_eq!(receive(&world), vec![6, 7, 8, 9]);
    }

    #[test]
    fn cancel_survives_save_before_cleanup() {
        let world = world();
        let id = send(&world, 1, 0..5);
        end_tick(&world);
        end_tick(&world);
        assert!(cancel(&world, id));

        let bytes = world.save();
        let mut restored = super::tests::world();
        restored.load(&bytes);

        assert_eq!(receive(&restored), Vec::<u64>::new());
        end_tick(&restored);
        assert_eq!(receive(&restored), Vec::<u64>::new());
    }

    fn send_open(world: &World, payload: u64) -> u64 {
        world
            .signal::<NumSignal>()
            .send(NumSignal(payload), ..)
            .unwrap()
    }

    fn permanent(world: &World) -> Vec<(u64, u64)> {
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        signals
            .permanent
            .iter()
            .map(|x| (x.id, x.payload.0))
            .collect()
    }

    fn sweep(world: &World, budget: u64, keep: impl FnMut(&NumSignal) -> bool) -> (u64, bool) {
        world
            .get_resource_mut::<Signals<NumSignal>>()
            .unwrap()
            .sweep(budget, keep)
    }

    #[test]
    fn open_signal_moves_to_permanent_after_one_tick_in_active() {
        let world = world();
        send_open(&world, 1);

        end_tick(&world);
        {
            let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
            assert_eq!(signals.permanent_joining.len(), 1);
            assert!(signals.permanent.is_empty());
        }

        end_tick(&world);
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert!(signals.permanent_joining.is_empty());
        assert!(signals.transitory_active.is_empty());
        assert!(signals.transitory_inactive.is_empty());
        assert_eq!(signals.permanent.len(), 1);
    }

    #[test]
    fn open_signal_is_delivered_once_every_tick() {
        let world = world();
        send_open(&world, 1);

        assert_eq!(run_ticks(&world, 4), vec![vec![1]; 4]);
    }

    #[test]
    fn open_signal_is_delivered_once_to_receiver_that_ran_before_sender() {
        let world = world();
        receive(&world);
        send_open(&world, 1);
        end_tick(&world);

        assert_eq!(run_ticks(&world, 3), vec![vec![1]; 3]);
    }

    #[test]
    fn finite_signal_never_moves_to_permanent() {
        let world = world();
        send(&world, 1, 0..100);
        run_ticks(&world, 3);

        assert!(permanent(&world).is_empty());
    }

    #[test]
    fn future_open_signal_starts_on_time_and_moves_to_permanent() {
        let world = world();
        world.signal::<NumSignal>().send(NumSignal(1), (At(2), ..));

        assert_eq!(
            run_ticks(&world, 5),
            vec![vec![], vec![], vec![1], vec![1], vec![1]]
        );
        assert_eq!(permanent(&world), vec![(0, 1)]);
    }

    #[test]
    fn late_joining_signal_keeps_permanent_sorted_by_id() {
        let world = world();
        let late = world
            .signal::<NumSignal>()
            .send(NumSignal(1), (At(3), ..))
            .unwrap();
        let early = send_open(&world, 2);

        assert_eq!(
            run_ticks(&world, 5),
            vec![vec![2], vec![2], vec![2], vec![1, 2], vec![1, 2]]
        );
        assert_eq!(permanent(&world), vec![(late, 1), (early, 2)]);
        assert!(world.signal::<NumSignal>().modify(late, NumSignal(3)));
        assert!(cancel(&world, early));
        assert_eq!(receive(&world), vec![3]);
    }

    #[test]
    fn modify_changes_permanent_signal() {
        let world = world();
        let id = send_open(&world, 1);
        run_ticks(&world, 2);

        assert!(world.signal::<NumSignal>().modify(id, NumSignal(2)));
        assert_eq!(receive(&world), vec![2]);
    }

    #[test]
    fn cancel_stops_permanent_signal() {
        let world = world();
        send_open(&world, 1);
        let id = send_open(&world, 2);
        send_open(&world, 3);
        run_ticks(&world, 2);

        assert!(cancel(&world, id));
        assert!(!cancel(&world, id));
        assert!(!world.signal::<NumSignal>().modify(id, NumSignal(9)));
        assert_eq!(receive(&world), vec![1, 3]);
    }

    #[test]
    fn cancelled_permanent_signal_is_removed_without_keep() {
        let world = world();
        send_open(&world, 1);
        let id = send_open(&world, 2);
        send_open(&world, 3);
        run_ticks(&world, 2);

        assert!(cancel(&world, id));
        end_tick(&world);

        assert_eq!(permanent(&world), vec![(0, 1), (2, 3)]);
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert_eq!(signals.permanent_cancelled, 0);
    }

    #[test]
    fn permanent_without_keep_or_cancel_is_never_swept() {
        let world = world();
        send_open(&world, 1);
        run_ticks(&world, 3);

        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert_eq!(signals.permanent_read, 0);
        assert_eq!(signals.permanent.len(), 1);
    }

    fn permanent_world(count: u64) -> World {
        let world = world();
        for payload in 0..count {
            send_open(&world, payload);
        }
        run_ticks(&world, 2);
        world
    }

    #[test]
    fn sweep_stops_at_its_budget() {
        let world = permanent_world(6);

        assert_eq!(sweep(&world, 2, |_| true), (2, false));
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert_eq!(signals.permanent_read, 2);
        assert_eq!(signals.permanent_write, 2);
    }

    #[test]
    fn sweep_stops_at_the_end_of_the_list() {
        let world = permanent_world(3);

        assert_eq!(sweep(&world, 10, |_| true), (3, true));
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert_eq!(signals.permanent_read, 0);
        assert_eq!(signals.permanent.len(), 3);
    }

    #[test]
    fn sweep_on_empty_list_finishes_at_no_cost() {
        let world = world();

        assert_eq!(sweep(&world, 10, |_| true), (0, true));
    }

    #[test]
    fn sweep_drops_failed_signals_across_ticks_and_keeps_order() {
        let world = permanent_world(6);
        let even = |s: &NumSignal| s.0 % 2 == 0;

        assert_eq!(sweep(&world, 2, even), (2, false));
        assert_eq!(sweep(&world, 2, even), (2, false));
        assert_eq!(sweep(&world, 2, even), (2, true));

        assert_eq!(permanent(&world), vec![(0, 0), (2, 2), (4, 4)]);
    }

    #[test]
    fn partial_sweep_keeps_delivery_and_lookups_working() {
        let world = permanent_world(6);

        assert_eq!(sweep(&world, 3, |s| s.0 % 2 == 0), (3, false));
        assert_eq!(receive(&world), vec![0, 2, 3, 4, 5]);

        assert!(world.signal::<NumSignal>().modify(2, NumSignal(20)));
        assert!(world.signal::<NumSignal>().modify(4, NumSignal(40)));
        assert!(!world.signal::<NumSignal>().modify(1, NumSignal(10)));
        assert!(!cancel(&world, 1));
        assert!(cancel(&world, 0));
        assert!(cancel(&world, 5));
        assert_eq!(peek(&world), vec![20, 3, 40]);
    }

    #[test]
    fn sweep_drops_cancelled_signals_and_clears_the_count() {
        let world = permanent_world(3);
        assert!(cancel(&world, 1));

        assert_eq!(sweep(&world, 3, |_| true), (3, true));
        assert_eq!(permanent(&world), vec![(0, 0), (2, 2)]);
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert_eq!(signals.permanent_cancelled, 0);
    }

    #[test]
    fn cancel_on_both_sides_of_the_sweep_survives_compaction() {
        let world = permanent_world(8);

        assert_eq!(sweep(&world, 4, |s| s.0 % 2 == 0), (4, false));
        assert!(cancel(&world, 2));
        assert!(cancel(&world, 6));
        assert!(!cancel(&world, 2));
        assert_eq!(receive(&world), vec![0, 4, 5, 7]);

        assert_eq!(sweep(&world, 10, |_| true), (4, true));
        assert_eq!(peek(&world), vec![0, 4, 5, 7]);
        assert!(!cancel(&world, 6));
        assert!(!world.signal::<NumSignal>().modify(2, NumSignal(9)));

        assert_eq!(sweep(&world, 10, |_| true), (5, true));
        assert_eq!(permanent(&world), vec![(0, 0), (4, 4), (5, 5), (7, 7)]);
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert_eq!(signals.permanent_cancelled, 0);
        assert!(
            signals
                .permanent_cancelled_bits
                .words
                .iter()
                .all(|&w| w == 0)
        );
        drop(signals);

        assert!(cancel(&world, 5));
        assert_eq!(peek(&world), vec![0, 4, 7]);
    }

    #[test]
    fn receive_skips_cancelled_signals_across_bit_words_and_the_gap() {
        let world = permanent_world(200);
        assert_eq!(sweep(&world, 70, |s| s.0 != 10), (70, false));

        let cancelled = [0, 9, 11, 63, 64, 68, 69, 70, 71, 127, 128, 199];
        for id in cancelled {
            assert!(cancel(&world, id));
        }

        let expected: Vec<u64> = (0..200)
            .filter(|id| *id != 10 && !cancelled.contains(id))
            .collect();
        assert_eq!(receive(&world), expected);
    }

    #[test]
    fn receive_handles_every_signal_cancelled() {
        let world = permanent_world(70);
        for id in 0..70 {
            assert!(cancel(&world, id));
        }

        assert_eq!(receive(&world), Vec::<u64>::new());
    }

    #[test]
    fn late_join_ahead_of_the_sweep_keeps_its_position_and_gap() {
        let world = kept_world();
        let entities = spawn_all(&world, 6);
        for &entity in &entities[..4] {
            world.signal::<Kept>().send(Kept(entity), ..);
        }
        world.signal::<Kept>().send(Kept(entities[4]), (At(3), ..));
        world.signal::<Kept>().send(Kept(entities[5]), ..);
        end_tick(&world);
        end_tick(&world);
        assert_eq!(sweep_kept(&world, 2, |s| s.0 != entities[0]), (2, false));

        end_tick(&world);
        end_tick(&world);

        {
            let signals = world.get_resource::<Signals<Kept>>().unwrap();
            assert_eq!(signals.permanent_write, 1);
            assert_eq!(signals.permanent_read, 2);
        }
        assert_eq!(received_kept(&world), entities[1..].to_vec());
        assert!(world.signal::<Kept>().modify(4, Kept(entities[4])));
        assert_eq!(sweep_kept(&world, 10, |_| true), (4, true));
        assert_eq!(kept_ids(&world), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn late_signals_and_new_signals_joining_together_stay_sorted() {
        let world = world();
        let late_a = world
            .signal::<NumSignal>()
            .send(NumSignal(1), (At(2), ..))
            .unwrap();
        send_open(&world, 2);
        let late_b = world
            .signal::<NumSignal>()
            .send(NumSignal(3), (At(2), ..))
            .unwrap();
        send_open(&world, 4);
        run_ticks(&world, 1);
        let newest = send_open(&world, 5);
        run_ticks(&world, 1);
        assert_eq!(permanent(&world), vec![(1, 2), (3, 4)]);

        run_ticks(&world, 1);

        assert_eq!(
            permanent(&world),
            vec![(late_a, 1), (1, 2), (late_b, 3), (3, 4), (newest, 5)]
        );
        for id in [late_a, late_b, newest, 1, 3] {
            assert!(cancel(&world, id));
        }
    }

    #[test]
    fn late_join_with_pending_cancels_keeps_the_right_signals_cancelled() {
        let world = kept_world();
        let entities = spawn_all(&world, 4);
        world.signal::<Kept>().send(Kept(entities[0]), (At(3), ..));
        for &entity in &entities[1..] {
            world.signal::<Kept>().send(Kept(entity), ..);
        }
        for _ in 0..3 {
            end_tick(&world);
        }
        assert!(world.signal::<Kept>().cancel(2));

        end_tick(&world);

        assert_eq!(kept_ids(&world), vec![0, 1, 2, 3]);
        assert_eq!(
            received_kept(&world),
            vec![entities[0], entities[1], entities[3]]
        );
        assert!(!world.signal::<Kept>().cancel(2));
        assert!(world.signal::<Kept>().modify(3, Kept(entities[3])));

        end_tick(&world);
        end_tick(&world);

        assert_eq!(kept_ids(&world), vec![0, 1, 3]);
        let signals = world.get_resource::<Signals<Kept>>().unwrap();
        assert_eq!(signals.permanent_cancelled, 0);
        assert!(
            signals
                .permanent_cancelled_bits
                .words
                .iter()
                .all(|&w| w == 0)
        );
    }

    #[test]
    fn late_join_drops_cancelled_permanent_signals() {
        let world = world();
        let late = world
            .signal::<NumSignal>()
            .send(NumSignal(9), (At(3), ..))
            .unwrap();
        for payload in 1..=3 {
            send_open(&world, payload);
        }
        run_ticks(&world, 2);
        assert!(cancel(&world, 2));

        run_ticks(&world, 2);

        assert_eq!(permanent(&world), vec![(late, 9), (1, 1), (3, 3)]);
        assert_eq!(receive(&world), vec![9, 1, 3]);
    }

    #[signal(entity)]
    pub struct Kept(pub Entity);

    #[signal(entity)]
    pub struct OtherKept(pub Entity);

    fn kept_world() -> World {
        let mut world = World::new();
        world.init_signal::<Kept>();
        world.init_signal::<OtherKept>();
        world
    }

    fn spawn_all(world: &World, count: usize) -> Vec<Entity> {
        let mut em = world.entity_manager();
        (0..count).map(|_| em.spawn()).collect()
    }

    fn received_kept(world: &World) -> Vec<Entity> {
        world.signal::<Kept>().receive().map(|s| s.0).collect()
    }

    fn kept_ids(world: &World) -> Vec<u64> {
        let signals = world.get_resource::<Signals<Kept>>().unwrap();
        signals.permanent.iter().map(|x| x.id).collect()
    }

    fn sweep_kept(world: &World, budget: u64, keep: impl FnMut(&Kept) -> bool) -> (u64, bool) {
        world
            .get_resource_mut::<Signals<Kept>>()
            .unwrap()
            .sweep(budget, keep)
    }

    #[test]
    fn repeat_receive_on_an_entity_signal_filters_only_new_signals() {
        let world = kept_world();
        let entities = spawn_all(&world, 4);
        world.signal::<Kept>().send(Kept(entities[0]), ..);
        world.signal::<Kept>().send(Kept(entities[1]), ..);
        end_tick(&world);
        end_tick(&world);

        assert_eq!(received_kept(&world), entities[..2].to_vec());
        assert_eq!(received_kept(&world), Vec::<Entity>::new());

        world.signal::<Kept>().emit(Kept(entities[2]));
        world.signal::<Kept>().emit(Kept(entities[3]));
        world.entity_manager().despawn(entities[3]);
        assert_eq!(received_kept(&world), vec![entities[2]]);

        end_tick(&world);
        assert_eq!(received_kept(&world), entities[..2].to_vec());
    }

    #[test]
    fn signal_joining_during_a_lap_is_kept_in_order() {
        let world = kept_world();
        let entities = spawn_all(&world, 5);
        for &entity in &entities[..4] {
            world.signal::<Kept>().send(Kept(entity), ..);
        }
        end_tick(&world);
        end_tick(&world);
        assert_eq!(sweep_kept(&world, 2, |s| s.0 != entities[0]), (2, false));

        world.signal::<Kept>().send(Kept(entities[4]), ..);
        end_tick(&world);
        end_tick(&world);

        assert_eq!(received_kept(&world), entities[1..].to_vec());
        assert_eq!(sweep_kept(&world, 10, |_| true), (3, true));
        assert_eq!(kept_ids(&world), vec![1, 2, 3, 4]);
    }

    #[test]
    fn late_signal_behind_the_sweep_waits_in_active_until_the_lap_ends() {
        let world = kept_world();
        let entities = spawn_all(&world, 5);
        world.signal::<Kept>().send(Kept(entities[0]), (At(3), ..));
        for &entity in &entities[1..] {
            world.signal::<Kept>().send(Kept(entity), ..);
        }
        end_tick(&world);
        end_tick(&world);
        assert_eq!(sweep_kept(&world, 2, |_| true), (2, false));

        end_tick(&world);
        end_tick(&world);

        {
            let signals = world.get_resource::<Signals<Kept>>().unwrap();
            assert_eq!(signals.permanent_joining.len(), 1);
            assert_eq!(signals.permanent.len(), 4);
        }
        assert_eq!(received_kept(&world), entities);
        assert!(world.signal::<Kept>().modify(0, Kept(entities[0])));

        assert_eq!(sweep_kept(&world, 10, |_| true), (2, true));
        end_tick(&world);

        let signals = world.get_resource::<Signals<Kept>>().unwrap();
        assert!(signals.permanent_joining.is_empty());
        assert_eq!(signals.permanent_read, 0);
        drop(signals);
        assert_eq!(kept_ids(&world), vec![0, 1, 2, 3, 4]);
        assert_eq!(received_kept(&world), entities);
    }

    #[test]
    fn lap_without_a_filter_finishes_after_its_cancels_are_gone() {
        let count = SWEEP_BUDGET + 10;
        let world = permanent_world(count);
        assert!(cancel(&world, 0));

        end_tick(&world);
        {
            let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
            assert_eq!(signals.permanent_cancelled, 0);
            assert!(signals.permanent_read > 0);
        }

        end_tick(&world);
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert_eq!(signals.permanent_read, 0);
        assert_eq!(signals.permanent_write, 0);
        assert_eq!(signals.permanent.len() as u64, count - 1);
    }

    #[test]
    fn dead_entity_signal_is_not_delivered_before_the_sweep_reaches_it() {
        let world = kept_world();
        let entities = spawn_all(&world, 2);
        world.signal::<Kept>().send(Kept(entities[0]), ..);
        world.signal::<Kept>().send(Kept(entities[1]), ..);
        for _ in 0..3 {
            end_tick(&world);
        }

        world.entity_manager().despawn(entities[0]);

        assert_eq!(received_kept(&world), vec![entities[1]]);
        let signals = world.get_resource::<Signals<Kept>>().unwrap();
        assert_eq!(signals.permanent.len(), 2);
    }

    #[test]
    fn dead_entity_one_tick_signal_is_not_delivered() {
        let world = kept_world();
        let entities = spawn_all(&world, 2);
        world.signal::<Kept>().emit(Kept(entities[0]));
        world.signal::<Kept>().emit(Kept(entities[1]));
        world.entity_manager().despawn(entities[0]);

        assert_eq!(received_kept(&world), vec![entities[1]]);
    }

    #[test]
    fn signal_for_a_new_entity_at_a_reused_index_is_delivered() {
        let world = kept_world();
        let old = spawn_all(&world, 1)[0];
        world.signal::<Kept>().send(Kept(old), ..);
        world.entity_manager().despawn(old);
        let new = world.entity_manager().spawn();
        assert_eq!(new.index(), old.index());
        world.signal::<Kept>().send(Kept(new), ..);

        assert_eq!(received_kept(&world), vec![new]);
    }

    #[test]
    fn signal_without_a_filter_is_delivered_after_its_entity_dies() {
        let mut world = World::new();
        world.init_signal::<TestSignal>();
        let entity = world.entity_manager().spawn();
        world.signal::<TestSignal>().emit(TestSignal(entity));
        world.entity_manager().despawn(entity);

        let received: Vec<Entity> = world
            .signal::<TestSignal>()
            .receive()
            .map(|s| s.0)
            .collect();
        assert_eq!(received, vec![entity]);
    }

    #[resource]
    struct LoopTargets {
        first: Entity,
        second: Entity,
        delivered: Vec<Entity>,
    }

    fn despawning_receiver(
        mut signal: Mut<Signal<Kept>>,
        mut em: Mut<EntityManager>,
        mut targets: Mut<Resource<LoopTargets>>,
    ) {
        for kept in signal.receive() {
            targets.delivered.push(kept.0);
            if kept.0 == targets.first {
                em.despawn(targets.second);
            }
        }
    }

    #[test]
    fn system_can_receive_entity_signals_and_despawn_in_the_same_loop() {
        let mut world = kept_world();
        let entities = spawn_all(&world, 3);
        for &entity in &entities {
            world.signal::<Kept>().send(Kept(entity), ..);
        }
        world.insert_resource(LoopTargets {
            first: entities[0],
            second: entities[1],
            delivered: Vec::new(),
        });

        despawning_receiver.into_system().run(&mut world);

        let targets = world.get_resource::<LoopTargets>().unwrap();
        assert_eq!(targets.delivered, vec![entities[0], entities[2]]);
    }

    #[test]
    fn sweep_removes_signals_of_dead_entities() {
        let world = kept_world();
        let entities = spawn_all(&world, 3);
        for &entity in &entities {
            world.signal::<Kept>().send(Kept(entity), ..);
        }
        world.entity_manager().despawn(entities[1]);

        for _ in 0..3 {
            end_tick(&world);
        }

        let alive: Vec<Entity> = world.signal::<Kept>().receive().map(|s| s.0).collect();
        assert_eq!(alive, vec![entities[0], entities[2]]);
        let signals = world.get_resource::<Signals<Kept>>().unwrap();
        assert_eq!(signals.permanent.len(), 2);
    }

    #[test]
    fn sweep_budget_is_shared_and_reaches_every_type() {
        let world = kept_world();
        let entities = spawn_all(&world, 2);
        world.signal::<Kept>().send(Kept(entities[0]), ..);
        world.signal::<Kept>().send(Kept(entities[1]), ..);
        world.signal::<OtherKept>().send(OtherKept(entities[0]), ..);
        world.signal::<OtherKept>().send(OtherKept(entities[1]), ..);
        world.entity_manager().despawn(entities[0]);

        for _ in 0..3 {
            end_tick(&world);
        }

        let kept = world.get_resource::<Signals<Kept>>().unwrap();
        let other = world.get_resource::<Signals<OtherKept>>().unwrap();
        assert_eq!(kept.permanent.len(), 1);
        assert_eq!(other.permanent.len(), 1);
    }

    #[test]
    fn large_list_is_swept_in_steps_no_bigger_than_the_budget() {
        let world = kept_world();
        let count = SWEEP_BUDGET as usize * 2 + 100;
        let entities = spawn_all(&world, count);
        for &entity in &entities {
            world.signal::<Kept>().send(Kept(entity), ..);
        }
        end_tick(&world);
        end_tick(&world);
        for &entity in &entities {
            world.entity_manager().despawn(entity);
        }

        end_tick(&world);
        let read = world
            .get_resource::<Signals<Kept>>()
            .unwrap()
            .permanent_read as u64;
        assert!(read > 0 && read < SWEEP_BUDGET);

        for _ in 0..3 {
            end_tick(&world);
        }
        let signals = world.get_resource::<Signals<Kept>>().unwrap();
        assert!(signals.permanent.is_empty());
    }

    fn idle_world(count: usize) -> (World, Vec<Entity>) {
        let world = kept_world();
        let entities = spawn_all(&world, count);
        for &entity in &entities {
            world.signal::<Kept>().send(Kept(entity), ..);
        }
        end_tick(&world);
        end_tick(&world);
        while world.get_resource::<SweepCursor>().unwrap().running
            || world.get_resource::<SweepCursor>().unwrap().woken
        {
            end_tick(&world);
        }
        (world, entities)
    }

    #[test]
    fn sweep_stays_idle_until_something_needs_it() {
        let (world, entities) = idle_world(SWEEP_BUDGET as usize * 2);

        for _ in 0..3 {
            end_tick(&world);
            assert!(!world.get_resource::<SweepCursor>().unwrap().running);
            assert_eq!(
                world
                    .get_resource::<Signals<Kept>>()
                    .unwrap()
                    .permanent_read,
                0
            );
        }

        world.entity_manager().despawn(entities[0]);
        end_tick(&world);

        assert!(world.get_resource::<SweepCursor>().unwrap().running);
        assert!(
            world
                .get_resource::<Signals<Kept>>()
                .unwrap()
                .permanent_read
                > 0
        );
    }

    #[test]
    fn new_permanent_signal_does_not_wake_the_sweep() {
        let (world, entities) = idle_world(2);
        world.signal::<Kept>().send(Kept(entities[0]), ..);
        for _ in 0..3 {
            end_tick(&world);
            let cursor = world.get_resource::<SweepCursor>().unwrap();
            assert!(!cursor.running && !cursor.woken);
        }

        assert_eq!(
            world
                .get_resource::<Signals<Kept>>()
                .unwrap()
                .permanent
                .len(),
            3
        );
    }

    #[test]
    fn signal_sent_to_a_dead_entity_while_idle_is_removed() {
        let (world, entities) = idle_world(2);
        world.entity_manager().despawn(entities[0]);
        for _ in 0..3 {
            end_tick(&world);
        }
        assert_eq!(
            world
                .get_resource::<Signals<Kept>>()
                .unwrap()
                .permanent
                .len(),
            1
        );

        world.signal::<Kept>().send(Kept(entities[0]), ..);
        for _ in 0..3 {
            end_tick(&world);
        }

        assert_eq!(
            world
                .get_resource::<Signals<Kept>>()
                .unwrap()
                .permanent
                .len(),
            1
        );
    }

    #[test]
    fn cancel_while_idle_is_removed() {
        let (world, _) = idle_world(2);

        assert!(world.signal::<Kept>().cancel(0));
        for _ in 0..2 {
            end_tick(&world);
        }

        let signals = world.get_resource::<Signals<Kept>>().unwrap();
        assert_eq!(signals.permanent.len(), 1);
        assert_eq!(signals.permanent_cancelled, 0);
    }

    #[test]
    fn death_behind_the_sweep_starts_another_lap() {
        let (world, entities) = idle_world(SWEEP_BUDGET as usize * 2);
        world.entity_manager().despawn(entities[entities.len() - 1]);
        end_tick(&world);
        assert!(
            world
                .get_resource::<Signals<Kept>>()
                .unwrap()
                .permanent_read
                > 0
        );

        world.entity_manager().despawn(entities[0]);
        for _ in 0..8 {
            end_tick(&world);
        }

        assert_eq!(
            world
                .get_resource::<Signals<Kept>>()
                .unwrap()
                .permanent
                .len(),
            entities.len() - 2
        );
        assert!(!world.get_resource::<SweepCursor>().unwrap().running);
    }

    #[test]
    fn save_during_a_lap_restores_the_same_run() {
        let world = kept_world();
        let count = SWEEP_BUDGET as usize * 2;
        let entities = spawn_all(&world, count);
        for &entity in &entities {
            world.signal::<Kept>().send(Kept(entity), ..);
        }
        end_tick(&world);
        end_tick(&world);
        for &entity in entities.iter().step_by(2) {
            world.entity_manager().despawn(entity);
        }
        end_tick(&world);
        assert!(
            world
                .get_resource::<Signals<Kept>>()
                .unwrap()
                .permanent_read
                > 0
        );

        let mut restored = kept_world();
        restored.load(&world.save());
        assert_eq!(restored.save(), world.save());

        for _ in 0..3 {
            end_tick(&world);
            end_tick(&restored);
            assert_eq!(restored.save(), world.save());
        }
        let signals = restored.get_resource::<Signals<Kept>>().unwrap();
        assert_eq!(signals.permanent.len(), count / 2);
    }

    fn mixed_lists_world() -> World {
        let world = world();
        for payload in 100..110 {
            send_open(&world, payload);
        }
        run_ticks(&world, 2);
        assert_eq!(sweep(&world, 4, |s| s.0 != 101), (4, false));
        assert!(cancel(&world, 0));
        assert!(cancel(&world, 5));

        receive(&world);
        send(&world, 7, 2..7);
        let cancelled = send(&world, 11, 2..7);
        world.signal::<NumSignal>().emit(NumSignal(8));
        end_tick(&world);

        assert!(cancel(&world, cancelled));
        send(&world, 9, 3..6);
        world.signal::<NumSignal>().emit(NumSignal(10));
        world
    }

    fn receive_by_next(world: &World) -> Vec<u64> {
        let mut signal = world.signal::<NumSignal>();
        let mut received = signal.receive();
        let mut payloads = Vec::new();
        while let Some(s) = received.next() {
            payloads.push(s.0);
        }
        payloads
    }

    #[test]
    fn fold_delivers_the_same_signals_as_next() {
        let by_next = receive_by_next(&mixed_lists_world());
        let by_fold: Vec<u64> = mixed_lists_world().signal::<NumSignal>().receive().fold(
            Vec::new(),
            |mut payloads, s| {
                payloads.push(s.0);
                payloads
            },
        );

        assert_eq!(
            by_next,
            vec![8, 9, 10, 7, 102, 103, 104, 106, 107, 108, 109]
        );
        assert_eq!(by_fold, by_next);
    }

    #[test]
    fn fold_after_partial_next_delivers_the_rest() {
        let world = mixed_lists_world();
        let mut signal = world.signal::<NumSignal>();
        let mut received = signal.receive();
        let mut payloads: Vec<u64> = (0..4).map(|_| received.next().unwrap().0).collect();
        payloads = received.fold(payloads, |mut payloads, s| {
            payloads.push(s.0);
            payloads
        });

        assert_eq!(payloads, receive_by_next(&mixed_lists_world()));
    }

    #[test]
    fn signal_storage_roundtrips_through_registry() {
        let world = world();
        world
            .signal::<NumSignal>()
            .send(NumSignal(42), (At(1), To(2)));

        let entry = get_registry_entry(Signals::<NumSignal>::TYPE_UUID).unwrap();
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        let bytes = (entry.serialize)(&*signals as &dyn std::any::Any);
        let restored = (entry.deserialize)(&bytes)
            .downcast::<Signals<NumSignal>>()
            .unwrap();

        assert_eq!(restored.transitory_pending.len(), 1);
        assert_eq!(restored.transitory_pending[0].1.payload, NumSignal(42));
    }
}
