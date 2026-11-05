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
    schedule::{CurrentSystem, Mut, Write},
    world::World,
};

pub trait SignalType: 'static + Copy + Default + Hashed + Serialize + DeserializeOwned {}

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

    pub fn modify(&mut self, id: u64, f: impl FnOnce(&mut S)) -> bool {
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

        let mut windowed: &[(SignalWindow, SignalInstance<S>)] = match transient.first() {
            Some(first) => {
                let active = &self.transitory_active;
                &active[active.partition_point(|x| x.1.id < first.id)..]
            }
            None => &[],
        };

        transient
            .iter()
            .filter(move |x| {
                while windowed.first().is_some_and(|w| w.1.id < x.id) {
                    windowed = &windowed[1..];
                }
                !windowed.first().is_some_and(|w| w.1.id == x.id)
            })
            .chain(transient_buffer)
            .map(|x| &x.payload)
            .chain(
                self.transitory_active
                    .iter()
                    .filter(|x| !x.0.is_cancelled())
                    .map(|x| &x.1.payload),
            )
    }

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
            self.transitory_active_buffer.extend(
                active[l..]
                    .iter()
                    .copied()
                    .filter(|(window, _)| window.end > next_tick),
            );
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
        self.signals.receive(&mut self.data, (*self.current).0)
    }

    #[inline]
    pub fn tick(&self) -> u64 {
        (*self.tick).0
    }
}

pub static mut SIGNAL_REGISTRY: Option<Vec<fn(&World)>> = None;

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

    #[inline]
    pub fn cleanup_signal<S: SignalType>(&self) {
        cleanup_in_world::<S>(self);
    }

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

        assert_eq!(receive(&world), vec![1, 3, 10, 2]);
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
