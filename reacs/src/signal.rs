//! Signals: messages between systems, lasting one tick or a window of ticks.

use std::{any::TypeId, marker::PhantomData, ops::RangeFull};

use crate::registry::Registry;

use reacs_derive::{param, resource};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    bit_set::BitSet,
    hashed::Hashed,
    registry::get_registry_entry,
    resource::Resource,
    schedule::{CurrentSystem, Mode, Mut, ParamInit, ParamWorld, Read, WorldAccess, Write},
    tick::TickCount,
    world::World,
};

/// How much work the sweep of dead signals may do per tick: each signal type visited and each
/// open-ended signal checked costs one.
///
/// The sweep removes open-ended signals that their filter dropped, like those of despawned
/// entities, a little each tick, so its cost per tick stays flat whatever the number of signals.
/// Changing it changes on which tick signals leave memory, and so the order some signals arrive in
/// and the contents of saves.
pub const SWEEP_BUDGET: u64 = if cfg!(miri) { 256 } else { 65536 };

/// Decides which signals of type `S` are still delivered.
///
/// [`SignalType::Filter`] picks it: plain `#[signal]` types use [`NoFilter`], and
/// `#[signal(entity)]` types use [`AliveFilter`](crate::entity::AliveFilter). Receiving checks every
/// signal against it, and the sweep removes open-ended signals it drops. Game code doesn't
/// implement it.
pub trait SignalFilter<S>: 'static {
    /// `false` for a filter that keeps every signal, so receiving skips it entirely.
    const ACTIVE: bool;
    /// What [`keep`](Self::keep) reads, fetched once per receive.
    type Context<'w>;

    /// Fetches the context from the world.
    fn fetch<'w>(world: &'w World) -> Self::Context<'w>;
    /// Returns `true` if `signal` should still be delivered.
    ///
    /// Once it returns `false` for a signal, it must keep doing so: the sweep removes that signal
    /// from memory for good.
    fn keep(context: &Self::Context<'_>, signal: &S) -> bool;
    /// Returns a number that changes whenever [`keep`](Self::keep) may have started dropping a
    /// signal, like the count of despawned entities. The sweep stays idle while it doesn't change.
    fn stamp(world: &World) -> u64;
}

/// The filter of plain `#[signal]` types: it keeps every signal, and compiles away.
pub struct NoFilter;

impl<S> SignalFilter<S> for NoFilter {
    const ACTIVE: bool = false;
    type Context<'w> = ();

    #[inline]
    fn fetch(_world: &World) {}

    #[inline]
    fn keep(_context: &(), _signal: &S) -> bool {
        true
    }

    #[inline]
    fn stamp(_world: &World) -> u64 {
        0
    }
}

/// A type declared with `#[signal]`.
///
/// Implemented by the macro; game code doesn't implement it by hand.
pub trait SignalType:
    'static + Copy + Default + Hashed + Serialize + DeserializeOwned + crate::save::Save
{
    /// Decides which signals are still delivered: none are dropped for a plain `#[signal]`, and
    /// signals of a despawned entity are dropped for `#[signal(entity)]`.
    type Filter: SignalFilter<Self>;
}

/// A resolved window: the ticks `start..end` a signal is delivered on.
///
/// Produced by [`IntoWindow`]; game code writes windows with [`In`], [`At`], [`For`] and [`To`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, crate::save::Save)]
#[repr(C)]
pub struct SignalWindow {
    /// The first tick of the window.
    pub start: u64,
    /// The tick after the last one, or `u64::MAX` for a window that lasts until cancelled.
    pub end: u64,
}

/// A window start: `n` ticks from now.
///
/// Alone, `In(n)` is a one-tick signal delivered `n` ticks from the current tick; `In(0)` is now.
/// Pair it with an end for a longer window: `(In(2), For(3))`, or `(In(2), ..)` until cancelled.
/// See [`IntoWindow`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct In(pub u64);

/// A window start: an absolute tick.
///
/// Alone, `At(tick)` is a one-tick signal delivered on that tick. Pair it with an end for a longer
/// window: `(At(40), To(50))`, or `(At(40), ..)` until cancelled. See [`IntoWindow`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct At(pub u64);

/// A window end: `n` ticks long, counted from the start.
///
/// Alone, `For(n)` starts now: `For(5)` is this tick and the next 4. `For(0)` is an empty window,
/// and `For(u64::MAX)` lasts until cancelled, like `..`. See [`IntoWindow`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct For(pub u64);

/// A window end: an absolute tick, included.
///
/// Alone, `To(tick)` starts now: `To(50)` runs from this tick through tick 50. See
/// [`IntoWindow`].
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
/// The ticks a signal is delivered on, given to [`Sender::send`] and [`Signal::send`].
///
/// A window has a start and an end. Starts are [`In(n)`](In) (`n` ticks from now) and
/// [`At(tick)`](At) (an absolute tick); ends are [`For(n)`](For) (`n` ticks long) and
/// [`To(tick)`](To) (an absolute tick, included). `In` and `For` are relative, `At` and `To`
/// absolute.
///
/// | Window             | Delivered on                         |
/// |--------------------|--------------------------------------|
/// | `In(2)`            | one tick, 2 ticks from now           |
/// | `At(40)`           | tick 40 only                         |
/// | `For(5)`           | this tick and the next 4             |
/// | `To(50)`           | this tick through tick 50            |
/// | `(In(2), For(3))`  | 3 ticks, starting 2 ticks from now   |
/// | `(At(40), To(50))` | ticks 40 through 50                  |
/// | `(In(2), To(50))`  | 2 ticks from now through tick 50     |
/// | `(At(40), For(3))` | 3 ticks, starting at tick 40         |
/// | `(In(2), ..)`      | from 2 ticks from now until cancelled |
/// | `..`               | from now until cancelled             |
///
/// Any start pairs with any end. Anything else, like `(For(3), In(2))` or `0..5`, doesn't compile.
/// "Now" is the tick the sending system runs on.
///
/// A window that has already ended (`At(2)` on tick 3) or is empty (`For(0)`) is rejected: `send`
/// returns `None` and sends nothing.
///
/// A window that lasts until cancelled stays in memory, and in saves, until it's cancelled, or
/// until its entity is despawned for a `#[signal(entity)]` type. Long windows of plain signals
/// that nothing cancels pile up.
pub trait IntoWindow {
    /// Resolves the window against the current tick, `now`.
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
    /// Returns `true` if `tick` is in the window.
    #[inline]
    pub fn contains(&self, tick: u64) -> bool {
        self.start <= tick && tick < self.end
    }

    /// Returns `true` if the signal holding this window was cancelled.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.start == u64::MAX
    }

    #[inline]
    fn cancel(&mut self) {
        self.start = u64::MAX;
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, crate::save::Save)]
#[serde(bound = "S: SignalType")]
#[repr(C)]
pub(crate) struct SignalInstance<S> {
    pub(crate) id: u64,
    pub(crate) payload: S,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, crate::save::Save)]
#[repr(C)]
pub(crate) struct ReceiveCursor {
    pub(crate) system: u128,
    pub(crate) next: u64,
    pub(crate) windows_tick: u64,
}

/// Where each system is in receiving signal type `S`, and the next signal id. Saved with the
/// world.
///
/// One of the two resources a signal type is stored in, with [`Signals`]. Systems use it through
/// [`Receiver`], [`Sender`] and [`Signal`]; its contents are private.
#[derive(Default, Hashed, Serialize, Deserialize, crate::save::Save)]
#[serde(bound = "")]
pub struct SignalData<S: SignalType> {
    pub(crate) cursors: Vec<ReceiveCursor>,
    pub(crate) next: u64,
    phantom: PhantomData<S>,
}

impl<S: SignalType> SignalData<S> {
    #[cfg(any(test, feature = "invariants"))]
    pub(crate) fn check_invariants(&self) {
        let cursors = &self.cursors;
        assert!(
            cursors.windows(2).all(|w| w[0].system < w[1].system),
            "cursors out of order"
        );
        assert!(
            cursors.iter().all(|c| c.next <= self.next),
            "cursor past the next id"
        );
    }
}

impl<S: SignalType> Signals<S> {
    /// The slower rule of the two ways a signal is stored: alone and with its window.
    #[doc(hidden)]
    pub const STORAGE_RULE: crate::save::Rule = crate::save::Rule::of::<SignalInstance<S>>()
        .slower(crate::save::Rule::of::<WindowEntry<S>>());
}

impl<S: SignalType> SignalData<S> {
    /// The rule each receive cursor is saved by.
    #[doc(hidden)]
    pub const CURSOR_RULE: crate::save::Rule = crate::save::Rule::of::<ReceiveCursor>();
}

pub(crate) const WINDOW_CHUNK_TICKS: u64 = 64;
const _: () = assert!(WINDOW_CHUNK_TICKS > 0 && WINDOW_CHUNK_TICKS <= 64);

pub(crate) type WindowEntry<S> = (SignalWindow, SignalInstance<S>);

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

#[derive(Clone, Debug, Serialize, Deserialize, crate::save::Save)]
#[serde(bound = "S: SignalType")]
pub(crate) struct WindowChunk<S> {
    pub(crate) key: u64,
    pub(crate) ticks: u64,
    pub(crate) cancelled: u64,
    pub(crate) head: usize,
    pub(crate) by_end: bool,
    pub(crate) due: bool,
    pub(crate) entries: Vec<WindowEntry<S>>,
}

impl<S: Copy> WindowChunk<S> {
    #[inline]
    pub(crate) fn live(&self) -> &[WindowEntry<S>] {
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
    Running(usize, usize),
    Waiting(usize, usize),
}

/// Every signal of type `S` in flight, grouped by how long it lasts. Saved with the world.
///
/// One of the two resources a signal type is stored in, with [`SignalData`]. Its id is the one
/// recordings name input signals by ([`RecordedInputs::signal`](crate::record::RecordedInputs)).
/// Systems use it through [`Receiver`], [`Sender`] and [`Signal`]; its contents are private.
#[derive(Default, Hashed, Serialize, Deserialize, crate::save::Save)]
#[serde(bound = "")]
pub struct Signals<S: SignalType> {
    pub(crate) transient: Vec<SignalInstance<S>>,
    pub(crate) transient_buffer: Vec<SignalInstance<S>>,

    pub(crate) transitory_pending: Vec<WindowEntry<S>>,
    pub(crate) transitory_running: Vec<WindowChunk<S>>,
    pub(crate) transitory_waiting: Vec<WindowChunk<S>>,
    pub(crate) transitory_compact: Vec<(bool, u64)>,
    pub(crate) transitory_wake: u64,

    pub(crate) permanent_joining: Vec<WindowEntry<S>>,
    #[serde(skip)]
    pub(crate) window_spare: Vec<Vec<WindowEntry<S>>>,
    #[serde(skip)]
    pub(crate) window_started: Vec<WindowEntry<S>>,
    pub(crate) permanent: Vec<SignalInstance<S>>,
    pub(crate) permanent_write: usize,
    pub(crate) permanent_read: usize,
    pub(crate) permanent_cancelled: u64,
    pub(crate) permanent_cancelled_bits: BitSet,
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

#[inline(never)]
fn retain_instances<S: Copy>(
    list: &mut Vec<SignalInstance<S>>,
    start: usize,
    keep: &impl Fn(&S) -> bool,
    f: &mut impl FnMut(&S) -> bool,
    mut cancel: impl FnMut(u64) -> bool,
) -> u64 {
    let mut positions = Vec::new();
    for (i, signal) in list[start..].iter().enumerate() {
        if keep(&signal.payload) && !f(&signal.payload) && cancel(signal.id) {
            positions.push(start + i);
        }
    }
    let Some(&first) = positions.first() else {
        return 0;
    };

    let len = list.len();
    let mut write = first;
    for (k, &position) in positions.iter().enumerate() {
        let next = positions.get(k + 1).copied().unwrap_or(len);
        list.copy_within(position + 1..next, write);
        write += next - position - 1;
    }
    list.truncate(write);
    positions.len() as u64
}

#[inline(never)]
fn retain_entries<S: Copy>(
    entries: &mut [WindowEntry<S>],
    next_tick: u64,
    keep: &impl Fn(&S) -> bool,
    f: &mut impl FnMut(&S) -> bool,
) -> u64 {
    let mut cancelled = 0;
    for entry in entries {
        if !entry.0.is_cancelled()
            && keep(&entry.1.payload)
            && !f(&entry.1.payload)
            && entry.0.end > next_tick
        {
            entry.0.cancel();
            cancelled += 1;
        }
    }
    cancelled
}

#[inline(never)]
fn retain_permanent<S>(
    permanent: &[SignalInstance<S>],
    range: std::ops::Range<usize>,
    bits: &mut BitSet,
    skip_bits: bool,
    keep: &impl Fn(&S) -> bool,
    f: &mut impl FnMut(&S) -> bool,
) -> u64 {
    let mut cancelled = 0;
    let mut position = range.start;
    while position < range.end {
        let next = match skip_bits {
            true => bits.next_set(position, range.end),
            false => range.end,
        };
        for (i, signal) in permanent[position..next].iter().enumerate() {
            if keep(&signal.payload) && !f(&signal.payload) {
                bits.set(position + i, true);
                cancelled += 1;
            }
        }
        position = next + 1;
    }
    cancelled
}

enum ReceiveStage {
    TransientBuffer,
    Running,
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
    running: &'a [WindowChunk<S>],
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
                    self.stage = ReceiveStage::Running;
                }
                ReceiveStage::Running => {
                    if let Some((chunk, rest)) = self.running.split_first() {
                        self.entries = chunk.live().iter();
                        self.running = rest;
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
                    self.stage = ReceiveStage::Running;
                }
                ReceiveStage::Running => {
                    for chunk in self.running {
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
    pub(crate) fn send(
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
    pub(crate) fn emit(&mut self, data: &mut SignalData<S>, signal: S) -> u64 {
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
                let (c, i) = find_in_chunks(&self.transitory_running, id)?;
                Some(WindowSlot::Running(c, i))
            })
            .or_else(|| {
                let (c, i) = find_in_chunks(&self.transitory_waiting, id)?;
                Some(WindowSlot::Waiting(c, i))
            })
    }

    fn windowed_mut(&mut self, slot: &WindowSlot) -> &mut WindowEntry<S> {
        match *slot {
            WindowSlot::Pending(i) => &mut self.transitory_pending[i],
            WindowSlot::Joining(i) => &mut self.permanent_joining[i],
            WindowSlot::Running(c, i) => &mut self.transitory_running[c].entries[i],
            WindowSlot::Waiting(c, i) => &mut self.transitory_waiting[c].entries[i],
        }
    }

    fn find_permanent(&self, id: u64) -> Option<usize> {
        let search = |list: &[SignalInstance<S>]| list.binary_search_by(|x| x.id.cmp(&id)).ok();
        search(&self.permanent[..self.permanent_write])
            .or_else(|| Some(search(&self.permanent[self.permanent_read..])? + self.permanent_read))
    }

    pub(crate) fn modify(&mut self, id: u64, f: impl FnOnce(&mut S)) -> bool {
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

    pub(crate) fn cancel(&mut self, id: u64) -> bool {
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
            window.cancel();
            found = true;

            if let WindowSlot::Running(c, _) | WindowSlot::Waiting(c, _) = slot {
                let running = matches!(slot, WindowSlot::Running(..));
                let chunks = match running {
                    true => &mut self.transitory_running,
                    false => &mut self.transitory_waiting,
                };
                let chunk = &mut chunks[c];
                chunk.cancelled += 1;
                if !chunk.due && chunk.cancelled * 2 > chunk.live().len() as u64 {
                    chunk.due = true;
                    self.transitory_compact.push((running, chunk.key));
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

    pub(crate) fn pending(&self, data: &SignalData<S>, system: u128, tick: u64) -> bool {
        let running = !self.transitory_running.is_empty()
            || !self.permanent_joining.is_empty()
            || self.permanent_write > 0
            || self.permanent_read < self.permanent.len();
        if !running && self.transient_buffer.is_empty() && self.transient.is_empty() {
            return false;
        }

        let (cursor, windows) = match data.cursors.binary_search_by_key(&system, |x| x.system) {
            Ok(i) => (data.cursors[i].next, data.cursors[i].windows_tick != tick),
            Err(_) => (0, true),
        };
        let unread = |list: &[SignalInstance<S>]| list.last().is_some_and(|x| x.id >= cursor);

        unread(&self.transient_buffer) || unread(&self.transient) || windows && running
    }

    #[inline(always)]
    pub(crate) fn receive<'a, K: Fn(&S) -> bool>(
        &'a self,
        data: &mut SignalData<S>,
        system: u128,
        tick: u64,
        keep: K,
    ) -> impl Iterator<Item = &'a S> + use<'a, S, K> {
        self.receive_iter(data, system, tick, keep)
    }

    /// Receives like [`receive`](Self::receive), calls `f` on each signal and cancels every
    /// signal `f` returns `false` for. Returns how many it cancelled.
    ///
    /// Walks the lists `receive` would, in the same order, and cancels each signal where it
    /// finds it, as [`cancel`](Self::cancel) would, but only signals with ticks left after this
    /// one: windows are marked in place and permanent signals get their cancelled bit. A signal
    /// on its last tick (every one-tick signal, and windows ending now) is left alone. In
    /// `transient_buffer`, only copies of windows sent this tick have ticks left: such a copy
    /// leaves the list (one compacting pass) and its window in `transitory_pending` is
    /// cancelled. Kept apart from `ReceiveIter` so its hot path is unchanged.
    pub(crate) fn retain<K: Fn(&S) -> bool>(
        &mut self,
        data: &mut SignalData<S>,
        system: u128,
        tick: u64,
        keep: K,
        mut f: impl FnMut(&S) -> bool,
    ) -> usize {
        let (transient, buffer, windows) = self.advance_cursor(data, system, tick);
        let transient = self.transient.len() - transient.len();
        let buffer = self.transient_buffer.len() - buffer.len();
        // `transient` only holds one-tick signals on their last tick, so none is cancelled.
        retain_instances(&mut self.transient, transient, &keep, &mut f, |_| false);
        // In the buffer, only copies of windows sent this tick have ticks left.
        let pending = &mut self.transitory_pending;
        let mut cancelled =
            retain_instances(&mut self.transient_buffer, buffer, &keep, &mut f, |id| {
                let found = pending.binary_search_by(|x| x.1.id.cmp(&id));
                found.map(|i| pending[i].0.cancel()).is_ok()
            });
        if !windows {
            return cancelled as usize;
        }

        for chunk in &mut self.transitory_running {
            let head = chunk.head;
            let count = retain_entries(&mut chunk.entries[head..], tick + 1, &keep, &mut f);
            if count > 0 {
                chunk.cancelled += count;
                cancelled += count;
                if !chunk.due && chunk.cancelled * 2 > chunk.live().len() as u64 {
                    chunk.due = true;
                    self.transitory_compact.push((true, chunk.key));
                }
            }
        }
        cancelled += retain_entries(&mut self.permanent_joining, tick + 1, &keep, &mut f);

        let skip_bits = self.permanent_cancelled > 0;
        let mut permanent = 0;
        for range in [
            0..self.permanent_write,
            self.permanent_read..self.permanent.len(),
        ] {
            permanent += retain_permanent(
                &self.permanent,
                range,
                &mut self.permanent_cancelled_bits,
                skip_bits,
                &keep,
                &mut f,
            );
        }
        self.permanent_cancelled += permanent;
        (cancelled + permanent) as usize
    }

    /// Moves `system`'s cursor past everything it receives now. Returns its unread signals in
    /// `transient` and `transient_buffer`, and whether it gets this tick's windows.
    ///
    /// Returns slices, not positions, so `receive` compiles as it did before `retain` shared this
    /// code: re-slicing by position added bounds checks and slowed `for` loops over entity
    /// signals by about 10%.
    #[inline(always)]
    fn advance_cursor<'a>(
        &'a self,
        data: &mut SignalData<S>,
        system: u128,
        tick: u64,
    ) -> (&'a [SignalInstance<S>], &'a [SignalInstance<S>], bool) {
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
        (transient, transient_buffer, windows)
    }

    #[inline(always)]
    fn receive_iter<'a, K: Fn(&S) -> bool>(
        &'a self,
        data: &mut SignalData<S>,
        system: u128,
        tick: u64,
        keep: K,
    ) -> ReceiveIter<'a, S, K> {
        let (transient, transient_buffer, windows) = self.advance_cursor(data, system, tick);

        let mut iter = ReceiveIter {
            run: transient.iter(),
            stage: ReceiveStage::TransientBuffer,
            transient_buffer,
            entries: [].iter(),
            running: &self.transitory_running,
            joining: &self.permanent_joining,
            permanent: &self.permanent,
            write: self.permanent_write,
            read: self.permanent_read,
            position: 0,
            cancelled: (self.permanent_cancelled > 0).then_some(&self.permanent_cancelled_bits),
            keep,
        };
        if !windows {
            iter.running = &[];
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

        debug_assert!(self.permanent_write <= self.permanent_read);
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

    fn expire_running(&mut self, next_tick: u64) {
        let key = chunk_key(next_tick);
        let expired = self
            .transitory_running
            .partition_point(|chunk| chunk.key < key);
        for chunk in self.transitory_running.drain(..expired) {
            recycle(&mut self.window_spare, chunk.entries);
        }

        let Some(chunk) = self.transitory_running.first_mut() else {
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
            let chunk = self.transitory_running.remove(0);
            recycle(&mut self.window_spare, chunk.entries);
        }
    }

    fn compact_cancelled(&mut self) {
        for (running, key) in std::mem::take(&mut self.transitory_compact) {
            let chunks = match running {
                true => &mut self.transitory_running,
                false => &mut self.transitory_waiting,
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

    fn start_waiting(&mut self, next_tick: u64, started: &mut Vec<WindowEntry<S>>) {
        let key = chunk_key(next_tick);
        let through = chunk_bits_through(next_tick);

        while let Some(chunk) = self.transitory_waiting.first_mut() {
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
            let chunk = self.transitory_waiting.remove(0);
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
            let chunk = chunk_mut(&mut self.transitory_waiting, &mut self.window_spare, key);
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
            let chunk = chunk_mut(&mut self.transitory_running, &mut self.window_spare, key);
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

    #[doc(hidden)]
    pub fn permanent_len(&self) -> usize {
        self.permanent.len()
    }

    pub(crate) fn cleanup(&mut self, tick: u64, keep: impl Fn(&S) -> bool) {
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

        #[cfg(any(test, feature = "invariants"))]
        self.check_invariants(next_tick);
    }

    #[cfg(any(test, feature = "invariants"))]
    pub(crate) fn check_invariants(&self, next_tick: u64) {
        let by_id = |list: &[SignalInstance<S>]| list.windows(2).all(|w| w[0].id < w[1].id);
        assert!(by_id(&self.transient), "transient out of id order");
        assert!(
            self.transient_buffer.is_empty(),
            "transient_buffer kept after cleanup"
        );
        assert!(
            self.transitory_pending.is_empty(),
            "pending windows kept after cleanup"
        );
        assert!(
            self.transitory_compact.is_empty(),
            "compaction queue kept after cleanup"
        );

        for (running, chunks) in [
            (true, &self.transitory_running),
            (false, &self.transitory_waiting),
        ] {
            assert!(
                chunks.windows(2).all(|w| w[0].key < w[1].key),
                "chunks out of key order"
            );
            for chunk in chunks {
                let key = chunk.key;
                let live = chunk.live();
                assert!(
                    chunk.head <= chunk.entries.len(),
                    "chunk {key}: head past its entries"
                );
                assert!(!live.is_empty(), "chunk {key}: empty chunk kept");
                assert!(!chunk.due, "chunk {key}: compaction left due");
                assert!(
                    live.windows(2).all(|w| w[0].1.id < w[1].1.id),
                    "chunk {key}: out of id order"
                );
                if running && chunk.by_end {
                    assert!(
                        live.is_sorted_by_key(|e| e.0.end),
                        "chunk {key}: by_end but not in end order"
                    );
                }
                let cancelled = live.iter().filter(|e| e.0.is_cancelled()).count() as u64;
                assert_eq!(chunk.cancelled, cancelled, "chunk {key}: cancelled count");
                for (window, signal) in live {
                    let id = signal.id;
                    assert!(window.end > next_tick, "chunk {key}: {id} ended but kept");
                    if !window.is_cancelled() {
                        assert!(
                            window.start < window.end,
                            "chunk {key}: {id} has an empty window"
                        );
                        assert_eq!(
                            window.start <= next_tick,
                            running,
                            "chunk {key}: {id} in the wrong list"
                        );
                    }
                    if running || !window.is_cancelled() {
                        let tick = if running { window.end } else { window.start };
                        assert_eq!(chunk_key(tick), key, "chunk {key}: {id} in the wrong chunk");
                        assert!(
                            chunk.ticks & chunk_bit(tick) != 0,
                            "chunk {key}: {id} has no mask bit"
                        );
                        assert!(
                            tick >= self.transitory_wake,
                            "chunk {key}: {id} before transitory_wake"
                        );
                    }
                }
            }
        }

        let joining = &self.permanent_joining;
        assert!(
            joining.windows(2).all(|w| w[0].1.id < w[1].1.id),
            "permanent_joining out of id order"
        );
        assert!(
            joining
                .iter()
                .all(|e| e.0.end == u64::MAX && !e.0.is_cancelled()),
            "permanent_joining holds a finite or cancelled window"
        );

        let (write, read, len) = (
            self.permanent_write,
            self.permanent_read,
            self.permanent.len(),
        );
        assert!(
            write <= read && read <= len,
            "sweep gap {write}..{read} of {len}"
        );
        let (checked, unchecked) = (&self.permanent[..write], &self.permanent[read..]);
        assert!(
            by_id(checked) && by_id(unchecked),
            "permanent out of id order"
        );
        if let (Some(a), Some(b)) = (checked.last(), unchecked.first()) {
            assert!(a.id < b.id, "permanent out of id order across the gap");
        }
        let bits = &self.permanent_cancelled_bits;
        assert!(
            (write..read).all(|i| !bits.get(i)),
            "cancel bit left in the sweep gap"
        );
        let cancelled = (0..write).chain(read..len).filter(|&i| bits.get(i)).count() as u64;
        assert_eq!(
            self.permanent_cancelled, cancelled,
            "permanent cancelled count"
        );
    }

    fn cleanup_windows(&mut self, next_tick: u64, keep: impl Fn(&S) -> bool) {
        self.drop_transient_copies();
        if !self.transitory_compact.is_empty() {
            self.compact_cancelled();
        }
        self.expire_running(next_tick);

        let mut started = std::mem::take(&mut self.window_started);
        self.start_waiting(next_tick, &mut started);
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

        self.transitory_wake = Self::first_bit_tick(&self.transitory_running)
            .min(Self::first_bit_tick(&self.transitory_waiting));
    }
}

#[doc(hidden)]
pub fn wake_init<S: SignalType>(world: ParamInit<'_>) {
    world.0.init_signal::<S>();
}

#[doc(hidden)]
#[inline]
pub fn wake_pending<S: SignalType>(world: ParamWorld<'_>, system: u128, tick: u64) -> bool {
    let signals = Read::fetch::<Signals<S>>(world);
    let data = Read::fetch::<SignalData<S>>(world);
    signals.pending(&data, system, tick)
}

#[doc(hidden)]
pub fn wake_name<S: SignalType>(names: &mut Vec<&'static str>) {
    let name = std::any::type_name::<S>();
    names.push(name.rsplit("::").next().unwrap_or(name));
}

/// System param that receives and sends signals of type `S`: `Mut<Signal<S>>`.
///
/// It has every method of [`Receiver`] and [`Sender`], for a system that both receives and sends
/// the same type. A system holding a `Signal` runs every tick, even with nothing to receive, so
/// prefer `Receiver` or `Sender` alone when the system only does one of the two.
///
/// # Examples
///
/// An alarm that rings every 3 ticks, each ring setting the next one:
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[signal]
/// struct Alarm;
///
/// #[resource]
/// struct Rings {
///     ticks: Vec<u64>,
/// }
///
/// #[system]
/// fn alarm(mut alarm: Mut<Signal<Alarm>>, mut rings: Mut<Resource<Rings>>) {
///     if alarm.receive().next().is_some() {
///         rings.ticks.push(alarm.tick());
///         alarm.send(Alarm, In(3));
///     }
/// }
///
/// fn main() {
///     let mut world = World::build(|setup| {
///         setup.signal::<Alarm>().emit(Alarm);
///     });
///     for _ in 0..10 {
///         world.step();
///     }
///     assert_eq!(world.get_resource::<Rings>().unwrap().ticks, [0, 3, 6, 9]);
/// }
/// ```
#[param(write, wake = always)]
pub struct Signal<S: SignalType> {
    signals: Resource<Signals<S>>,
    data: Resource<SignalData<S>>,
    #[read]
    tick: Resource<TickCount>,
    #[read]
    current: Resource<CurrentSystem>,
    #[read]
    world: WorldAccess,
}

/// System param that receives signals of type `S`: `Mut<Receiver<S>>`.
///
/// A system with a receiver sleeps: it's skipped on ticks where none of its receivers has anything
/// new, so reacting to rare signals costs almost nothing. The whole system is skipped, so it
/// shouldn't do work that doesn't come from its signals (see `#[system]`).
///
/// A receiver can also stop the signals it receives, with [`retain`](Self::retain), and still
/// sleep while nothing is in flight.
///
/// Receiving and sending the same type in one system needs [`Signal`]: a `Receiver<S>` and a
/// `Sender<S>` in the same system is a borrow conflict.
#[param(write, wake = __gate(S))]
pub struct Receiver<S: SignalType> {
    signals: Resource<Signals<S>>,
    data: Resource<SignalData<S>>,
    #[read]
    tick: Resource<TickCount>,
    #[read]
    current: Resource<CurrentSystem>,
    #[read]
    world: WorldAccess,
}

/// System param that sends, modifies and cancels signals of type `S`: `Mut<Sender<S>>`.
///
/// A sender doesn't change when its system runs.
///
/// # Examples
///
/// A bite that poisons a unit for 3 ticks:
///
/// ```rust,standalone_crate
/// use reacs::prelude::*;
///
/// #[component]
/// struct Health(i32);
///
/// #[signal(input)]
/// struct Bite(Entity);
///
/// #[signal(entity)]
/// struct Poison(Entity);
///
/// #[system]
/// fn bite(mut bites: Mut<Receiver<Bite>>, mut poison: Mut<Sender<Poison>>) {
///     for &Bite(unit) in bites.receive() {
///         poison.send(Poison(unit), For(3));
///     }
/// }
///
/// #[system(run_after(bite))]
/// fn hurt(mut poison: Mut<Receiver<Poison>>, mut health: Mut<Component<Health>>) {
///     for &Poison(unit) in poison.receive() {
///         if let Some(hp) = health.get_mut(unit) {
///             hp.0 -= 1;
///         }
///     }
/// }
///
/// fn main() {
///     let mut unit = Entity::default();
///     let mut world = World::build(|setup| {
///         unit = setup.spawn();
///         setup.set_component(unit, Health(10));
///     });
///     world.input(Bite(unit));
///     for _ in 0..5 {
///         world.step();
///     }
///     // Poisoned on ticks 0, 1 and 2.
///     assert_eq!(world.get_component::<Health>(unit).unwrap().0, 7);
/// }
/// ```
#[param(write)]
pub struct Sender<S: SignalType> {
    signals: Resource<Signals<S>>,
    data: Resource<SignalData<S>>,
    #[read]
    tick: Resource<TickCount>,
}

impl<'w, S: SignalType> Signal<'w, S, Write> {
    /// Sends `signal` over a window of ticks, and returns its id.
    ///
    /// The signal is delivered on every tick of its window (see [`IntoWindow`]). On its first
    /// tick, systems that run after this one get it that same tick. Returns `None`, sending
    /// nothing, if the window has already ended or is empty.
    ///
    /// The id works with [`modify`](Self::modify) and [`cancel`](Self::cancel). Ids are never
    /// reused.
    #[inline]
    pub fn send(&mut self, signal: S, window: impl IntoWindow) -> Option<u64> {
        let tick = self.tick();
        self.signals
            .send(&mut self.data, tick, signal, window.into_window(tick))
    }

    /// Sends `signal` for one tick, and returns its id.
    ///
    /// Systems that run after this one get it this tick, the others next tick. The same as
    /// `send(signal, In(0))`, but cheaper.
    #[inline]
    pub fn emit(&mut self, signal: S) -> u64 {
        self.signals.emit(&mut self.data, signal)
    }

    /// Replaces the payload of the signal with id `id`.
    ///
    /// Returns `false`, changing nothing, if the signal doesn't exist, has ended or was
    /// cancelled. Only the payload changes; to change a window, cancel the signal and send a new
    /// one.
    #[inline]
    pub fn modify(&mut self, id: u64, payload: S) -> bool {
        self.signals.modify(id, |current| *current = payload)
    }

    /// Cancels the signal with id `id`, so it isn't delivered again, starting with the systems
    /// that run after this one.
    ///
    /// Returns `false` if the signal doesn't exist, has ended or was already cancelled.
    #[inline]
    pub fn cancel(&mut self, id: u64) -> bool {
        self.signals.cancel(id)
    }

    /// Returns the signals this system hasn't received yet.
    ///
    /// The first call in a tick returns every signal sent since this system last received, plus
    /// every windowed signal delivered this tick. A later call in the same tick only returns
    /// one-tick signals sent since the call before. Signals of a despawned entity's
    /// `#[signal(entity)]` type are skipped, even if it was despawned earlier in the same loop.
    ///
    /// Everything is marked as received when `receive` is called, even if the iterator is dropped
    /// early, so don't call it just to peek. In hot loops, `for_each` is faster than `for`.
    ///
    /// The order is deterministic, but systems shouldn't rely on it. The iterator borrows the
    /// param, so to send the same type while receiving, collect the signals first.
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

    /// Receives like [`receive`](Self::receive), calling `f` on each signal, and cancels every
    /// signal `f` returns `false` for that still has ticks left. Returns how many signals it
    /// cancelled.
    ///
    /// This ends a window on a condition: send the signal with `..` (or a long window), and
    /// return `false` once it should stop, for example when the burning unit runs out of fuel.
    /// The cancel works like [`cancel`](Self::cancel): systems that run after this one no
    /// longer get the signal, this tick included. A signal on its last tick (any one-tick
    /// signal, or a window ending this tick) ends anyway, so `false` leaves it alone and later
    /// systems still get it. `f` sees the signals in the order `receive` gives them and can use
    /// the system's other params.
    ///
    /// Costs the same as `receive().for_each(..)` with the same work in `f`; each cancel adds a
    /// few nanoseconds. Cancelling a window sent this tick also moves the one-tick signals sent
    /// after it this tick, once per call.
    ///
    /// # Examples
    ///
    /// A fire that burns until its unit's fuel runs out:
    ///
    /// ```rust,standalone_crate
    /// use reacs::prelude::*;
    ///
    /// #[component]
    /// struct Fuel(u32);
    ///
    /// #[signal(entity)]
    /// struct Burn(Entity);
    ///
    /// #[system]
    /// fn burn(mut burns: Mut<Signal<Burn>>, mut fuel: Mut<Component<Fuel>>) {
    ///     burns.retain(|&Burn(unit)| {
    ///         let Some(left) = fuel.get_mut(unit) else {
    ///             return false;
    ///         };
    ///         left.0 -= 1;
    ///         left.0 > 0
    ///     });
    /// }
    ///
    /// fn main() {
    ///     let mut unit = Entity::default();
    ///     let mut world = World::build(|setup| {
    ///         unit = setup.spawn();
    ///         setup.set_component(unit, Fuel(3));
    ///         setup.signal::<Burn>().send(Burn(unit), ..);
    ///     });
    ///     for _ in 0..10 {
    ///         world.step();
    ///     }
    ///     // Burned on ticks 0, 1 and 2, then cancelled.
    ///     assert_eq!(world.get_component::<Fuel>(unit).unwrap().0, 0);
    /// }
    /// ```
    pub fn retain(&mut self, f: impl FnMut(&S) -> bool) -> usize {
        let context = S::Filter::fetch(&self.world);
        let system = (*self.current).0;
        let tick = (*self.tick).0;
        self.signals.retain(
            &mut self.data,
            system,
            tick,
            move |signal| S::Filter::keep(&context, signal),
            f,
        )
    }

    /// Returns the current tick.
    #[inline]
    pub fn tick(&self) -> u64 {
        (*self.tick).0
    }
}

impl<'w, S: SignalType> Receiver<'w, S, Write> {
    /// Returns the signals this system hasn't received yet.
    ///
    /// The first call in a tick returns every signal sent since this system last received, plus
    /// every windowed signal delivered this tick. A later call in the same tick only returns
    /// one-tick signals sent since the call before. Signals of a despawned entity's
    /// `#[signal(entity)]` type are skipped, even if it was despawned earlier in the same loop.
    ///
    /// Everything is marked as received when `receive` is called, even if the iterator is dropped
    /// early, so don't call it just to peek. In hot loops, `for_each` is faster than `for`.
    ///
    /// The order is deterministic, but systems shouldn't rely on it. The iterator borrows the
    /// param, so to send the same type while receiving, collect the signals first.
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

    /// Receives like [`receive`](Self::receive), calling `f` on each signal, and cancels every
    /// signal `f` returns `false` for that still has ticks left. Returns how many signals it
    /// cancelled.
    ///
    /// The same as [`Signal::retain`], which has the details and an example, but the system
    /// still sleeps while nothing is in flight.
    ///
    /// # Examples
    ///
    /// Regeneration that stops once a unit is healed, in a system that sleeps while no unit is
    /// regenerating:
    ///
    /// ```rust,standalone_crate
    /// use reacs::prelude::*;
    ///
    /// #[component]
    /// struct Health(u32);
    ///
    /// #[signal(entity)]
    /// struct Regen(Entity);
    ///
    /// #[system]
    /// fn regen(mut regen: Mut<Receiver<Regen>>, mut health: Mut<Component<Health>>) {
    ///     regen.retain(|&Regen(unit)| {
    ///         let Some(hp) = health.get_mut(unit) else {
    ///             return false;
    ///         };
    ///         hp.0 += 10;
    ///         hp.0 < 100
    ///     });
    /// }
    ///
    /// fn main() {
    ///     let mut unit = Entity::default();
    ///     let mut world = World::build(|setup| {
    ///         unit = setup.spawn();
    ///         setup.set_component(unit, Health(70));
    ///         setup.signal::<Regen>().send(Regen(unit), ..);
    ///     });
    ///     for _ in 0..10 {
    ///         world.step();
    ///     }
    ///     assert_eq!(world.get_component::<Health>(unit).unwrap().0, 100);
    /// }
    /// ```
    pub fn retain(&mut self, f: impl FnMut(&S) -> bool) -> usize {
        let context = S::Filter::fetch(&self.world);
        let system = (*self.current).0;
        let tick = (*self.tick).0;
        self.signals.retain(
            &mut self.data,
            system,
            tick,
            move |signal| S::Filter::keep(&context, signal),
            f,
        )
    }

    /// Returns the current tick.
    #[inline]
    pub fn tick(&self) -> u64 {
        (*self.tick).0
    }
}

impl<'w, S: SignalType> Sender<'w, S, Write> {
    /// Sends `signal` over a window of ticks, and returns its id.
    ///
    /// The signal is delivered on every tick of its window (see [`IntoWindow`]). On its first
    /// tick, systems that run after this one get it that same tick. Returns `None`, sending
    /// nothing, if the window has already ended or is empty.
    ///
    /// The id works with [`modify`](Self::modify) and [`cancel`](Self::cancel). Ids are never
    /// reused.
    #[inline]
    pub fn send(&mut self, signal: S, window: impl IntoWindow) -> Option<u64> {
        let tick = self.tick();
        self.signals
            .send(&mut self.data, tick, signal, window.into_window(tick))
    }

    /// Sends `signal` for one tick, and returns its id.
    ///
    /// Systems that run after this one get it this tick, the others next tick. The same as
    /// `send(signal, In(0))`, but cheaper.
    #[inline]
    pub fn emit(&mut self, signal: S) -> u64 {
        self.signals.emit(&mut self.data, signal)
    }

    /// Replaces the payload of the signal with id `id`.
    ///
    /// Returns `false`, changing nothing, if the signal doesn't exist, has ended or was
    /// cancelled. Only the payload changes; to change a window, cancel the signal and send a new
    /// one.
    #[inline]
    pub fn modify(&mut self, id: u64, payload: S) -> bool {
        self.signals.modify(id, |current| *current = payload)
    }

    /// Cancels the signal with id `id`, so it isn't delivered again, starting with the systems
    /// that run after this one.
    ///
    /// Returns `false` if the signal doesn't exist, has ended or was already cancelled.
    #[inline]
    pub fn cancel(&mut self, id: u64) -> bool {
        self.signals.cancel(id)
    }

    /// Returns the current tick.
    #[inline]
    pub fn tick(&self) -> u64 {
        (*self.tick).0
    }
}

static SIGNAL_REGISTRY: Registry<Vec<fn(&World)>> = Registry::new();

/// Sweeps one filtered signal type for at most `budget`; returns the budget used and whether the
/// lap finished.
type SweepFn = fn(&World, u64) -> (u64, bool);

/// Reads a filter's stamp, which changes when signals may have started failing it.
type StampFn = fn(&World) -> u64;

static SWEEP_REGISTRY: Registry<Vec<(u128, SweepFn)>> = Registry::new();

static SWEEP_STAMPS: Registry<Vec<(TypeId, StampFn)>> = Registry::new();

#[resource]
pub(crate) struct SweepCursor {
    pub(crate) type_index: usize,
    pub(crate) running: bool,
    pub(crate) woken: bool,
    pub(crate) seen_stamp: u64,
}

#[doc(hidden)]
pub fn register_signal<S: SignalType>() {
    let name = std::any::type_name::<S>();
    SIGNAL_REGISTRY.add(name, |signals| signals.push(cleanup_in_world::<S>));

    if S::Filter::ACTIVE {
        SWEEP_REGISTRY.add(name, |sweeps| {
            let uuid = Signals::<S>::TYPE_UUID;
            let i = sweeps.partition_point(|x| x.0 < uuid);
            sweeps.insert(i, (uuid, sweep_in_world::<S>));
        });
        SWEEP_STAMPS.add(name, |stamps| {
            let filter = TypeId::of::<S::Filter>();
            if !stamps.iter().any(|x| x.0 == filter) {
                stamps.push((filter, S::Filter::stamp));
            }
        });
    }
}

#[diagnostic::on_unimplemented(
    message = "`{Self}` is not an input signal",
    note = "declare it with #[signal(input)]; other signals are sent from systems or World::build"
)]
/// A signal the host may send with [`World::input`]. Declared with `#[signal(input)]`.
pub trait InputSignal: SignalType {}

fn get_registered_sweeps() -> &'static [(u128, SweepFn)] {
    SWEEP_REGISTRY.get()
}

fn get_registered_stamps() -> &'static [(TypeId, StampFn)] {
    SWEEP_STAMPS.get()
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
    SIGNAL_REGISTRY.get()
}

fn cleanup_in_world<S: SignalType>(world: &World) {
    let (Some(mut signals), Some(tick)) = (
        world.get_resource_mut::<Signals<S>>(),
        world.get_resource::<TickCount>(),
    ) else {
        return;
    };
    let context = (!signals.permanent_joining.is_empty()).then(|| S::Filter::fetch(world));
    signals.cleanup(tick.0, |signal| {
        context
            .as_ref()
            .is_none_or(|context| S::Filter::keep(context, signal))
    });

    if S::Filter::ACTIVE
        && signals.permanent_cancelled > 0
        && let Some(mut cursor) = world.get_resource_mut::<SweepCursor>()
    {
        cursor.woken = true;
    }

    #[cfg(any(test, feature = "invariants"))]
    if let Some(data) = world.get_resource::<SignalData<S>>() {
        data.check_invariants();
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
        self.init_resource::<TickCount>();
    }

    #[inline]
    pub(crate) fn signal<S: SignalType>(&self) -> Mut<Signal<'_, S>> {
        self.fetch_param::<Mut<Signal<S>>>()
    }

    /// Sends an input signal to the world.
    ///
    /// Every system receiving `S` gets it during the next [`step`](Self::step). Inputs are the
    /// only way the host affects a running world, apart from replacing its state with
    /// [`load`](Self::load). A recording stores them tick by tick, along with the starting state
    /// and a full save after each `load`. Only `#[signal(input)]` types can be sent this way; any
    /// other is a compile error.
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
        entity::Entity, prelude::*, registry::get_registry_entry, schedule::CurrentSystem,
        tick::TickCount,
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
        world.get_resource_mut::<TickCount>().unwrap().0 += 1;
    }

    fn receive(world: &World) -> Vec<u64> {
        world.signal::<NumSignal>().receive().map(|s| s.0).collect()
    }

    fn peek(world: &World) -> Vec<u64> {
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        let mut data = world.get_resource_mut::<SignalData<NumSignal>>().unwrap();
        let tick = world.get_resource::<TickCount>().unwrap().0;
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
                end_tick(world);
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
            for chunks in [&signals.transitory_running, &signals.transitory_waiting] {
                assert!(chunks.windows(2).all(|w| w[0].key < w[1].key));
                for chunk in chunks {
                    assert!(chunk.live().windows(2).all(|w| w[0].1.id < w[1].1.id));
                }
            }
        }

        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        let active: Vec<u64> = signals
            .transitory_running
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

    #[test]
    fn pending_is_never_false_when_receive_would_deliver() {
        for seed in [
            0x2545F4914F6CDD1Du64,
            0x9E3779B97F4A7C15,
            0xD1B54A32D192ED03,
        ] {
            let run = pending_model_run(seed);
            println!("seed {seed:x}: {run:?}");
            assert!(run.idle > 1_000 && run.delivered > 1_000, "{run:?}");
        }
    }

    #[derive(Debug, Default)]
    struct PendingRun {
        checks: u64,
        idle: u64,
        delivered: u64,
        over_wakes: u64,
    }

    fn pending_model_run(seed: u64) -> PendingRun {
        let world = world();
        let mut rng = seed;
        let mut random = move |below: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % below
        };
        let mut ids: Vec<u64> = Vec::new();
        let mut run = PendingRun::default();

        for tick in 0..3_000u64 {
            let phase = tick % 200;
            if phase == 100 {
                let mut s = world.signal::<NumSignal>();
                for id in ids.drain(..) {
                    s.cancel(id);
                }
            }
            for receiver in [1, 2, 3, 2, 4u128] {
                let sends = if phase < 100 {
                    random(3)
                } else if random(40) == 0 {
                    1
                } else {
                    0
                };
                for _ in 0..sends {
                    let mut s = world.signal::<NumSignal>();
                    let payload = NumSignal(random(100));
                    let kind = if phase < 100 { random(12) } else { 0 };
                    let id = match kind {
                        0..=3 => Some(s.emit(payload)),
                        4 => s.send(payload, In(random(30))),
                        5 => s.send(payload, For(1 + random(30))),
                        6 => s.send(payload, (In(random(30)), For(1 + random(30)))),
                        7 => s.send(payload, ..),
                        8 => s.send(payload, (In(random(30)), ..)),
                        9 => {
                            let now = s.tick();
                            s.send(payload, (At(now + random(20)), To(now + random(40))))
                        }
                        _ => {
                            if !ids.is_empty() {
                                let id = ids[random(ids.len() as u64) as usize];
                                s.cancel(id);
                            }
                            None
                        }
                    };
                    if phase < 100 {
                        ids.extend(id);
                    }
                }

                if random(4) == 0 {
                    continue;
                }
                let now = world.get_resource::<TickCount>().unwrap().0;
                let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
                let mut data = world.get_resource_mut::<SignalData<NumSignal>>().unwrap();
                let pending = signals.pending(&data, receiver, now);
                let delivered = signals.receive(&mut data, receiver, now, |_| true).count();
                assert!(
                    pending || delivered == 0,
                    "receiver {receiver} would skip {delivered} signals at tick {now}"
                );
                run.checks += 1;
                run.idle += (!pending) as u64;
                run.delivered += (delivered > 0) as u64;
                run.over_wakes += (pending && delivered == 0) as u64;
            }
            end_tick(&world);
        }
        run
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
            if signals
                .transitory_running
                .iter()
                .any(|chunk| chunk.head > 0)
            {
                head_moves += 1;
            }
        }

        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert!(signals.transitory_running.len() + signals.transitory_waiting.len() > 1);
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
        assert_eq!(world.get_resource::<CurrentSystem>().unwrap().0, 0);

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
        assert!(signals.transitory_running.is_empty());
        assert!(signals.transitory_waiting.is_empty());
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
            signals.transitory_running[0].live().len()
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
        assert!(signals.transitory_running.is_empty());
        assert!(signals.transitory_waiting.is_empty());
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
        let even = |s: &NumSignal| s.0.is_multiple_of(2);

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
        let received = signal.receive();
        let mut payloads = Vec::new();
        for s in received {
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
    fn retain_delivers_the_same_signals_in_the_same_order_as_next() {
        let world = mixed_lists_world();
        let mut by_retain = Vec::new();
        let cancelled = world.signal::<NumSignal>().retain(|s| {
            by_retain.push(s.0);
            true
        });

        assert_eq!(cancelled, 0);
        assert_eq!(by_retain, receive_by_next(&mixed_lists_world()));
    }

    #[test]
    fn retain_cancels_in_every_list() {
        let world = mixed_lists_world();
        // 8: one-tick on its last tick (left alone), 9: a window's copy in the buffer, 102 and
        // 106: permanent around the sweep gap with cancelled bits already set.
        let dropped = [8, 9, 102, 106];
        let cancelled = world
            .signal::<NumSignal>()
            .retain(|s| !dropped.contains(&s.0));
        assert_eq!(cancelled, 3);
        assert!(world.signal::<NumSignal>().retain(|_| false) == 0);

        end_tick(&world);
        let mut next = receive(&world);
        next.sort_unstable();
        assert_eq!(next, [7, 103, 104, 107, 108, 109]);
    }

    #[test]
    fn retain_leaves_signals_on_their_last_tick_to_later_receivers() {
        let mut world = world();
        let (a, b) = receivers(&mut world);
        send(&world, 2, 0..1);
        world.signal::<NumSignal>().emit(NumSignal(1));
        send(&world, 3, 0..2);
        send(&world, 4, 0..9);

        world.get_resource_mut::<CurrentSystem>().unwrap().0 = 7;
        assert_eq!(world.signal::<NumSignal>().retain(|_| false), 2);
        world.get_resource_mut::<CurrentSystem>().unwrap().0 = 0;
        b.run(&mut world);
        assert_eq!(take_b(&world), [2, 1]);

        end_tick(&world);
        a.run(&mut world);
        // `a` ran before the senders, so it gets the one-tick signals now; 3 and 4 were cancelled.
        assert_eq!(take_a(&world), [2, 1]);
    }

    #[test]
    fn retain_leaves_a_window_on_its_last_tick() {
        let world = world();
        send(&world, 5, 0..2);
        end_tick(&world);

        let mut seen = Vec::new();
        let cancelled = world.signal::<NumSignal>().retain(|s| {
            seen.push(s.0);
            false
        });
        assert_eq!((seen, cancelled), (vec![5], 0));
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

    #[signal(entity)]
    pub struct Tagged(pub Entity, pub u64);

    const RECEIVERS: usize = 4;

    struct ModelSignal {
        id: u64,
        entity: Entity,
        payload: u64,
        sent: u64,
        start: u64,
        end: u64,
        cancelled: bool,
        taken: [bool; RECEIVERS],
    }

    #[derive(Default, Debug)]
    struct ModelRun {
        receives: u64,
        delivered: u64,
        idle: u64,
        cancels: u64,
        retain_cancels: u64,
        retain_last_tick: u64,
        modifies: u64,
        rejected: u64,
        partial_sweeps: u64,
        loads: u64,
        loads_mid_lap: u64,
        most_permanent: usize,
    }

    fn tagged_world() -> World {
        let mut world = World::new();
        world.init_param::<Mut<EntityManager>>();
        world.init_signal::<Tagged>();
        world
    }

    fn model_receive(
        model: &mut [ModelSignal],
        receiver: usize,
        tick: u64,
        first: bool,
        alive: impl Fn(Entity) -> bool,
    ) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        for signal in model.iter_mut().filter(|signal| !signal.cancelled) {
            let window = first && tick > signal.sent && signal.start <= tick && tick < signal.end;
            let transient = !signal.taken[receiver]
                && signal.start <= signal.sent
                && (tick == signal.sent || (tick == signal.sent + 1 && signal.end == tick));
            if transient {
                signal.taken[receiver] = true;
            }
            if (window || transient) && alive(signal.entity) {
                out.push((signal.entity.to_bits(), signal.payload));
            }
        }
        out.sort();
        out
    }

    fn signal_model_run(seed: u64, ticks: u64) -> ModelRun {
        let mut rng = seed;
        let mut random = move |below: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % below
        };
        let mut world = tagged_world();
        let mut model: Vec<ModelSignal> = Vec::new();
        let mut cancelled_ids: Vec<u64> = Vec::new();
        let mut entities: Vec<Entity> = Vec::new();
        let mut payloads = 0u64;
        let mut run = ModelRun::default();

        for tick in 0..ticks {
            if tick > 0 && random(40) == 0 {
                if random(2) == 0 {
                    let mut signals = world.get_resource_mut::<Signals<Tagged>>().unwrap();
                    let em = world.entities();
                    signals.sweep(1 + random(6), |s| em.is_alive(s.0));
                }
                let bytes = world.save();
                world = tagged_world();
                world.load(&bytes);
                run.loads += 1;
                let signals = world.get_resource::<Signals<Tagged>>().unwrap();
                run.loads_mid_lap += (signals.permanent_read > signals.permanent_write) as u64;
            }
            assert_eq!(world.get_resource::<TickCount>().unwrap().0, tick);

            let mut events: Vec<usize> = Vec::new();
            for receiver in 0..RECEIVERS {
                if random(6) != 0 {
                    events.push(receiver);
                    if random(8) == 0 {
                        events.push(receiver);
                    }
                }
            }
            events.extend(std::iter::repeat_n(RECEIVERS, random(12) as usize));
            for i in (1..events.len()).rev() {
                events.swap(i, random(i as u64 + 1) as usize);
            }

            let mut received_this_tick = [false; RECEIVERS];
            for event in events {
                let alive = |entity: Entity| world.entities().is_alive(entity);
                if event < RECEIVERS {
                    let receiver = event;
                    let first = !received_this_tick[receiver];
                    received_this_tick[receiver] = true;
                    let expected = model_receive(&mut model, receiver, tick, first, alive);
                    let system = receiver as u128 + 1;
                    let pending = {
                        let signals = world.get_resource::<Signals<Tagged>>().unwrap();
                        let data = world.get_resource::<SignalData<Tagged>>().unwrap();
                        signals.pending(&data, system, tick)
                    };
                    world.get_resource_mut::<CurrentSystem>().unwrap().0 = system;
                    let mut received: Vec<(u64, u64)> = Vec::new();
                    let mut dropped: Vec<u64> = Vec::new();
                    let mut count = 0;
                    let mut signals = world.signal::<Tagged>();
                    match run.receives % 3 {
                        0 => {
                            for signal in signals.receive() {
                                received.push((signal.0.to_bits(), signal.1));
                            }
                        }
                        1 => signals
                            .receive()
                            .for_each(|signal| received.push((signal.0.to_bits(), signal.1))),
                        _ => {
                            let kill = if random(4) == 0 { random(8) } else { 8 };
                            count = signals.retain(|signal| {
                                received.push((signal.0.to_bits(), signal.1));
                                let keep = signal.1 % 8 != kill;
                                if !keep {
                                    dropped.push(signal.1);
                                }
                                keep
                            });
                        }
                    }
                    drop(signals);
                    let mut expected_count = 0;
                    for signal in model.iter_mut().filter(|s| dropped.contains(&s.payload)) {
                        assert!(!signal.cancelled, "retain cancelled {} twice", signal.id);
                        // Only signals with ticks left are cancelled.
                        if signal.end > tick + 1 {
                            signal.cancelled = true;
                            cancelled_ids.push(signal.id);
                            run.retain_cancels += 1;
                            expected_count += 1;
                        } else {
                            run.retain_last_tick += 1;
                        }
                    }
                    assert_eq!(count, expected_count, "seed {seed:x}, tick {tick}");
                    world.get_resource_mut::<CurrentSystem>().unwrap().0 = 0;
                    received.sort();
                    assert_eq!(
                        received, expected,
                        "seed {seed:x}, tick {tick}, receiver {receiver}, first {first}"
                    );
                    assert!(
                        pending || received.is_empty(),
                        "seed {seed:x}, tick {tick}: pending said no to {received:?}"
                    );
                    run.receives += 1;
                    run.delivered += received.len() as u64;
                    run.idle += (!pending) as u64;
                    continue;
                }

                match random(100) {
                    0..=9 => {
                        let entity = world.entity_manager().spawn();
                        entities.push(entity);
                    }
                    10..=14 if !entities.is_empty() => {
                        let at = random(entities.len() as u64) as usize;
                        world.entity_manager().despawn(entities[at]);
                        if random(3) == 0 {
                            entities.swap_remove(at);
                        }
                    }
                    15..=59 => {
                        let entity = if entities.is_empty() || random(20) == 0 {
                            Entity::default()
                        } else {
                            entities[random(entities.len() as u64) as usize]
                        };
                        payloads += 1;
                        let payload = Tagged(entity, payloads);
                        let a = random(6);
                        let b = random(8);
                        let x = (tick + random(12)).saturating_sub(4);
                        let (start, end, id) = {
                            let mut signals = world.signal::<Tagged>();
                            match random(10) {
                                0 => (tick, tick + 1, Some(signals.emit(payload))),
                                1 => (tick + a, tick + a + 1, signals.send(payload, In(a))),
                                2 => (tick, tick + b, signals.send(payload, For(b))),
                                3 => (
                                    tick + a,
                                    tick + a + b,
                                    signals.send(payload, (In(a), For(b))),
                                ),
                                4 => (x, x + b, signals.send(payload, (At(x), To(x + b - 1)))),
                                5 => (x, x + 1, signals.send(payload, At(x))),
                                6 => (tick, x + 1, signals.send(payload, To(x))),
                                7 => (tick + a, u64::MAX, signals.send(payload, (In(a), ..))),
                                _ => (tick, u64::MAX, signals.send(payload, ..)),
                            }
                        };
                        let valid = end > start && end > tick;
                        assert_eq!(id.is_some(), valid, "tick {tick}: [{start}, {end})");
                        let Some(id) = id else {
                            run.rejected += 1;
                            continue;
                        };
                        model.push(ModelSignal {
                            id,
                            entity,
                            payload: payloads,
                            sent: tick,
                            start,
                            end,
                            cancelled: false,
                            taken: [false; RECEIVERS],
                        });
                    }
                    60..=79 if !model.is_empty() => {
                        let at = random(model.len() as u64) as usize;
                        let certain =
                            alive(model[at].entity) && tick < model[at].end && !model[at].cancelled;
                        let signal = &mut model[at];
                        let entity = if random(4) == 0 && !entities.is_empty() {
                            entities[random(entities.len() as u64) as usize]
                        } else {
                            signal.entity
                        };
                        payloads += 1;
                        let changed = world
                            .signal::<Tagged>()
                            .modify(signal.id, Tagged(entity, payloads));
                        assert!(
                            changed || !certain,
                            "tick {tick}: modify lost {}",
                            signal.id
                        );
                        assert!(
                            !(changed && signal.cancelled),
                            "modified a cancelled signal"
                        );
                        if changed {
                            signal.entity = entity;
                            signal.payload = payloads;
                            run.modifies += 1;
                        }
                    }
                    80..=91 if !model.is_empty() => {
                        let at = random(model.len() as u64) as usize;
                        let certain =
                            alive(model[at].entity) && tick < model[at].end && !model[at].cancelled;
                        let id = model[at].id;
                        let cancelled = world.signal::<Tagged>().cancel(id);
                        assert!(cancelled || !certain, "tick {tick}: cancel lost {id}");
                        assert!(!(cancelled && model[at].cancelled), "cancelled {id} twice");
                        if cancelled {
                            model[at].cancelled = true;
                            cancelled_ids.push(id);
                            run.cancels += 1;
                        }
                    }
                    92..=95 if !cancelled_ids.is_empty() => {
                        let id = cancelled_ids[random(cancelled_ids.len() as u64) as usize];
                        let mut signals = world.signal::<Tagged>();
                        assert!(!signals.cancel(id), "tick {tick}: cancelled {id} twice");
                        assert!(!signals.modify(id, Tagged(Entity::default(), 0)));
                    }
                    96..=97 => {
                        let mut signals = world.get_resource_mut::<Signals<Tagged>>().unwrap();
                        let em = world.entities();
                        let (_, done) = signals.sweep(1 + random(6), |s| em.is_alive(s.0));
                        run.partial_sweeps += (!done) as u64;
                    }
                    _ => {
                        let unknown = payloads + 1_000_000;
                        assert!(!world.signal::<Tagged>().cancel(unknown));
                    }
                }
            }

            world
                .get_resource::<SignalData<Tagged>>()
                .unwrap()
                .check_invariants();
            let permanent = world
                .get_resource::<Signals<Tagged>>()
                .unwrap()
                .permanent
                .len();
            run.most_permanent = run.most_permanent.max(permanent);
            end_tick(&world);
            let next = tick + 1;
            let alive = |entity: Entity| world.entities().is_alive(entity);
            model.retain(|signal| {
                !signal.cancelled
                    && (next <= signal.sent + 1 || next < signal.end)
                    && (alive(signal.entity) || next <= signal.sent + 1 || random(4) != 0)
            });
            if cancelled_ids.len() > 200 {
                cancelled_ids.drain(..100);
            }
        }
        run
    }

    #[test]
    fn entity_signals_match_a_naive_model() {
        let ticks = crate::test_size(4_000, 150) as u64;
        for seed in [
            0x2545F4914F6CDD1Du64,
            0x9E3779B97F4A7C15,
            0xD1B54A32D192ED03,
        ] {
            let run = signal_model_run(seed, ticks);
            println!("seed {seed:x}: {run:?}");
            assert!(
                run.delivered > ticks && run.idle > 0 && run.rejected > 0,
                "{run:?}"
            );
            assert!(
                run.cancels > 0
                    && run.retain_cancels > 0
                    && run.retain_last_tick > 0
                    && run.modifies > 0
                    && run.partial_sweeps > 0,
                "{run:?}"
            );
            if !cfg!(miri) {
                assert!(run.loads > 10 && run.loads_mid_lap > 0, "{run:?}");
            }
        }
    }
}
