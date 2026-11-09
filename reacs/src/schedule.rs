use crate::Tick;
use crate::entity::{Entity, EntityManager};
use crate::hashed::{Hashed, hash_path};
use crate::resource::LocalResourceType;
use crate::signal::{Signal, SignalType};
use crate::world::World;

use reacs_derive::resource;
use std::cell::{Ref, RefMut};
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fmt::Write as _;
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

pub(crate) const ANONYMOUS_SYSTEM: u128 = 0;

#[resource]
pub struct CurrentSystem(pub u128);

pub trait SystemId {
    const UUID: u128;
}

pub struct SystemEntry {
    pub uuid: u128,
    pub module: &'static str,
    pub name: &'static str,
    pub before: Vec<u128>,
    pub after: Vec<u128>,
    pub groups: Vec<u128>,
    pub(crate) init_fn: Box<dyn Fn(&mut World)>,
    pub(crate) run_fn: Box<dyn Fn(&mut World)>,
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

    pub fn in_group(mut self, groups: &[u128]) -> Self {
        self.groups.extend_from_slice(groups);
        self
    }

    pub(crate) fn init(&self, world: &mut World) {
        (self.init_fn)(world);
    }

    pub(crate) fn run(&self, world: &mut World) {
        set_current_system(world, self.uuid);
        (self.run_fn)(world);
        set_current_system(world, ANONYMOUS_SYSTEM);
    }
}

pub(crate) fn set_current_system(world: &World, uuid: u128) {
    if let Some(mut current) = world.get_resource_mut_silent::<CurrentSystem>() {
        current.0 = uuid;
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

        if system.groups.iter().any(|group| *group != system.groups[0]) {
            panic!(
                "system `{}::{}` is in more than one group: a system can only belong to one group, so keep one `in_group` target and use `run_after` or `run_before` to order it against the others",
                system.module, system.name
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

        for system in &self.nodes {
            for inner in system.groups.iter().filter_map(index_of) {
                let inner = &self.nodes[inner];
                if let Some(outer) = inner.groups.iter().find_map(index_of) {
                    let outer = &self.nodes[outer];
                    panic!(
                        "nested groups aren't supported: `{}::{}` is in group `{}::{}`, which is in group `{}::{}`",
                        system.module,
                        system.name,
                        inner.module,
                        inner.name,
                        outer.module,
                        outer.name
                    );
                }
            }
        }

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

        let declared = successors.clone();
        for (i, system) in self.nodes.iter().enumerate() {
            for group in system.groups.iter().filter_map(index_of) {
                successors[group].push(i);
                blockers[i] += 1;

                let group_uuid = self.nodes[group].uuid;
                for &next in &declared[group] {
                    if !self.nodes[next].groups.contains(&group_uuid) {
                        successors[i].push(next);
                        blockers[next] += 1;
                    }
                }
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

        let block_of: Vec<usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, system)| system.groups.iter().find_map(index_of).unwrap_or(i))
            .collect();

        let mut blocks = vec![Vec::new(); count];
        for &i in &order {
            blocks[block_of[i]].push(i);
        }

        let mut block_successors = vec![Vec::new(); count];
        let mut block_blockers = vec![0usize; count];
        for (i, nexts) in successors.iter().enumerate() {
            for &next in nexts {
                if block_of[i] != block_of[next] {
                    block_successors[block_of[i]].push(block_of[next]);
                    block_blockers[block_of[next]] += 1;
                }
            }
        }

        let mut ready: BinaryHeap<_> = (0..count)
            .filter(|&i| block_of[i] == i && block_blockers[i] == 0)
            .map(Reverse)
            .collect();
        let mut block_order = Vec::with_capacity(count);

        while let Some(Reverse(i)) = ready.pop() {
            block_order.push(i);
            for &next in &block_successors[i] {
                block_blockers[next] -= 1;
                if block_blockers[next] == 0 {
                    ready.push(Reverse(next));
                }
            }
        }

        if block_order.iter().map(|&i| blocks[i].len()).sum::<usize>() != count {
            panic!(
                "system ordering cycle: {} (a group and its members run as one block, so nothing else can run between them)",
                self.describe_cycle(&block_successors, &block_blockers)
            );
        }

        let mut slots: Vec<_> = self.nodes.drain(..).map(Some).collect();
        self.nodes.extend(
            block_order
                .into_iter()
                .flat_map(|block| std::mem::take(&mut blocks[block]))
                .map(|i| slots[i].take().unwrap()),
        );
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

    pub fn tree(&self) -> String {
        let width = self.nodes.len().to_string().len();
        let mut tree = String::new();
        let mut block = None;

        for (i, system) in self.nodes.iter().enumerate() {
            let group = system.groups.first();
            let branch = if group.is_some() && group == block {
                let next = self.nodes.get(i + 1);
                if next.is_some_and(|next| next.groups.first() == block) {
                    "├─ "
                } else {
                    "└─ "
                }
            } else {
                block = Some(&system.uuid);
                ""
            };

            writeln!(
                tree,
                "{:>width$} {branch}{}::{}",
                i + 1,
                system.module,
                system.name
            )
            .unwrap();
        }

        tree
    }

    pub fn print_tree(&self) {
        print!("{}", self.tree());
    }

    pub(crate) fn init(&self, world: &mut World) {
        for system in self.iter() {
            system.init(world);
        }
    }

    pub(crate) fn run(&self, world: &mut World) {
        for system in self.iter() {
            system.run(world);
        }
    }
}

#[derive(Clone, Copy)]
pub struct ParamWorld<'w>(pub(crate) &'w World);

pub struct ParamInit<'w>(pub(crate) &'w mut World);

impl ParamInit<'_> {
    #[inline]
    pub fn reborrow(&mut self) -> ParamInit<'_> {
        ParamInit(self.0)
    }
}

pub trait SystemParam {
    type Item<'w>;

    fn init(world: ParamInit<'_>);
    fn fetch<'w>(world: ParamWorld<'w>) -> Self::Item<'w>;
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
                    groups: Vec::new(),
                    init_fn: Box::new(move |world: &mut World| {
                        $( $param::init(ParamInit(&mut *world)); )*
                    }),

                    run_fn: Box::new(move |world: &mut World| {
                        fn call_inner<$($param),*>(
                            f: impl Fn($($param),*),
                            $($param: $param),*
                        ) {
                            f($($param),*)
                        }

                        let world = ParamWorld(&*world);
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
    type WorldGuard<'w>: Deref<Target = World>;
    fn fetch<'w, T: 'static + Hashed>(world: ParamWorld<'w>) -> Self::Guard<'w, T>;
    fn fetch_local<'w, T: LocalResourceType>(world: ParamWorld<'w>) -> Self::Guard<'w, T>;
}

impl Mode for Read {
    type Guard<'w, T: 'w> = Ref<'w, T>;
    type WorldGuard<'w> = &'w World;
    fn fetch<'w, T: 'static + Hashed>(world: ParamWorld<'w>) -> Ref<'w, T> {
        world
            .0
            .get_resource::<T>()
            .unwrap_or_else(|| conflict::<T>())
    }
    fn fetch_local<'w, T: LocalResourceType>(world: ParamWorld<'w>) -> Ref<'w, T> {
        world
            .0
            .get_local_resource::<T>()
            .unwrap_or_else(|| conflict::<T>())
    }
}

impl Mode for Write {
    type Guard<'w, T: 'w> = RefMut<'w, T>;
    type WorldGuard<'w> = &'w mut World;
    fn fetch<'w, T: 'static + Hashed>(world: ParamWorld<'w>) -> RefMut<'w, T> {
        world
            .0
            .get_resource_mut_silent::<T>()
            .unwrap_or_else(|| conflict::<T>())
    }
    fn fetch_local<'w, T: LocalResourceType>(world: ParamWorld<'w>) -> RefMut<'w, T> {
        world
            .0
            .get_local_resource_mut::<T>()
            .unwrap_or_else(|| conflict::<T>())
    }
}

pub trait WithMode<M: Mode> {
    type Out;
}

pub type Mut<P> = <P as WithMode<Write>>::Out;

pub struct WorldAccess<'w, M: Mode = Read>(M::WorldGuard<'w>);

impl<'w, M0: Mode, M: Mode> WithMode<M> for WorldAccess<'w, M0> {
    type Out = WorldAccess<'w, M>;
}

impl<M: Mode> Deref for WorldAccess<'_, M> {
    type Target = World;
    fn deref(&self) -> &World {
        &self.0
    }
}

impl WorldAccess<'_, Write> {
    #[inline]
    pub fn init_resource<R: 'static + Default + Hashed>(&mut self) {
        self.0.init_resource::<R>();
    }

    #[inline]
    pub fn insert_resource<R: 'static + Hashed>(&mut self, resource: R) {
        self.0.insert_resource(resource);
    }

    #[inline]
    pub fn remove_resource<R: 'static + Hashed>(&mut self) -> bool {
        self.0.remove_resource::<R>()
    }

    #[inline]
    pub fn get_resource_mut<R: 'static + Hashed>(&self) -> Option<RefMut<'_, R>> {
        self.0.get_resource_mut::<R>()
    }

    #[inline]
    pub fn init_component<C: 'static + Copy + Default + Hashed>(&mut self) {
        self.0.init_component::<C>();
    }

    #[inline]
    pub fn set_component<C: 'static + Copy + Default + Hashed>(
        &mut self,
        entity: Entity,
        value: C,
    ) {
        self.0.set_component(entity, value);
    }

    #[inline]
    pub fn remove_component<C: 'static + Copy + Default + Hashed>(&mut self, entity: Entity) {
        self.0.remove_component::<C>(entity);
    }

    #[inline]
    pub fn entity_manager(&self) -> Mut<EntityManager<'_>> {
        self.0.entity_manager()
    }

    #[inline]
    pub fn spawn(&self) -> Entity {
        self.0.entity_manager().spawn()
    }

    #[inline]
    pub fn signal<S: SignalType>(&self) -> Mut<Signal<'_, S>> {
        self.0.signal::<S>()
    }
}

impl SystemParam for WorldAccess<'_, Read> {
    type Item<'w> = WorldAccess<'w, Read>;

    fn init(_world: ParamInit<'_>) {}

    fn fetch<'w>(world: ParamWorld<'w>) -> WorldAccess<'w, Read> {
        WorldAccess(world.0)
    }
}

pub trait SystemArg {
    type Item<'w>;
}

impl<P: SystemParam> SystemArg for P {
    type Item<'w> = P::Item<'w>;
}

impl SystemArg for WorldAccess<'_, Write> {
    type Item<'w> = WorldAccess<'w, Write>;
}

pub struct ExclusiveSystem;

impl<F> IntoSystem<ExclusiveSystem> for F
where
    F: for<'w> Fn(WorldAccess<'w, Write>) + 'static,
{
    fn into_system(self) -> SystemEntry {
        SystemEntry {
            uuid: ANONYMOUS_SYSTEM,
            module: "",
            name: std::any::type_name::<F>(),
            before: Vec::new(),
            after: Vec::new(),
            groups: Vec::new(),
            init_fn: Box::new(|_world: &mut World| {}),
            run_fn: Box::new(move |world: &mut World| self(WorldAccess(world))),
        }
    }
}

#[cold]
pub(crate) fn conflict<T>() -> ! {
    panic!(
        "Resource `{}` is missing or already borrowed (conflicting params?)",
        std::any::type_name::<T>()
    )
}

impl World {
    pub fn build(setup: impl FnOnce(&mut WorldAccess<'_, Write>)) -> Self {
        let mut world = Self::new();
        setup(&mut WorldAccess(&mut world));
        world
    }

    #[inline]
    pub(crate) fn init_param<P: SystemParam>(&mut self) {
        P::init(ParamInit(self));
    }

    #[inline]
    pub(crate) fn fetch_param<P: SystemParam>(&self) -> P::Item<'_> {
        P::fetch(ParamWorld(self))
    }

    #[inline]
    pub(crate) fn run_systems(&mut self) {
        system_graph().run(self);
    }

    pub fn step(&mut self) {
        self.run_systems();
        self.cleanup_signals();
        self.get_resource_mut::<Tick>()
            .unwrap_or_else(|| conflict::<Tick>())
            .0 += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{SystemEntry, SystemGraph, SystemId, system_graph};
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
    fn macro_registers_system_with_same_uuid_as_named() {
        let uuid = hash_path(module_path!(), "physics");
        let registered = system_graph().iter().find(|s| s.uuid == uuid).unwrap();

        assert_eq!(registered.module, module_path!());
        assert_eq!(registered.name, "physics");
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
    fn read_world_access_runs_beside_other_params() {
        #[system]
        fn read_world(
            world: WorldAccess,
            res: Resource<IntResource>,
            _flag: Mut<Resource<BoolResource>>,
        ) {
            assert_eq!(world.get_resource::<IntResource>().unwrap().0, (*res).0);
            assert!(world.get_resource::<BoolResource>().is_none());
        }

        let mut world = World::new();
        world.insert_resource(IntResource(4));
        world.init_resource::<BoolResource>();

        read_world.into_system().run(&mut world);
    }

    #[test]
    fn mut_world_access_changes_the_world_directly() {
        #[component]
        struct Health(pub i32);

        #[system]
        fn write_world(mut world: Mut<WorldAccess>) {
            world.insert_resource(IntResource(9));
            world.init_component::<Health>();

            let entity = world.entity_manager().spawn();
            world.set_component(entity, Health(3));
            assert_eq!(world.get_component::<Health>(entity).unwrap().0, 3);
        }

        let mut world = World::new();

        write_world.into_system().run(&mut world);

        assert_eq!(world.get_resource::<IntResource>().unwrap().0, 9);
    }

    #[test]
    fn mut_world_access_sees_the_current_system() {
        #[system]
        fn current(world: Mut<WorldAccess>) {
            let current = world
                .get_resource::<crate::schedule::CurrentSystem>()
                .unwrap();
            assert_eq!(current.0, <current as crate::schedule::SystemId>::UUID);
        }

        let mut world = World::new();
        let entry = system_graph()
            .iter()
            .find(|system| system.name == "current")
            .unwrap();

        entry.run(&mut world);
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
    fn targets_outside_the_graph_are_ignored() {
        let graph = graph_of(vec![
            entry("physics", &[], &["audio"]),
            entry("input", &["audio"], &[]),
        ]);

        assert_eq!(names(&graph), ["physics", "input"]);
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

    mod groups {
        use crate::prelude::*;

        #[system]
        pub fn group_input() {}

        #[system(run_after(group_input))]
        pub fn group_physics() {}

        #[system(run_after(group_physics))]
        pub fn group_logic() {}

        #[system(run_after(group_logic))]
        pub fn group_audio() {}

        #[system(run_after(group_audio))]
        pub fn group_render() {}
    }

    #[system(in_group(groups::group_input))]
    fn read_controls() {}

    #[system(in_group(groups::group_physics))]
    fn integrate_bodies() {}

    #[system(in_group(groups::group_logic))]
    fn apply_damage() {}

    #[system(in_group(groups::group_audio))]
    fn mix_sounds() {}

    #[system(in_group(groups::group_render))]
    fn draw_frame() {}

    mod third_party {
        use super::groups::{group_logic, group_physics};
        use crate::prelude::*;

        #[system(run_after(group_physics), run_before(group_logic))]
        pub fn group_collisions() {}

        #[system(in_group(group_collisions))]
        pub fn resolve_contacts() {}

        #[system(in_group(group_physics))]
        pub fn extra_physics() {}
    }

    fn group(name: &'static str, before: &[&str], after: &[&str]) -> SystemEntry {
        fn empty() {}

        let mut entry = empty.into_system().named(module_path!(), name);
        entry.before = before.iter().map(|n| id(n)).collect();
        entry.after = after.iter().map(|n| id(n)).collect();
        entry
    }

    fn member(entry: SystemEntry, groups: &[&str]) -> SystemEntry {
        let groups: Vec<u128> = groups.iter().map(|n| id(n)).collect();
        entry.in_group(&groups)
    }

    #[test]
    fn third_party_module_fits_between_core_groups() {
        let physics = position(groups::group_physics::UUID);
        let collisions = position(third_party::group_collisions::UUID);
        let logic = position(groups::group_logic::UUID);

        for core_member in [integrate_bodies::UUID, third_party::extra_physics::UUID] {
            assert!(physics < position(core_member));
            assert!(position(core_member) < collisions);
        }

        let contacts = position(third_party::resolve_contacts::UUID);
        assert!(collisions < contacts);
        assert!(contacts < logic);
    }

    #[test]
    fn member_stays_between_group_and_its_run_before_target() {
        assert!(id("group_second") < id("render"));
        assert!(id("physics") < id("group_first"));

        let late = graph_of(vec![
            group("group_first", &["group_second"], &[]),
            group("group_second", &[], &[]),
            member(entry("render", &[], &[]), &["group_first"]),
        ]);
        assert_eq!(names(&late), ["group_first", "render", "group_second"]);

        let early = graph_of(vec![
            group("group_first", &["group_second"], &[]),
            group("group_second", &[], &[]),
            member(entry("physics", &[], &[]), &["group_first"]),
        ]);
        assert_eq!(names(&early), ["group_first", "physics", "group_second"]);
    }

    #[test]
    fn plain_system_running_after_group_waits_for_its_members() {
        assert!(id("physics") < id("render"));

        let graph = graph_of(vec![
            group("group_first", &[], &[]),
            member(entry("render", &[], &[]), &["group_first"]),
            entry("physics", &[], &["group_first"]),
        ]);

        assert_eq!(names(&graph), ["group_first", "render", "physics"]);
    }

    #[test]
    fn inserted_group_needs_no_change_to_existing_groups() {
        assert!(id("group_inserted") < id("render"));

        for order in permutations(&[0, 1, 2, 3, 4]) {
            let mut slots: Vec<_> = vec![
                group("group_first", &["group_last"], &[]),
                group("group_last", &[], &[]),
                member(entry("render", &[], &[]), &["group_first"]),
                group("group_inserted", &["group_last"], &["group_first"]),
                member(entry("input", &[], &[]), &["group_inserted"]),
            ]
            .into_iter()
            .map(Some)
            .collect();

            let mut graph = SystemGraph::new();
            for &i in &order {
                graph.insert(slots[i].take().unwrap());
            }

            assert_eq!(
                names(&graph),
                [
                    "group_first",
                    "render",
                    "group_inserted",
                    "input",
                    "group_last"
                ],
                "insertion order {order:?}"
            );
        }
    }

    #[test]
    fn member_running_after_its_group_does_not_order_other_members() {
        assert!(id("physics") < id("render"));

        let graph = graph_of(vec![
            group("group_first", &[], &[]),
            member(entry("physics", &[], &["group_first"]), &["group_first"]),
            member(entry("render", &[], &[]), &["group_first"]),
        ]);

        assert_eq!(names(&graph), ["group_first", "physics", "render"]);
    }

    #[test]
    fn members_can_order_each_other() {
        assert!(id("physics") < id("render"));

        let graph = graph_of(vec![
            group("group_first", &["group_second"], &[]),
            group("group_second", &[], &[]),
            member(entry("physics", &[], &["render"]), &["group_first"]),
            member(entry("render", &[], &[]), &["group_first"]),
        ]);

        assert_eq!(
            names(&graph),
            ["group_first", "render", "physics", "group_second"]
        );
    }

    #[test]
    fn group_outside_the_graph_is_ignored() {
        let graph = graph_of(vec![member(entry("render", &[], &[]), &["group_missing"])]);

        assert_eq!(names(&graph), ["render"]);
    }

    #[test]
    fn member_running_after_a_later_group_panics() {
        let message = cycle_panic(vec![
            group("group_first", &[], &[]),
            group("group_second", &[], &["group_first"]),
            member(entry("input", &[], &["group_second"]), &["group_first"]),
        ]);

        assert_eq!(
            message,
            cycle_message(&["group_second", "input", "group_second"])
        );
    }

    #[test]
    #[should_panic(expected = "a system can only belong to one group")]
    fn member_of_two_groups_panics() {
        graph_of(vec![
            group("group_first", &[], &[]),
            group("group_second", &[], &[]),
            member(entry("input", &[], &[]), &["group_first", "group_second"]),
        ]);
    }

    #[test]
    fn repeating_the_same_group_is_one_group() {
        let graph = graph_of(vec![
            group("group_first", &[], &[]),
            member(entry("input", &[], &[]), &["group_first", "group_first"]),
        ]);

        assert_eq!(names(&graph), ["group_first", "input"]);
    }

    #[test]
    fn unrelated_systems_never_run_inside_a_group() {
        let unrelated = (0..200).map(|i| &*format!("unrelated{i}").leak());
        let members = (0..6).map(|i| &*format!("member{i}").leak());

        let mut entries = vec![group("group_first", &[], &[])];
        entries.extend(members.map(|name| member(group(name, &[], &[]), &["group_first"])));
        entries.extend(unrelated.map(|name| group(name, &[], &[])));
        let graph = graph_of(entries);

        let names = names(&graph);
        let start = names.iter().position(|n| *n == "group_first").unwrap();
        assert!(
            names[start + 1..start + 7]
                .iter()
                .all(|n| n.starts_with("member"))
        );
    }

    #[test]
    fn member_ordering_moves_the_whole_block() {
        for entries in [
            vec![
                group("group_first", &[], &[]),
                member(entry("render", &[], &["physics"]), &["group_first"]),
                member(entry("input", &[], &[]), &["group_first"]),
                entry("physics", &[], &[]),
            ],
            vec![
                entry("physics", &[], &[]),
                member(entry("input", &[], &[]), &["group_first"]),
                member(entry("render", &[], &["physics"]), &["group_first"]),
                group("group_first", &[], &[]),
            ],
        ] {
            let graph = graph_of(entries);
            let names = names(&graph);

            assert_eq!(names[0], "physics");
            assert_eq!(names[1], "group_first");
        }
    }

    #[test]
    #[should_panic(expected = "run as one block")]
    fn system_ordered_between_members_panics() {
        graph_of(vec![
            group("group_first", &[], &[]),
            member(entry("input", &[], &[]), &["group_first"]),
            member(entry("render", &[], &["physics"]), &["group_first"]),
            entry("physics", &[], &["input"]),
        ]);
    }

    #[test]
    #[should_panic(expected = "nested groups aren't supported")]
    fn nested_group_panics() {
        graph_of(vec![
            group("outer", &[], &[]),
            member(group("inner", &[], &[]), &["outer"]),
            member(group("leaf", &[], &[]), &["inner"]),
        ]);
    }

    #[test]
    fn member_with_no_members_of_its_own_is_not_nested() {
        graph_of(vec![
            group("outer", &[], &[]),
            member(group("inner", &[], &[]), &["outer"]),
        ]);
    }

    #[test]
    fn systems_stay_inside_their_group() {
        let expected = [
            groups::group_input::UUID,
            read_controls::UUID,
            groups::group_physics::UUID,
            integrate_bodies::UUID,
            groups::group_logic::UUID,
            apply_damage::UUID,
            groups::group_audio::UUID,
            mix_sounds::UUID,
            groups::group_render::UUID,
            draw_frame::UUID,
        ];
        assert!(expected.windows(2).any(|w| w[0] > w[1]));

        let positions = expected.map(position);
        assert!(positions.windows(2).all(|w| w[0] < w[1]));
    }

    fn tree_lines(lines: &[&str]) -> String {
        lines
            .iter()
            .map(|line| format!("{}\n", line.replace("@", module_path!())))
            .collect()
    }

    #[test]
    fn tree_of_an_empty_graph_is_empty() {
        assert_eq!(SystemGraph::new().tree(), "");
    }

    #[test]
    fn tree_lists_plain_systems_in_execution_order() {
        let graph = graph_of(vec![
            entry("physics", &[], &["render"]),
            entry("render", &[], &[]),
        ]);

        assert_eq!(graph.tree(), tree_lines(&["1 @::render", "2 @::physics"]));
    }

    #[test]
    fn tree_puts_members_under_their_group() {
        assert!(id("physics") < id("render"));

        let graph = graph_of(vec![
            group("group_first", &["group_second"], &[]),
            group("group_second", &[], &[]),
            member(entry("physics", &[], &[]), &["group_first"]),
            member(entry("render", &[], &[]), &["group_first"]),
            member(entry("input", &[], &[]), &["group_second"]),
            entry("logic", &[], &["group_second"]),
        ]);

        assert_eq!(
            graph.tree(),
            tree_lines(&[
                "1 @::group_first",
                "2 ├─ @::physics",
                "3 └─ @::render",
                "4 @::group_second",
                "5 └─ @::input",
                "6 @::logic",
            ])
        );
    }

    #[test]
    fn tree_shows_a_group_without_members_as_a_plain_system() {
        let graph = graph_of(vec![
            group("group_first", &["render"], &[]),
            entry("render", &[], &[]),
        ]);

        assert_eq!(
            graph.tree(),
            tree_lines(&["1 @::group_first", "2 @::render"])
        );
    }

    #[test]
    fn tree_shows_members_of_a_missing_group_as_plain_systems() {
        let graph = graph_of(vec![
            member(entry("physics", &["render"], &[]), &["group_missing"]),
            member(entry("render", &[], &[]), &["group_missing"]),
        ]);

        assert_eq!(graph.tree(), tree_lines(&["1 @::physics", "2 @::render"]));
    }

    #[test]
    fn tree_aligns_numbers_past_nine_systems() {
        let graph = graph_of(
            (0..10)
                .map(|i| group(format!("system{i}").leak(), &[], &[]))
                .collect(),
        );
        let tree = graph.tree();
        let lines: Vec<&str> = tree.lines().collect();

        assert!(lines[0].starts_with(" 1 "));
        assert!(lines[8].starts_with(" 9 "));
        assert!(lines[9].starts_with("10 "));
    }

    #[test]
    fn groups_run_as_no_ops_between_their_members() {
        let graph = graph_of(vec![
            group("group_first", &[], &[]),
            group("group_second", &[], &["group_first"]),
            entry("render", &["group_first"], &[]),
            entry("physics", &["group_second"], &["group_first"]),
            entry("input", &[], &["group_second"]),
        ]);
        let mut world = World::new();

        graph.run(&mut world);

        assert_eq!(
            world.get_resource::<ExecLog>().unwrap().0,
            ["render", "physics", "input"]
        );
    }
}
