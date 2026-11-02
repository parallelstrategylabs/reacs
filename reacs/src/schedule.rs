use crate::hashed::{Hashed, hash_path};
use crate::world::World;

use std::cell::{Ref, RefMut};
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

pub struct SystemEntry {
    pub uuid: u128,
    pub module: &'static str,
    pub name: &'static str,
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

        match self.nodes.binary_search_by_key(&system.uuid, |x| x.uuid) {
            Ok(i) => panic!(
                "system id collision: `{}::{}` and `{}::{}` share id {:#034x}",
                self.nodes[i].module, self.nodes[i].name, system.module, system.name, system.uuid
            ),
            Err(i) => self.nodes.insert(i, system),
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

    #[resource]
    struct IntResource(pub u64);

    #[resource]
    struct BoolResource(pub bool);

    #[resource]
    struct CustomResource {
        pub value: i32,
        pub active: bool,
    }

    #[test]
    fn world_initializes_param_through_system() {
        #[system]
        fn param_system(_res: Resource<IntResource>) {}

        let world = World::new();
        assert!(world.has_resource::<IntResource>());
    }

    #[test]
    #[should_panic]
    fn system_panics_with_conflict() {
        #[system]
        fn conflicting_system(_res: Resource<IntResource>, _res_mut: Mut<Resource<IntResource>>) {}

        let mut world = World::new();
        conflicting_system.into_system().run(&mut world);
    }

    #[test]
    fn system_runs_with_no_params() {
        #[system]
        fn no_param_system() {}

        let mut world = World::new();
        no_param_system.into_system().run(&mut world);
    }

    #[test]
    fn system_runs_with_many_params() {
        #[system]
        fn two_param_system(
            _res_mut_bool: Mut<Resource<BoolResource>>,
            _res_int: Resource<IntResource>,
        ) {
        }

        #[system]
        fn three_param_system(
            _res_custom: Mut<Resource<CustomResource>>,
            _res_int: Resource<IntResource>,
            _res_bool: Resource<BoolResource>,
        ) {
        }

        let mut world = World::new();

        two_param_system.into_system().run(&mut world);
        three_param_system.into_system().run(&mut world);
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
        let uuids: Vec<u128> = system_graph().iter().map(|s| s.uuid).collect();

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
}
