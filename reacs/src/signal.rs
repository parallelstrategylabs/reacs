use std::{
    marker::PhantomData,
    ops::{Bound, RangeBounds},
    ptr::addr_of_mut,
};

use reacs_derive::param;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    Tick,
    hashed::Hashed,
    registry::get_registry_entry,
    resource::Resource,
    schedule::{Mut, Write},
    world::World,
};

/// Implemented by `#[signal]`. Named `SignalType` so `Signal` can be the system param.
pub trait SignalType: 'static + Copy + Default + Hashed + Serialize + DeserializeOwned {}

/// Half-open tick window `[start, end)`.
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
    /// Id of the first transient signal not yet received.
    pub cursor: u64,
    /// Id handed to the next sent signal.
    pub next: u64,
    phantom: PhantomData<S>,
}

/// All lists are kept sorted by id: ids are monotonic and `cleanup` merges preserving order.
#[derive(Default, Hashed, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Signals<S: SignalType> {
    /// Sent last tick, still readable by systems that ran before the sender.
    pub transient: Vec<SignalInstance<S>>,
    /// Sent this tick.
    pub transient_buffer: Vec<SignalInstance<S>>,

    pub transitory_active: Vec<(SignalWindow, SignalInstance<S>)>,
    pub transitory_active_buffer: Vec<(SignalWindow, SignalInstance<S>)>,

    pub transitory_inactive: Vec<(SignalWindow, SignalInstance<S>)>,
    pub transitory_inactive_buffer: Vec<(SignalWindow, SignalInstance<S>)>,
}

impl<S: SignalType> Signals<S> {
    /// Returns `None` when the window is empty or already over.
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

        // Delivered this tick.
        if window.start <= tick {
            self.transient_buffer.push(instance);
        }

        // Still has ticks left after this one.
        if window.end > tick + 1 {
            self.transitory_inactive.push((window, instance));
        }

        Some(instance.id)
    }

    /// Modifies a pending or active transitory signal. Transient copies are not affected.
    pub fn modify(&mut self, id: u64) -> Option<&mut S> {
        for list in [&mut self.transitory_active, &mut self.transitory_inactive] {
            if let Ok(i) = list.binary_search_by(|x| x.1.id.cmp(&id)) {
                return Some(&mut list[i].1.payload);
            }
        }
        None
    }

    pub fn receive<'a>(
        &'a self,
        data: &mut SignalData<S>,
    ) -> impl Iterator<Item = &'a S> + use<'a, S> {
        let cursor = data.cursor;
        let unread =
            |list: &'a [SignalInstance<S>]| &list[list.partition_point(|x| x.id < cursor)..];

        let transient = unread(&self.transient);
        let transient_buffer = unread(&self.transient_buffer);

        if let Some(last) = transient_buffer.last().or_else(|| transient.last()) {
            data.cursor = last.id + 1;
        }

        transient
            .iter()
            .chain(transient_buffer)
            .map(|x| &x.payload)
            .chain(self.transitory_active.iter().map(|x| &x.1.payload))
    }

    /// End of `tick`: rotates the transient buffers and merges the transitory lists.
    /// Must run before the global `Tick` advances.
    pub fn cleanup(&mut self, tick: u64) {
        std::mem::swap(&mut self.transient, &mut self.transient_buffer);
        self.transient_buffer.clear();

        let next_tick = tick + 1;
        let active = &self.transitory_active;
        let inactive = &self.transitory_inactive;

        let mut l = 0;
        let mut r = 0;

        while l < active.len() && r < inactive.len() {
            let (active_window, active_signal) = active[l];
            let (inactive_window, inactive_signal) = inactive[r];

            if active_signal.id < inactive_signal.id {
                if active_window.end > next_tick {
                    self.transitory_active_buffer
                        .push((active_window, active_signal));
                }
                l += 1;
            } else {
                if inactive_window.start <= next_tick {
                    self.transitory_active_buffer
                        .push((inactive_window, inactive_signal));
                } else {
                    self.transitory_inactive_buffer
                        .push((inactive_window, inactive_signal));
                }
                r += 1;
            }
        }

        if l < active.len() {
            self.transitory_active_buffer.extend(
                active[l..]
                    .iter()
                    .copied()
                    .filter(|(window, _)| window.end > next_tick),
            );
        }

        if r < inactive.len() {
            for &(window, signal) in &inactive[r..] {
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
    }
}

/// Sends and receives signals of type `S`. Write only: receiving advances the cursor.
#[param(write)]
pub struct Signal<S: SignalType> {
    pub signals: Resource<Signals<S>>,
    pub data: Resource<SignalData<S>>,
    #[read]
    pub tick: Resource<Tick>,
}

impl<'w, S: SignalType> Signal<'w, S, Write> {
    /// Sends `signal`, delivered on every tick inside `window` (absolute ticks).
    /// Returns the signal id, or `None` if the window is empty or already over.
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

    /// Sends `signal` for the current tick only.
    #[inline]
    pub fn emit(&mut self, signal: S) -> u64 {
        let tick = self.tick();
        self.send(signal, tick..=tick)
            .expect("current tick window is never empty")
    }

    #[inline]
    pub fn modify(&mut self, id: u64) -> Option<&mut S> {
        self.signals.modify(id)
    }

    /// Unread transient signals followed by every active transitory signal.
    #[inline]
    pub fn receive(&mut self) -> impl Iterator<Item = &S> {
        self.signals.receive(&mut self.data)
    }

    #[inline]
    pub fn tick(&self) -> u64 {
        (*self.tick).0
    }
}

pub static mut SIGNAL_REGISTRY: Option<Vec<fn(&World)>> = None;

/// Called by `#[signal]` so `World::cleanup_signals` can reach every signal type.
pub fn register_signal<S: SignalType>() {
    unsafe {
        let ptr = addr_of_mut!(SIGNAL_REGISTRY);
        (*ptr)
            .get_or_insert_with(Vec::new)
            .push(cleanup_in_world::<S>);
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
    signals.cleanup(tick.0);
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

    /// Ends the tick for one signal type. No-op if it was never initialized.
    #[inline]
    pub fn cleanup_signal<S: SignalType>(&self) {
        cleanup_in_world::<S>(self);
    }

    /// Ends the tick for every initialized signal type. Call once per tick, after all
    /// systems and before advancing the global `Tick`.
    #[inline]
    pub fn cleanup_signals(&self) {
        for cleanup in get_registered_signals() {
            cleanup(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SignalData, Signals};
    use crate::{Tick, entity::Entity, prelude::*, registry::get_registry_entry};

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

    /// Runs `ticks` ticks, returning what one receive per tick saw.
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

        // Pending (inactive).
        *world.signal::<NumSignal>().modify(id).unwrap() = NumSignal(2);
        end_tick(&world);
        assert_eq!(receive(&world), vec![2]);

        // Active.
        *world.signal::<NumSignal>().modify(id).unwrap() = NumSignal(3);
        assert_eq!(receive(&world), vec![3]);

        end_tick(&world);
        end_tick(&world);
        assert!(world.signal::<NumSignal>().modify(id).is_none());
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
        // Tick 4: 0, 1, 3 still open; 102 (3..5) and 103 (4..6) active; 104 (5..7) pending.
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
        let mut sender = sender.into_system();
        let mut receiver = receiver.into_system();

        sender.run(&mut world);
        receiver.run(&mut world);
        end_tick(&world);
        receiver.run(&mut world);

        let received = world.get_resource::<Received>().unwrap();
        assert_eq!(received.0, vec![Entity::new(4, 1)]);
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
