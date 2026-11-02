use crate::hashed::{Hashed, hash_path};
use crate::world::World;

use std::cell::{Ref, RefMut};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::ops::Deref;
use std::ptr::{addr_of, addr_of_mut};

static mut SYSTEM_GRAPH: SystemGraph = SystemGraph::new();

pub fn register_system(system: SystemEntry) {
    unsafe {
        (&mut *addr_of_mut!(SYSTEM_GRAPH)).insert(system);
    }
}

pub fn system_graph() -> &'static SystemGraph {
    unsafe { &*addr_of!(SYSTEM_GRAPH) }
}

const ANONYMOUS_SYSTEM: u128 = 0;

pub trait SystemId {
    const UUID: u128;
}

pub struct SystemEntry {
    pub uuid: u128,
    pub module: &'static str,
    pub name: &'static str,
    pub before: Vec<u128>,
    pub after: Vec<u128>,
    pub init_fn: Box<dyn Fn(&mut World)>,
    pub run_fn: Box<dyn Fn(&mut World)>,
}

impl SystemEntry {
    pub fn named(mut self, module: &'static str, name: &'static str) -> Self {
        self.uuid = hash_path(module, name);
        self.module = module;
        self.name = name;
        self
    }

    pub fn run_before(mut self, targets: &[u128]) -> Self {
        self.before.extend_from_slice(targets);
        self
    }

    pub fn run_after(mut self, targets: &[u128]) -> Self {
        self.after.extend_from_slice(targets);
        self
    }

    pub fn init(&self, world: &mut World) {
        (self.init_fn)(world);
    }

    pub fn run(&self, world: &mut World) {
        (self.run_fn)(world);
    }
}

pub struct SystemGraph {
    nodes: Vec<SystemEntry>,
}

impl SystemGraph {
    pub const fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    pub fn insert(&mut self, system: SystemEntry) {
        assert!(
            system.uuid != ANONYMOUS_SYSTEM,
            "system `{}` has no stable id, use #[system] or SystemEntry::named",
            system.name
        );

        if let Some(other) = self.nodes.iter().find(|x| x.uuid == system.uuid) {
            panic!(
                "system id collision: `{}::{}` and `{}::{}` share id {:#034x}",
                other.module, other.name, system.module, system.name, system.uuid
            );
        }

        self.nodes.push(system);
        self.sort();
    }

    fn sort(&mut self) {
        self.nodes.sort_unstable_by_key(|x| x.uuid);

        let count = self.nodes.len();
        let mut successors = vec![Vec::new(); count];
        let mut blockers = vec![0usize; count];

        let index_of = |uuid: &u128| self.nodes.binary_search_by_key(uuid, |x| x.uuid).ok();
        for (i, system) in self.nodes.iter().enumerate() {
            for next in system.before.iter().filter_map(index_of) {
                successors[i].push(next);
                blockers[next] += 1;
            }
            for previous in system.after.iter().filter_map(index_of) {
                successors[previous].push(i);
                blockers[i] += 1;
            }
        }

        let mut ready: BinaryHeap<_> = (0..count)
            .filter(|&i| blockers[i] == 0)
            .map(Reverse)
            .collect();
        let mut order = Vec::with_capacity(count);

        while let Some(Reverse(i)) = ready.pop() {
            order.push(i);
            for &next in &successors[i] {
                blockers[next] -= 1;
                if blockers[next] == 0 {
                    ready.push(Reverse(next));
                }
            }
        }

        if order.len() != count {
            panic!(
                "system ordering cycle: {}",
                self.describe_cycle(&successors, &blockers)
            );
        }

        let mut slots: Vec<_> = self.nodes.drain(..).map(Some).collect();
        self.nodes
            .extend(order.into_iter().map(|i| slots[i].take().unwrap()));
    }

    fn describe_cycle(&self, successors: &[Vec<usize>], blockers: &[usize]) -> String {
        let start = blockers.iter().position(|&count| count > 0).unwrap();
        let mut path = vec![start];

        loop {
            let current = path[path.len() - 1];
            let previous = (0..self.nodes.len())
                .find(|&i| blockers[i] > 0 && successors[i].contains(&current))
                .unwrap();

            if let Some(first) = path.iter().position(|&i| i == previous) {
                return std::iter::once(&previous)
                    .chain(path[first..].iter().rev())
                    .map(|&i| format!("`{}::{}`", self.nodes[i].module, self.nodes[i].name))
                    .collect::<Vec<_>>()
                    .join(" -> ");
            }

            path.push(previous);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &SystemEntry> {
        self.nodes.iter()
    }

    pub fn init(&self, world: &mut World) {
        for system in self.iter() {
            system.init(world);
        }
    }

    pub fn run(&self, world: &mut World) {
        for system in self.iter() {
            system.run(world);
        }
    }
}

pub trait SystemParam {
    type Item<'w>;

    fn init(world: &mut World);
    fn fetch<'w>(world: &'w World) -> Self::Item<'w>;
}

pub trait IntoSystem<P> {
    fn into_system(self) -> SystemEntry;
}

macro_rules! impl_into_system_tuples {
    ($($param:ident),*) => {
        impl<F, $($param),*> IntoSystem<($($param,)*)> for F
        where
            F: Fn($($param),*) + 'static,
            F: for<'w> Fn($($param::Item<'w>),*),
            $($param: SystemParam),*
        {
            #[allow(nonstandard_style, unused_variables)]
            fn into_system(self) -> SystemEntry {
                SystemEntry {
                    uuid: ANONYMOUS_SYSTEM,
                    module: "",
                    name: std::any::type_name::<F>(),
                    before: Vec::new(),
                    after: Vec::new(),
                    init_fn: Box::new(move |world: &mut World| {
                        $( $param::init(world); )*
                    }),

                    run_fn: Box::new(move |world: &mut World| {
                        fn call_inner<$($param),*>(
                            f: impl Fn($($param),*),
                            $($param: $param),*
                        ) {
                            f($($param),*)
                        }

                        $( let $param = $param::fetch(world); )*
                        call_inner(&self, $($param),*);
                    }),
                }
            }
        }
    };
}

impl_into_system_tuples!();
impl_into_system_tuples!(P1);
impl_into_system_tuples!(P1, P2);
impl_into_system_tuples!(P1, P2, P3);
impl_into_system_tuples!(P1, P2, P3, P4);
impl_into_system_tuples!(P1, P2, P3, P4, P5);
impl_into_system_tuples!(P1, P2, P3, P4, P5, P6);
impl_into_system_tuples!(P1, P2, P3, P4, P5, P6, P7);
impl_into_system_tuples!(P1, P2, P3, P4, P5, P6, P7, P8);

pub struct Read;
pub struct Write;

pub trait Mode {
    type Guard<'w, T: 'w>: Deref<Target = T>;
    fn fetch<'w, T: 'static + Hashed>(world: &'w World) -> Self::Guard<'w, T>;
}

impl Mode for Read {
    type Guard<'w, T: 'w> = Ref<'w, T>;
    fn fetch<'w, T: 'static + Hashed>(world: &'w World) -> Ref<'w, T> {
        world.get_resource::<T>().unwrap_or_else(|| conflict::<T>())
    }
}

impl Mode for Write {
    type Guard<'w, T: 'w> = RefMut<'w, T>;
    fn fetch<'w, T: 'static + Hashed>(world: &'w World) -> RefMut<'w, T> {
        world
            .get_resource_mut_silent::<T>()
            .unwrap_or_else(|| conflict::<T>())
    }
}

pub trait WithMode<M: Mode> {
    type Out;
}

pub type Mut<P> = <P as WithMode<Write>>::Out;

#[cold]
fn conflict<T>() -> ! {
    panic!(
        "Resource `{}` is missing or already borrowed (conflicting params?)",
        std::any::type_name::<T>()
    )
}

impl World {
    #[inline]
    pub fn init_param<P: SystemParam>(&mut self) {
        P::init(self);
    }

    #[inline]
    pub fn fetch_param<P: SystemParam>(&self) -> P::Item<'_> {
        P::fetch(self)
    }

    #[inline]
    pub fn run_systems(&mut self) {
        system_graph().run(self);
    }
}

#[cfg(test)]
mod tests {
    use super::{SystemEntry, SystemGraph, system_graph};
    use crate::{hashed::hash_path, prelude::*, registry::get_registry_entry};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[resource]
    struct IntResource(pub u64);

    #[resource]
    struct BoolResource(pub bool);

    #[resource]
    struct CustomResource {
        pub value: i32,
        pub active: bool,
    }

    #[resource]
    struct ParamOnlyResource(pub u64);

    #[test]
    fn world_initializes_param_through_system() {
        #[system]
        fn param_system(_res: Resource<ParamOnlyResource>) {}

        let world = World::new();
        assert!(world.has_resource::<ParamOnlyResource>());
    }

    #[test]
    #[should_panic(
        expected = "Resource `reacs::schedule::tests::IntResource` is missing or already borrowed"
    )]
    fn system_panics_with_conflict() {
        #[system]
        fn conflicting_system(_res: Resource<IntResource>, _res_mut: Mut<Resource<IntResource>>) {}

        let mut world = World::new();
        conflicting_system.into_system().run(&mut world);
    }

    #[test]
    #[should_panic(
        expected = "Resource `reacs::schedule::tests::IntResource` is missing or already borrowed"
    )]
    fn system_panics_with_conflict_when_write_comes_first() {
        #[system]
        fn write_first_system(_res_mut: Mut<Resource<IntResource>>, _res: Resource<IntResource>) {}

        let mut world = World::new();
        write_first_system.into_system().run(&mut world);
    }

    #[test]
    fn system_runs_with_no_params() {
        static RAN: AtomicBool = AtomicBool::new(false);

        #[system]
        fn no_param_system() {
            RAN.store(true, Ordering::Relaxed);
        }

        let mut world = World::new();
        assert!(!RAN.load(Ordering::Relaxed));

        no_param_system.into_system().run(&mut world);
        assert!(RAN.load(Ordering::Relaxed));
    }

    #[test]
    fn system_runs_with_many_params() {
        #[system]
        fn two_param_system(
            mut res_mut_bool: Mut<Resource<BoolResource>>,
            res_int: Resource<IntResource>,
        ) {
            (*res_mut_bool).0 = (*res_int).0 == 7;
        }

        #[system]
        fn three_param_system(
            mut res_custom: Mut<Resource<CustomResource>>,
            res_int: Resource<IntResource>,
            res_bool: Resource<BoolResource>,
        ) {
            (*res_custom).value = (*res_int).0 as i32;
            (*res_custom).active = (*res_bool).0;
        }

        let mut world = World::new();
        world.insert_resource(IntResource(7));

        two_param_system.into_system().run(&mut world);
        three_param_system.into_system().run(&mut world);

        let custom = world.get_resource::<CustomResource>().unwrap();
        assert_eq!((*custom).value, 7);
        assert!((*custom).active);
    }

    #[resource]
    struct ExecLog(pub Vec<String>);

    macro_rules! logging_systems {
        ($($name:ident),*) => {
            $(
                #[system]
                fn $name(mut log: Mut<Resource<ExecLog>>) {
                    (*log).0.push(stringify!($name).to_string());
                }
            )*

            fn logging_entries() -> Vec<SystemEntry> {
                vec![$($name.into_system().named(module_path!(), stringify!($name))),*]
            }
        };
    }

    logging_systems!(input, logic, physics, audio, render);

    const UUID_ORDER: [&str; 5] = ["physics", "input", "logic", "audio", "render"];

    fn permutations(items: &[usize]) -> Vec<Vec<usize>> {
        if items.len() <= 1 {
            return vec![items.to_vec()];
        }

        let mut result = Vec::new();
        for (i, &head) in items.iter().enumerate() {
            let mut rest = items.to_vec();
            rest.remove(i);
            for mut tail in permutations(&rest) {
                tail.insert(0, head);
                result.push(tail);
            }
        }
        result
    }

    fn graph_in_order(order: &[usize]) -> SystemGraph {
        let mut slots: Vec<_> = logging_entries().into_iter().map(Some).collect();
        let mut graph = SystemGraph::new();
        for &i in order {
            graph.insert(slots[i].take().unwrap());
        }
        graph
    }

    #[test]
    fn hash_path_matches_pinned_values() {
        const MODULE: &str = "reacs::schedule::tests";
        assert_eq!(
            hash_path(MODULE, "physics"),
            0x0690cfe3b21ce294a3cb775e212c1f0e
        );
        assert_eq!(
            hash_path(MODULE, "render"),
            0xa12ad1e77e1e295aae76e13a45d20d39
        );
    }

    #[test]
    fn named_system_uuid_comes_from_module_and_name() {
        let entry = physics.into_system().named(module_path!(), "physics");

        assert_eq!(entry.uuid, hash_path("reacs::schedule::tests", "physics"));
        assert_eq!(entry.module, "reacs::schedule::tests");
        assert_eq!(entry.name, "physics");
    }

    #[test]
    fn macro_registers_system_with_same_uuid_as_named() {
        let uuid = hash_path(module_path!(), "physics");
        let registered = system_graph().iter().find(|s| s.uuid == uuid).unwrap();

        assert_eq!(registered.module, module_path!());
        assert_eq!(registered.name, "physics");
    }

    #[test]
    fn registered_systems_stay_sorted_by_uuid() {
        let targets: Vec<u128> = system_graph()
            .iter()
            .flat_map(|s| s.before.iter().chain(&s.after).copied())
            .collect();
        let uuids: Vec<u128> = system_graph()
            .iter()
            .filter(|s| s.before.is_empty() && s.after.is_empty() && !targets.contains(&s.uuid))
            .map(|s| s.uuid)
            .collect();

        assert!(uuids.len() >= UUID_ORDER.len());
        assert!(uuids.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn graph_is_ordered_after_every_insert() {
        let mut graph = SystemGraph::new();

        for (count, entry) in logging_entries().into_iter().enumerate() {
            graph.insert(entry);

            let uuids: Vec<u128> = graph.iter().map(|s| s.uuid).collect();
            assert_eq!(uuids.len(), count + 1);
            assert!(uuids.windows(2).all(|w| w[0] < w[1]));
        }
    }

    #[test]
    fn insertion_order_never_changes_execution_order() {
        let orders = permutations(&[0, 1, 2, 3, 4]);
        assert_eq!(orders.len(), 120);

        for order in orders {
            let graph = graph_in_order(&order);
            let mut world = World::new();
            graph.run(&mut world);

            let log = world.get_resource::<ExecLog>().unwrap();
            assert_eq!(log.0, UUID_ORDER, "insertion order {order:?}");
        }
    }

    #[test]
    fn graph_init_restores_param_resources() {
        let graph = graph_in_order(&[0, 1, 2, 3, 4]);
        let mut world = World::new();
        world.remove_resource::<ExecLog>();
        assert!(!world.has_resource::<ExecLog>());

        graph.init(&mut world);

        assert!(world.has_resource::<ExecLog>());
    }

    #[test]
    #[should_panic(expected = "system id collision")]
    fn inserting_same_path_twice_panics() {
        let mut graph = SystemGraph::new();
        graph.insert(input.into_system().named(module_path!(), "input"));
        graph.insert(logic.into_system().named(module_path!(), "input"));
    }

    #[test]
    #[should_panic(expected = "no stable id")]
    fn inserting_anonymous_system_panics() {
        SystemGraph::new().insert(input.into_system());
    }

    #[test]
    fn global_graph_keeps_pinned_order_and_uuids() {
        let names: Vec<&str> = system_graph()
            .iter()
            .map(|s| s.name)
            .filter(|name| UUID_ORDER.contains(name))
            .collect();
        assert_eq!(names, UUID_ORDER);

        for name in UUID_ORDER {
            let system = system_graph().iter().find(|s| s.name == name).unwrap();
            assert_eq!(system.uuid, hash_path(module_path!(), name));
        }
    }

    #[test]
    fn graph_runs_each_system_once_per_run() {
        let graph = graph_in_order(&[4, 2, 0, 3, 1]);
        let mut world = World::new();

        graph.run(&mut world);
        assert_eq!(world.get_resource::<ExecLog>().unwrap().0, UUID_ORDER);

        graph.run(&mut world);
        let expected: Vec<&str> = UUID_ORDER.iter().chain(&UUID_ORDER).copied().collect();
        assert_eq!(world.get_resource::<ExecLog>().unwrap().0, expected);
    }

    #[test]
    fn worlds_share_systems_but_not_state() {
        let graph = graph_in_order(&[0, 1, 2, 3, 4]);
        let mut first = World::new();
        let mut second = World::new();

        graph.run(&mut first);
        graph.run(&mut first);
        graph.run(&mut second);

        let count = |world: &World| world.get_resource::<ExecLog>().unwrap().0.len();
        assert_eq!(count(&first), 2 * UUID_ORDER.len());
        assert_eq!(count(&second), UUID_ORDER.len());
    }

    #[test]
    fn insertion_order_never_changes_serialized_state() {
        let snapshot = |order: &[usize]| -> Vec<(u128, Vec<u8>)> {
            let mut world = World::new();
            graph_in_order(order).run(&mut world);
            world
                .resources
                .iter()
                .map(|(id, resource)| {
                    let entry = get_registry_entry(*id).unwrap();
                    (*id, (entry.serialize)(&*resource.borrow()))
                })
                .collect()
        };

        assert_eq!(snapshot(&[0, 1, 2, 3, 4]), snapshot(&[4, 3, 2, 1, 0]));
    }

    #[test]
    #[should_panic(expected = "is missing or already borrowed")]
    fn running_without_a_required_resource_panics() {
        let graph = graph_in_order(&[0, 1, 2, 3, 4]);
        let mut world = World::new();
        world.remove_resource::<ExecLog>();

        graph.run(&mut world);
    }

    fn id(name: &str) -> u128 {
        hash_path(module_path!(), name)
    }

    fn entry(name: &'static str, before: &[&str], after: &[&str]) -> SystemEntry {
        let mut entry = logging_entries()
            .into_iter()
            .find(|e| e.name == name)
            .unwrap();
        entry.before = before.iter().map(|n| id(n)).collect();
        entry.after = after.iter().map(|n| id(n)).collect();
        entry
    }

    fn names(graph: &SystemGraph) -> Vec<&'static str> {
        graph.iter().map(|s| s.name).collect()
    }

    fn graph_of(entries: Vec<SystemEntry>) -> SystemGraph {
        let mut graph = SystemGraph::new();
        for entry in entries {
            graph.insert(entry);
        }
        graph
    }

    fn position(uuid: u128) -> usize {
        system_graph().iter().position(|s| s.uuid == uuid).unwrap()
    }

    fn cycle_panic(entries: Vec<SystemEntry>) -> String {
        let payload = catch_unwind(AssertUnwindSafe(|| graph_of(entries)))
            .err()
            .expect("graph should panic with a cycle");
        payload.downcast_ref::<String>().unwrap().clone()
    }

    fn cycle_message(path: &[&str]) -> String {
        let path: Vec<String> = path
            .iter()
            .map(|name| format!("`{}::{name}`", module_path!()))
            .collect();
        format!("system ordering cycle: {}", path.join(" -> "))
    }

    #[test]
    fn run_after_overrides_uuid_order() {
        let graph = graph_of(vec![
            entry("physics", &[], &["render"]),
            entry("input", &[], &[]),
            entry("logic", &[], &[]),
            entry("audio", &[], &[]),
            entry("render", &[], &[]),
        ]);

        assert_eq!(
            names(&graph),
            ["input", "logic", "audio", "render", "physics"]
        );
    }

    #[test]
    fn run_before_overrides_uuid_order() {
        let graph = graph_of(vec![
            entry("physics", &[], &[]),
            entry("input", &[], &[]),
            entry("logic", &[], &[]),
            entry("audio", &[], &[]),
            entry("render", &["physics"], &[]),
        ]);

        assert_eq!(
            names(&graph),
            ["input", "logic", "audio", "render", "physics"]
        );
    }

    #[test]
    fn uuid_breaks_ties_between_unordered_systems() {
        let graph = graph_of(vec![
            entry("physics", &[], &[]),
            entry("input", &[], &[]),
            entry("logic", &[], &[]),
            entry("audio", &["input"], &[]),
            entry("render", &[], &[]),
        ]);

        assert_eq!(
            names(&graph),
            ["physics", "logic", "audio", "input", "render"]
        );
    }

    #[test]
    fn ordering_is_transitive() {
        let graph = graph_of(vec![
            entry("render", &["audio"], &[]),
            entry("audio", &[], &[]),
            entry("logic", &[], &["audio"]),
            entry("input", &["render"], &[]),
            entry("physics", &[], &["logic"]),
        ]);

        assert_eq!(
            names(&graph),
            ["input", "render", "audio", "logic", "physics"]
        );
    }

    #[test]
    fn insertion_order_never_changes_constrained_order() {
        let expected = ["logic", "audio", "input", "render", "physics"];

        for order in permutations(&[0, 1, 2, 3, 4]) {
            let mut slots: Vec<_> = vec![
                entry("physics", &[], &["render"]),
                entry("input", &[], &[]),
                entry("logic", &[], &[]),
                entry("audio", &["input"], &[]),
                entry("render", &[], &[]),
            ]
            .into_iter()
            .map(Some)
            .collect();

            let mut graph = SystemGraph::new();
            for &i in &order {
                graph.insert(slots[i].take().unwrap());
            }

            assert_eq!(names(&graph), expected, "insertion order {order:?}");
        }
    }

    #[test]
    fn constrained_graph_runs_in_stored_order() {
        let graph = graph_of(vec![
            entry("physics", &[], &["render"]),
            entry("render", &[], &[]),
        ]);
        let mut world = World::new();

        graph.run(&mut world);

        assert_eq!(
            world.get_resource::<ExecLog>().unwrap().0,
            ["render", "physics"]
        );
    }

    #[test]
    fn targets_outside_the_graph_are_ignored() {
        let graph = graph_of(vec![
            entry("physics", &[], &["audio"]),
            entry("input", &["audio"], &[]),
        ]);

        assert_eq!(names(&graph), ["physics", "input"]);
    }

    #[test]
    fn system_waits_for_every_target_it_runs_after() {
        let graph = graph_of(vec![
            entry("physics", &[], &["render", "input"]),
            entry("input", &[], &[]),
            entry("render", &[], &[]),
        ]);

        assert_eq!(names(&graph), ["input", "render", "physics"]);
    }

    #[test]
    fn repeated_target_counts_as_one_constraint() {
        let graph = graph_of(vec![
            entry("physics", &[], &["render", "render"]),
            entry("render", &[], &[]),
        ]);

        assert_eq!(names(&graph), ["render", "physics"]);
    }

    #[test]
    fn same_constraint_can_be_declared_from_both_sides() {
        let graph = graph_of(vec![
            entry("physics", &[], &["render"]),
            entry("render", &["physics"], &[]),
        ]);

        assert_eq!(names(&graph), ["render", "physics"]);
    }

    #[test]
    fn ordering_cycle_panics() {
        let message = cycle_panic(vec![
            entry("input", &["logic"], &[]),
            entry("logic", &["input"], &[]),
        ]);

        assert_eq!(message, cycle_message(&["input", "logic", "input"]));
    }

    #[test]
    fn cycle_message_follows_execution_direction() {
        let message = cycle_panic(vec![
            entry("input", &["logic"], &[]),
            entry("logic", &["audio"], &[]),
            entry("audio", &["input"], &[]),
        ]);

        assert_eq!(
            message,
            cycle_message(&["input", "logic", "audio", "input"])
        );
    }

    #[test]
    fn cycle_message_excludes_systems_outside_the_cycle() {
        let message = cycle_panic(vec![
            entry("physics", &[], &["audio"]),
            entry("render", &[], &["audio"]),
            entry("input", &["logic"], &[]),
            entry("logic", &["audio"], &[]),
            entry("audio", &["input"], &[]),
        ]);

        assert_eq!(
            message,
            cycle_message(&["audio", "input", "logic", "audio"])
        );
    }

    #[test]
    fn system_ordered_against_itself_panics() {
        let message = cycle_panic(vec![entry("input", &["input"], &[])]);

        assert_eq!(message, cycle_message(&["input", "input"]));
    }

    #[test]
    fn before_and_after_same_target_panics() {
        let message = cycle_panic(vec![
            entry("input", &["logic"], &["logic"]),
            entry("logic", &[], &[]),
        ]);

        assert_eq!(message, cycle_message(&["input", "logic", "input"]));
    }

    mod ordered {
        use crate::prelude::*;

        #[system]
        pub fn nested_first() {}
    }

    use ordered::nested_first as renamed_first;

    #[system]
    fn ordered_group() {}

    #[system(run_after(ordered::nested_first), run_before(ordered_group))]
    fn ordered_middle() {}

    #[system(run_after(ordered_middle, self::ordered_group))]
    fn ordered_last() {}

    #[system(
        run_after(renamed_first),
        run_before(crate::schedule::tests::ordered_middle)
    )]

    fn ordered_early() {}

    #[system]
    fn against_after_target() {}

    #[system(run_after(against_after_target))]
    fn against_after() {}

    #[system]
    fn opposed_before_target() {}

    #[system(run_before(opposed_before_target))]
    fn opposed_before() {}

    #[system]
    fn repeated_first() {}

    #[system]
    fn repeated_second() {}

    #[system]
    fn repeated_third() {}

    #[system]
    fn repeated_fourth() {}

    #[system(
        run_after(repeated_first),
        run_after(repeated_second),
        run_before(repeated_third),
        run_before(repeated_fourth)
    )]
    fn repeated_lists() {}

    #[test]
    fn run_after_attribute_overrides_uuid_order() {
        assert!(id("against_after") < id("against_after_target"));
        assert!(position(id("against_after_target")) < position(id("against_after")));
    }

    #[test]
    fn run_before_attribute_overrides_uuid_order() {
        assert!(id("opposed_before_target") < id("opposed_before"));
        assert!(position(id("opposed_before")) < position(id("opposed_before_target")));
    }

    #[test]
    fn repeated_attribute_lists_accumulate() {
        let system = system_graph()
            .iter()
            .find(|s| s.uuid == id("repeated_lists"))
            .unwrap();

        assert_eq!(system.after, [id("repeated_first"), id("repeated_second")]);
        assert_eq!(system.before, [id("repeated_third"), id("repeated_fourth")]);
    }

    #[test]
    fn attributes_order_registered_systems() {
        let first = position(hash_path("reacs::schedule::tests::ordered", "nested_first"));
        let early = position(id("ordered_early"));
        let middle = position(id("ordered_middle"));
        let group = position(id("ordered_group"));
        let last = position(id("ordered_last"));

        assert!(first < early);
        assert!(early < middle);
        assert!(middle < group);
        assert!(group < last);
    }

    #[test]
    fn attributes_store_resolved_targets() {
        let middle = system_graph()
            .iter()
            .find(|s| s.uuid == id("ordered_middle"))
            .unwrap();

        assert_eq!(middle.before, [id("ordered_group")]);
        assert_eq!(
            middle.after,
            [hash_path("reacs::schedule::tests::ordered", "nested_first")]
        );
    }
}
