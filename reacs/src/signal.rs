use std::{
    any::TypeId,
    marker::PhantomData,
    ops::{Bound, RangeBounds},
    ptr::addr_of_mut,
};

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

impl SignalWindow {
    pub fn from_bounds(window: impl RangeBounds<u64>) -> Self {
        let start = match window.start_bound() {
            Bound::Included(&x) => x,
            Bound::Excluded(&x) => x.saturating_add(1),
            Bound::Unbounded => 0,
        };

        let end = match window.end_bound() {
            Bound::Included(&x) => x.saturating_add(1),
            Bound::Excluded(&x) => x,
            Bound::Unbounded => u64::MAX,
        };

        Self { start, end }
    }

    #[inline]
    pub fn contains(&self, tick: u64) -> bool {
        self.start <= tick && tick < self.end
    }

    #[inline]
    pub fn is_cancelled(&self) -> bool {
        self.end == 0
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(bound = "S: SignalType")]
pub struct SignalInstance<S> {
    pub id: u64,
    pub payload: S,
}

#[derive(Default, Hashed, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct SignalData<S: SignalType> {
    pub cursors: Vec<(u128, u64)>,
    pub next: u64,
    phantom: PhantomData<S>,
}

#[derive(Default, Hashed, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Signals<S: SignalType> {
    pub transient: Vec<SignalInstance<S>>,
    pub transient_buffer: Vec<SignalInstance<S>>,

    pub transitory_active: Vec<(SignalWindow, SignalInstance<S>)>,
    pub transitory_active_buffer: Vec<(SignalWindow, SignalInstance<S>)>,

    pub transitory_inactive: Vec<(SignalWindow, SignalInstance<S>)>,
    pub transitory_inactive_buffer: Vec<(SignalWindow, SignalInstance<S>)>,

    pub permanent: Vec<SignalInstance<S>>,
    pub permanent_write: usize,
    pub permanent_read: usize,
    pub permanent_cancelled: u64,
    pub permanent_cancelled_bits: BitSet,
}

enum ReceiveStage {
    Transient,
    TransientBuffer,
    Active,
    PermanentLeft,
    PermanentRight,
    PermanentCancelled,
    Done,
}

struct ReceiveIter<'a, S: SignalType> {
    run: std::slice::Iter<'a, SignalInstance<S>>,
    stage: ReceiveStage,
    transient: std::slice::Iter<'a, SignalInstance<S>>,
    windowed: &'a [(SignalWindow, SignalInstance<S>)],
    transient_buffer: &'a [SignalInstance<S>],
    active: std::slice::Iter<'a, (SignalWindow, SignalInstance<S>)>,
    permanent: &'a [SignalInstance<S>],
    write: usize,
    read: usize,
    position: usize,
    cancelled: Option<&'a BitSet>,
}

impl<'a, S: SignalType> ReceiveIter<'a, S> {
    fn next_slow(&mut self) -> Option<&'a S> {
        loop {
            if let Some(signal) = self.run.next() {
                return Some(&signal.payload);
            }

            match self.stage {
                ReceiveStage::Transient => {
                    if self.windowed.is_empty() {
                        self.run = std::mem::replace(&mut self.transient, [].iter());
                        self.stage = ReceiveStage::TransientBuffer;
                        continue;
                    }

                    let Some(signal) = self.transient.next() else {
                        self.stage = ReceiveStage::TransientBuffer;
                        continue;
                    };
                    while self.windowed.first().is_some_and(|w| w.1.id < signal.id) {
                        self.windowed = &self.windowed[1..];
                    }
                    if !self.windowed.first().is_some_and(|w| w.1.id == signal.id) {
                        return Some(&signal.payload);
                    }
                }
                ReceiveStage::TransientBuffer => {
                    self.run = self.transient_buffer.iter();
                    self.stage = ReceiveStage::Active;
                }
                ReceiveStage::Active => match self.active.find(|x| !x.0.is_cancelled()) {
                    Some(x) => return Some(&x.1.payload),
                    None => {
                        self.stage = if self.cancelled.is_some() {
                            ReceiveStage::PermanentCancelled
                        } else {
                            ReceiveStage::PermanentLeft
                        };
                    }
                },
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

impl<'a, S: SignalType> Iterator for ReceiveIter<'a, S> {
    type Item = &'a S;

    #[inline]
    fn next(&mut self) -> Option<&'a S> {
        if let Some(signal) = self.run.next() {
            return Some(&signal.payload);
        }
        self.next_slow()
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
            self.transitory_inactive.push((window, instance));
        }

        Some(instance.id)
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

        let windowed = [&mut self.transitory_active, &mut self.transitory_inactive]
            .into_iter()
            .find_map(|list| {
                let i = list.binary_search_by(|x| x.1.id.cmp(&id)).ok()?;
                let (window, signal) = &mut list[i];
                (!window.is_cancelled()).then_some(&mut signal.payload)
            });
        let transient = [&mut self.transient, &mut self.transient_buffer]
            .into_iter()
            .find_map(|list| {
                let i = list.binary_search_by(|x| x.id.cmp(&id)).ok()?;
                Some(&mut list[i].payload)
            });

        match (windowed, transient) {
            (Some(windowed), transient) => {
                f(windowed);
                if let Some(transient) = transient {
                    *transient = *windowed;
                }
                true
            }
            (None, Some(transient)) => {
                f(transient);
                true
            }
            (None, None) => false,
        }
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
        for list in [&mut self.transitory_active, &mut self.transitory_inactive] {
            if let Ok(i) = list.binary_search_by(|x| x.1.id.cmp(&id)) {
                let window = &mut list[i].0;
                if window.is_cancelled() {
                    return false;
                }
                window.end = 0;
                found = true;
                break;
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

    pub fn receive<'a>(
        &'a self,
        data: &mut SignalData<S>,
        system: u128,
    ) -> impl Iterator<Item = &'a S> + use<'a, S> {
        let slot = data.cursors.binary_search_by_key(&system, |x| x.0);
        let cursor = slot.map_or(0, |i| data.cursors[i].1);
        let unread =
            |list: &'a [SignalInstance<S>]| &list[list.partition_point(|x| x.id < cursor)..];

        let transient = unread(&self.transient);
        let transient_buffer = unread(&self.transient_buffer);

        if let Some(last) = transient_buffer.last().or_else(|| transient.last()) {
            match slot {
                Ok(i) => data.cursors[i].1 = last.id + 1,
                Err(i) => data.cursors.insert(i, (system, last.id + 1)),
            }
        }

        let windowed = match transient.first() {
            Some(first) => {
                let active = &self.transitory_active;
                &active[active.partition_point(|x| x.1.id < first.id)..]
            }
            None => &[],
        };

        ReceiveIter {
            run: [].iter(),
            stage: ReceiveStage::Transient,
            transient: transient.iter(),
            windowed,
            transient_buffer,
            active: self.transitory_active.iter(),
            permanent: &self.permanent,
            write: self.permanent_write,
            read: self.permanent_read,
            position: 0,
            cancelled: (self.permanent_cancelled > 0).then_some(&self.permanent_cancelled_bits),
        }
    }

    pub fn sweep(&mut self, budget: u64, mut keep: impl FnMut(&S) -> bool) -> (u64, bool) {
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

    fn sort_permanent(&mut self, first_new: usize) {
        let (write, read) = (self.permanent_write, self.permanent_read);
        let next = (read < first_new).then(|| self.permanent[read].id);

        let bits = std::mem::take(&mut self.permanent_cancelled_bits);
        let mut i = 0;
        self.permanent.retain(|_| {
            let in_gap = (write..read).contains(&i);
            let keep = !in_gap && !bits.get(i);
            i += 1;
            keep
        });
        self.permanent_cancelled = 0;
        self.permanent.sort_by_key(|x| x.id);

        let read = next.map_or(0, |id| self.permanent.partition_point(|x| x.id < id));
        self.permanent_write = read;
        self.permanent_read = read;
    }

    pub fn cleanup(&mut self, tick: u64) -> bool {
        std::mem::swap(&mut self.transient, &mut self.transient_buffer);
        self.transient_buffer.clear();

        let next_tick = tick + 1;
        let active = &self.transitory_active;
        let inactive = &self.transitory_inactive;

        let first_new = self.permanent.len();
        let newest = self.permanent.last().map(|x| x.id);

        let mut l = 0;
        let mut r = 0;

        while l < active.len() && r < inactive.len() {
            let (active_window, active_signal) = active[l];
            let (inactive_window, inactive_signal) = inactive[r];

            if active_signal.id < inactive_signal.id {
                if active_window.end == u64::MAX {
                    self.permanent.push(active_signal);
                } else if active_window.end > next_tick {
                    self.transitory_active_buffer
                        .push((active_window, active_signal));
                }
                l += 1;
            } else {
                if !inactive_window.is_cancelled() {
                    if inactive_window.start <= next_tick {
                        self.transitory_active_buffer
                            .push((inactive_window, inactive_signal));
                    } else {
                        self.transitory_inactive_buffer
                            .push((inactive_window, inactive_signal));
                    }
                }
                r += 1;
            }
        }

        if l < active.len() {
            for &(window, signal) in &active[l..] {
                if window.end == u64::MAX {
                    self.permanent.push(signal);
                } else if window.end > next_tick {
                    self.transitory_active_buffer.push((window, signal));
                }
            }
        }

        if r < inactive.len() {
            for &(window, signal) in &inactive[r..] {
                if window.is_cancelled() {
                    continue;
                }

                if window.start <= next_tick {
                    self.transitory_active_buffer.push((window, signal));
                } else {
                    self.transitory_inactive_buffer.push((window, signal));
                }
            }
        }

        std::mem::swap(
            &mut self.transitory_active,
            &mut self.transitory_active_buffer,
        );
        self.transitory_active_buffer.clear();

        std::mem::swap(
            &mut self.transitory_inactive,
            &mut self.transitory_inactive_buffer,
        );
        self.transitory_inactive_buffer.clear();

        let joined = self.permanent.len() > first_new;
        let joined_late = self
            .permanent
            .get(first_new)
            .zip(newest)
            .is_some_and(|(joined, newest)| joined.id < newest);
        if joined_late {
            self.sort_permanent(first_new);
        }

        if !S::Filter::ACTIVE && self.permanent_cancelled > 0 {
            self.sweep(SWEEP_BUDGET, |_| true);
        }

        joined
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
    pub fn send(&mut self, signal: S, window: impl RangeBounds<u64>) -> Option<u64> {
        let tick = self.tick();
        self.signals.send(
            &mut self.data,
            tick,
            signal,
            SignalWindow::from_bounds(window),
        )
    }

    #[inline]
    pub fn emit(&mut self, signal: S) -> u64 {
        let tick = self.tick();
        self.send(signal, tick..=tick)
            .expect("current tick window is never empty")
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
        self.signals
            .receive(&mut self.data, (*self.current).0)
            .filter(move |signal| S::Filter::keep(&context, signal))
    }

    #[inline]
    pub fn tick(&self) -> u64 {
        (*self.tick).0
    }
}

pub static mut SIGNAL_REGISTRY: Option<Vec<fn(&World)>> = None;

pub static mut SWEEP_REGISTRY: Option<Vec<(u128, fn(&World, u64) -> (u64, bool))>> = None;

pub static mut SWEEP_STAMPS: Option<Vec<(TypeId, fn(&World) -> u64)>> = None;

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
    let joined = signals.cleanup(tick.0);

    if S::Filter::ACTIVE && (joined || signals.permanent_cancelled > 0) {
        if let Some(mut cursor) = world.get_resource_mut::<SweepCursor>() {
            cursor.woken = true;
        }
    }
}

impl World {
    #[inline]
    pub fn init_signal<S: SignalType>(&mut self) {
        get_registry_entry(Signals::<S>::TYPE_UUID).unwrap_or_else(|| {
            let type_name = std::any::type_name::<S>();
            panic!("Signal `{type_name}` not registered, try using #[signal] {type_name}!")
        });

        self.init_resource::<Signals<S>>();
        self.init_resource::<SignalData<S>>();
        self.init_resource::<Tick>();
    }

    #[inline]
    pub fn signal<S: SignalType>(&self) -> Mut<Signal<'_, S>> {
        self.fetch_param::<Mut<Signal<S>>>()
    }

    #[inline]
    pub fn cleanup_signal<S: SignalType>(&self) {
        cleanup_in_world::<S>(self);
    }

    #[inline]
    pub fn cleanup_signals(&self) {
        sweep_signals(self);

        for cleanup in get_registered_signals() {
            cleanup(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SWEEP_BUDGET, SignalData, Signals, SweepCursor};
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
        assert_eq!(s.send(NumSignal(0), 0..5), Some(1));
        assert_eq!(s.send(NumSignal(0), 3..5), Some(2));
    }

    #[test]
    fn future_window_is_delivered_exactly_inside_window() {
        let world = world();
        world.signal::<NumSignal>().send(NumSignal(9), 2..4);

        assert_eq!(
            run_ticks(&world, 6),
            vec![vec![], vec![], vec![9], vec![9], vec![], vec![]]
        );
    }

    #[test]
    fn window_starting_now_is_delivered_once_per_tick() {
        let world = world();
        world.signal::<NumSignal>().send(NumSignal(5), 0..3);

        assert_eq!(
            run_ticks(&world, 4),
            vec![vec![5], vec![5], vec![5], vec![]]
        );
    }

    #[test]
    fn inclusive_window() {
        let world = world();
        world.signal::<NumSignal>().send(NumSignal(5), 1..=2);

        assert_eq!(run_ticks(&world, 4), vec![vec![], vec![5], vec![5], vec![]]);
    }

    #[test]
    fn unbounded_window_never_expires() {
        let world = world();
        world.signal::<NumSignal>().send(NumSignal(5), 2..);

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
        assert_eq!(s.send(NumSignal(0), 0..5), None);
        assert_eq!(s.send(NumSignal(0), 7..7), None);
        assert_eq!(s.send(NumSignal(0), 8..7), None);
        assert_eq!(s.send(NumSignal(0), 0..6), Some(0));
    }

    #[test]
    fn modify_changes_transitory_payload() {
        let world = world();
        let id = world
            .signal::<NumSignal>()
            .send(NumSignal(1), 1..3)
            .unwrap();

        assert!(world.signal::<NumSignal>().modify(id, NumSignal(2)));
        end_tick(&world);
        assert_eq!(receive(&world), vec![2]);

        assert!(world.signal::<NumSignal>().modify(id, NumSignal(3)));
        assert_eq!(receive(&world), vec![3]);

        end_tick(&world);
        end_tick(&world);
        assert!(!world.signal::<NumSignal>().modify(id, NumSignal(0)));
    }

    #[test]
    fn merge_keeps_transitory_sorted_by_id() {
        let world = world();
        {
            let mut s = world.signal::<NumSignal>();
            s.send(NumSignal(0), 1..10);
            s.send(NumSignal(1), 3..10);
            s.send(NumSignal(2), 1..2);
            s.send(NumSignal(3), 2..10);
        }

        for _ in 0..4 {
            end_tick(&world);
            {
                let mut s = world.signal::<NumSignal>();
                let tick = s.tick();
                s.send(NumSignal(100 + tick), tick + 1..tick + 3);
            }

            let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
            for list in [&signals.transitory_active, &signals.transitory_inactive] {
                assert!(list.windows(2).all(|w| w[0].1.id < w[1].1.id));
            }
        }

        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        let active: Vec<u64> = signals
            .transitory_active
            .iter()
            .map(|x| x.1.payload.0)
            .collect();
        assert_eq!(active, vec![0, 1, 3, 102, 103]);
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
        assert!(data.cursors.windows(2).all(|w| w[0].0 < w[1].0));
    }

    #[test]
    fn window_sent_after_receiver_ran_is_not_delivered_twice() {
        let mut world = world();
        let (a, b) = receivers(&mut world);

        a.run(&mut world);
        world.signal::<NumSignal>().send(NumSignal(5), 0..3);
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
            s.send(NumSignal(5), 0..2);
            s.send(NumSignal(5), 0..2);
        }
        end_tick(&world);

        a.run(&mut world);
        assert_eq!(take_a(&world), vec![5, 5, 5, 5]);
    }

    #[test]
    fn dedup_only_skips_signals_in_both_lists() {
        let world = world();
        world.signal::<NumSignal>().send(NumSignal(10), 0..);
        receive(&world);
        end_tick(&world);

        receive(&world);
        {
            let mut s = world.signal::<NumSignal>();
            s.emit(NumSignal(1));
            s.send(NumSignal(2), 1..4);
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
            .send(NumSignal(1), 0..3)
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
            .send(NumSignal(1), 0..3)
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
            .send(NumSignal(payload), window)
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
        assert_eq!(receive(&world), Vec::<u64>::new());
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
            assert_eq!(signals.transitory_active.len(), 1);
            assert!(signals.permanent.is_empty());
        }

        end_tick(&world);
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
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
        world.signal::<NumSignal>().send(NumSignal(1), 2..);

        assert_eq!(
            run_ticks(&world, 5),
            vec![vec![], vec![], vec![1], vec![1], vec![1]]
        );
        assert_eq!(permanent(&world), vec![(0, 1)]);
    }

    #[test]
    fn late_joining_signal_keeps_permanent_sorted_by_id() {
        let world = world();
        let late = world.signal::<NumSignal>().send(NumSignal(1), 3..).unwrap();
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
        assert_eq!(receive(&world), vec![20, 3, 40]);
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
        assert_eq!(receive(&world), vec![0, 4, 5, 7]);
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
        assert_eq!(receive(&world), vec![0, 4, 7]);
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
    fn late_join_drops_cancelled_permanent_signals() {
        let world = world();
        let late = world.signal::<NumSignal>().send(NumSignal(9), 3..).unwrap();
        for payload in 1..=3 {
            send_open(&world, payload);
        }
        run_ticks(&world, 2);
        assert!(cancel(&world, 2));

        run_ticks(&world, 2);

        assert_eq!(permanent(&world), vec![(late, 9), (1, 1), (3, 3)]);
        assert_eq!(receive(&world), vec![9, 1, 3]);
    }

    #[test]
    fn signal_joining_during_a_lap_is_kept_in_order() {
        let world = permanent_world(4);
        assert_eq!(sweep(&world, 2, |s| s.0 != 0), (2, false));

        send_open(&world, 4);
        run_ticks(&world, 2);

        assert_eq!(receive(&world), vec![1, 2, 3, 4]);
        assert_eq!(sweep(&world, 10, |_| true), (3, true));
        assert_eq!(permanent(&world), vec![(1, 1), (2, 2), (3, 3), (4, 4)]);
    }

    #[test]
    fn late_join_during_a_lap_keeps_the_sweep_position() {
        let world = world();
        let late = world.signal::<NumSignal>().send(NumSignal(9), 3..).unwrap();
        for payload in 1..=4 {
            send_open(&world, payload);
        }
        run_ticks(&world, 2);
        assert_eq!(sweep(&world, 2, |s| s.0 != 1), (2, false));

        run_ticks(&world, 2);

        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        assert_eq!(signals.permanent_read, signals.permanent_write);
        assert_eq!(
            signals.permanent[signals.permanent_read].payload,
            NumSignal(3)
        );
        drop(signals);
        assert_eq!(receive(&world), vec![9, 2, 3, 4]);
        assert!(world.signal::<NumSignal>().modify(late, NumSignal(8)));
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

    #[test]
    fn signal_storage_roundtrips_through_registry() {
        let world = world();
        world.signal::<NumSignal>().send(NumSignal(42), 1..3);

        let entry = get_registry_entry(Signals::<NumSignal>::TYPE_UUID).unwrap();
        let signals = world.get_resource::<Signals<NumSignal>>().unwrap();
        let bytes = (entry.serialize)(&*signals as &dyn std::any::Any);
        let restored = (entry.deserialize)(&bytes)
            .downcast::<Signals<NumSignal>>()
            .unwrap();

        assert_eq!(restored.transitory_inactive.len(), 1);
        assert_eq!(restored.transitory_inactive[0].1.payload, NumSignal(42));
    }
}
