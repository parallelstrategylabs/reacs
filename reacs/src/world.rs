use std::{
    any::{Any, TypeId},
    cell::{Ref, RefCell, RefMut},
};

use crate::{
    hashed::Hashed,
    registry::get_registry_entry,
    resource::LocalResourceType,
    schedule::{ANONYMOUS_SYSTEM, CurrentSystem, set_current_system, system_graph},
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct SaveHeader {
    app_version: u32,
}

struct LocalEntry {
    id: TypeId,
    cell: Box<RefCell<dyn Any>>,
    on_load: fn(&mut dyn Any, &World),
}

fn run_on_load<R: LocalResourceType>(resource: &mut dyn Any, world: &World) {
    if let Some(hook) = R::ON_LOAD {
        let resource = resource
            .downcast_mut::<R>()
            .expect("resource has the wrong type");
        hook(resource, world);
    }
}

pub struct World {
    pub resources: Vec<(u128, Box<RefCell<dyn Any>>)>,
    local_resources: Vec<LocalEntry>,
    app_version: u32,
}

impl World {
    #[inline]
    pub fn new() -> Self {
        let mut world = Self {
            resources: Vec::new(),
            local_resources: Vec::new(),
            app_version: 0,
        };

        world.init_resource::<CurrentSystem>();
        system_graph().init(&mut world);

        world
    }

    #[inline]
    pub fn init_resource<R: 'static + Default + Hashed>(&mut self) {
        if !self.has_resource::<R>() {
            self.insert_resource(R::default());
        }
    }

    #[inline]
    pub fn insert_resource<R: 'static + Hashed>(&mut self, resource: R) {
        let id = R::TYPE_UUID;
        let search_result = self.resources.binary_search_by(|x| x.0.cmp(&id));
        let new_resource = Box::new(RefCell::new(resource));

        match search_result {
            Ok(i) => self.resources[i].1 = new_resource,
            Err(i) => self.resources.insert(i, (id, new_resource)),
        }
    }

    #[inline]
    pub fn remove_resource<R: 'static + Hashed>(&mut self) -> bool {
        let id = R::TYPE_UUID;
        let search_result = self.resources.binary_search_by(|x| x.0.cmp(&id));

        if let Some(i) = search_result.ok() {
            self.resources.remove(i);
        }

        search_result.is_ok()
    }

    #[inline]
    pub fn has_resource<R: 'static + Hashed>(&self) -> bool {
        let id = R::TYPE_UUID;
        self.resources.binary_search_by(|x| x.0.cmp(&id)).is_ok()
    }

    #[inline]
    pub fn get_resource<R: 'static + Hashed>(&self) -> Option<Ref<'_, R>> {
        let id = R::TYPE_UUID;
        let i = self.resources.binary_search_by(|x| x.0.cmp(&id)).ok()?;
        let resource = self.resources[i].1.try_borrow().ok()?;
        Ref::filter_map(resource, |any| any.downcast_ref::<R>()).ok()
    }

    #[inline]
    pub fn get_resource_mut<R: 'static + Hashed>(&self) -> Option<RefMut<'_, R>> {
        self.get_resource_mut_silent::<R>()
    }

    #[inline]
    pub(crate) fn get_resource_mut_silent<R: 'static + Hashed>(&self) -> Option<RefMut<'_, R>> {
        let id = R::TYPE_UUID;
        let i = self.resources.binary_search_by(|x| x.0.cmp(&id)).ok()?;
        let resource = self.resources[i].1.try_borrow_mut().ok()?;
        RefMut::filter_map(resource, |any| any.downcast_mut::<R>()).ok()
    }

    #[inline]
    fn find_local_resource<R: LocalResourceType>(&self) -> Result<usize, usize> {
        let id = TypeId::of::<R>();
        self.local_resources.binary_search_by(|x| x.id.cmp(&id))
    }

    #[inline]
    pub fn init_local_resource<R: LocalResourceType>(&mut self) {
        if !self.has_local_resource::<R>() {
            self.insert_local_resource(R::default());
        }
    }

    #[inline]
    pub fn insert_local_resource<R: LocalResourceType>(&mut self, resource: R) {
        let cell = Box::new(RefCell::new(resource));

        match self.find_local_resource::<R>() {
            Ok(i) => self.local_resources[i].cell = cell,
            Err(i) => self.local_resources.insert(
                i,
                LocalEntry {
                    id: TypeId::of::<R>(),
                    cell,
                    on_load: run_on_load::<R>,
                },
            ),
        }
    }

    #[inline]
    pub fn remove_local_resource<R: LocalResourceType>(&mut self) -> bool {
        let search_result = self.find_local_resource::<R>();

        if let Ok(i) = search_result {
            self.local_resources.remove(i);
        }

        search_result.is_ok()
    }

    #[inline]
    pub fn has_local_resource<R: LocalResourceType>(&self) -> bool {
        self.find_local_resource::<R>().is_ok()
    }

    #[inline]
    pub fn get_local_resource<R: LocalResourceType>(&self) -> Option<Ref<'_, R>> {
        let i = self.find_local_resource::<R>().ok()?;
        let resource = self.local_resources[i].cell.try_borrow().ok()?;
        Ref::filter_map(resource, |any| any.downcast_ref::<R>()).ok()
    }

    #[inline]
    pub fn get_local_resource_mut<R: LocalResourceType>(&self) -> Option<RefMut<'_, R>> {
        let i = self.find_local_resource::<R>().ok()?;
        let resource = self.local_resources[i].cell.try_borrow_mut().ok()?;
        RefMut::filter_map(resource, |any| any.downcast_mut::<R>()).ok()
    }

    #[inline]
    pub fn set_app_version(&mut self, version: u32) {
        self.app_version = version;
    }

    #[inline]
    pub fn app_version(&self) -> u32 {
        self.app_version
    }

    pub fn save(&self) -> Vec<u8> {
        let saved: Vec<(u128, Vec<u8>)> = self
            .resources
            .iter()
            .map(|(id, cell)| {
                let entry = get_registry_entry(*id).unwrap_or_else(|| {
                    panic!("Can't save resource {id:#034x}: it isn't registered, use #[resource]")
                });
                let resource = cell.try_borrow().unwrap_or_else(|_| {
                    panic!(
                        "Can't save resource `{}` while it's borrowed for writing",
                        entry.type_name
                    )
                });
                (*id, (entry.serialize)(&*resource))
            })
            .collect();

        let header = SaveHeader {
            app_version: self.app_version,
        };
        bincode::serialize(&(header, saved)).expect("failed to encode save")
    }

    pub fn load(&mut self, bytes: &[u8]) {
        let mut body = bytes;
        let header: SaveHeader = bincode::deserialize_from(&mut body)
            .unwrap_or_else(|error| panic!("Failed to load save, it's corrupted: {error}"));

        if header.app_version != self.app_version {
            panic!(
                "Failed to load save: it has app version {}, but this world has app version {}",
                header.app_version, self.app_version
            );
        }

        let saved: Vec<(u128, Vec<u8>)> = bincode::deserialize(body)
            .unwrap_or_else(|error| panic!("Failed to load save, it's corrupted: {error}"));

        for (id, _) in &self.resources {
            if !saved.iter().any(|(saved_id, _)| saved_id == id) {
                let name = get_registry_entry(*id).map_or("unregistered", |entry| entry.type_name);
                panic!("Failed to load save: resource `{name}` ({id:#034x}) is missing from it");
            }
        }

        let resources = saved
            .iter()
            .map(|(id, bytes)| {
                let entry = get_registry_entry(*id).unwrap_or_else(|| {
                    panic!("Failed to load save: it has an unknown resource {id:#034x}")
                });
                (*id, (entry.restore)(bytes))
            })
            .collect();

        self.resources = resources;
        set_current_system(self, ANONYMOUS_SYSTEM);

        for local in &self.local_resources {
            (local.on_load)(&mut *local.cell.borrow_mut(), self);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SaveHeader;
    use crate::prelude::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[resource]
    struct IntResource(pub u64);

    fn save_with_unknown_resource(world: &World) -> Vec<u8> {
        let (header, mut saved): (SaveHeader, Vec<(u128, Vec<u8>)>) =
            bincode::deserialize(&world.save()).unwrap();
        saved.push((1, Vec::new()));
        bincode::serialize(&(header, saved)).unwrap()
    }

    #[test]
    fn world_macro_creates_initialized_world() {
        let world = world!();

        assert!(world.has_resource::<crate::schedule::CurrentSystem>());
    }

    #[test]
    fn get_resource_returns_none_while_mutably_borrowed() {
        let mut world = World::new();
        world.init_resource::<IntResource>();

        let _guard = world.get_resource_mut::<IntResource>().unwrap();

        assert!(world.get_resource::<IntResource>().is_none());
    }

    #[test]
    fn get_resource_mut_returns_none_while_borrowed() {
        let mut world = World::new();
        world.init_resource::<IntResource>();

        let _guard = world.get_resource::<IntResource>().unwrap();

        assert!(world.get_resource_mut::<IntResource>().is_none());
        assert!(world.get_resource::<IntResource>().is_some());
    }

    #[test]
    fn world_inserts_resource() {
        let mut world = World::new();
        world.init_resource::<IntResource>();

        assert!(
            world.has_resource::<IntResource>(),
            "Resource insertion failed!"
        );
    }

    #[test]
    fn world_gets_resource() {
        let mut world = World::new();
        world.insert_resource(IntResource(78));

        let value = world
            .get_resource::<IntResource>()
            .expect("Resource not initialized!");

        assert_eq!((*value).0, 78);
    }

    #[resource]
    struct OtherResource(pub bool);

    #[resource]
    struct ThirdResource(pub i32);

    #[test]
    fn remove_resource_clears_presence() {
        let mut world = World::new();
        world.init_resource::<IntResource>();

        world.remove_resource::<IntResource>();

        assert!(!world.has_resource::<IntResource>());
    }

    #[test]
    fn remove_first_resource_keeps_others_reachable() {
        let mut world = World::new();
        world.insert_resource(IntResource(1));
        world.insert_resource(OtherResource(true));
        world.insert_resource(ThirdResource(3));

        world.remove_resource::<IntResource>();

        assert!(world.get_resource::<OtherResource>().unwrap().0);
        assert_eq!(world.get_resource::<ThirdResource>().unwrap().0, 3);
    }

    #[test]
    fn remove_middle_resource_keeps_others_reachable() {
        let mut world = World::new();
        world.insert_resource(IntResource(1));
        world.insert_resource(OtherResource(true));
        world.insert_resource(ThirdResource(3));

        world.remove_resource::<OtherResource>();

        assert_eq!(world.get_resource::<IntResource>().unwrap().0, 1);
        assert_eq!(world.get_resource::<ThirdResource>().unwrap().0, 3);
    }

    #[test]
    fn remove_last_resource_keeps_others_reachable() {
        let mut world = World::new();
        world.insert_resource(IntResource(1));
        world.insert_resource(OtherResource(true));
        world.insert_resource(ThirdResource(3));

        world.remove_resource::<ThirdResource>();

        assert_eq!(world.get_resource::<IntResource>().unwrap().0, 1);
        assert!(world.get_resource::<OtherResource>().unwrap().0);
    }

    #[component]
    struct IntComponent(pub i32);

    #[test]
    fn world_sets_component() {
        const KEY: Entity = Entity::new(18, 1);
        const VALUE: i32 = 967;

        let mut world = World::new();
        world.init_component::<IntComponent>();
        assert!(!world.has_component::<IntComponent>(KEY));

        world.set_component(KEY, IntComponent(VALUE));
        assert!(world.has_component::<IntComponent>(KEY));

        assert_eq!(world.get_component::<IntComponent>(KEY).unwrap().0, VALUE);
    }

    #[test]
    fn world_removes_component() {
        const KEY: Entity = Entity::new(7, 1);

        let mut world = World::new();
        world.init_component::<IntComponent>();
        assert!(!world.has_component::<IntComponent>(KEY));

        world.set_component(KEY, IntComponent(344));
        assert!(world.has_component::<IntComponent>(KEY));

        world.remove_component::<IntComponent>(KEY);
        assert!(!world.has_component::<IntComponent>(KEY));
    }

    #[test]
    fn init_resource_keeps_existing_value() {
        let mut world = World::new();
        world.insert_resource(IntResource(7));

        world.init_resource::<IntResource>();

        assert_eq!(world.get_resource::<IntResource>().unwrap().0, 7);
    }

    #[test]
    fn init_component_keeps_existing_components() {
        let mut world = World::new();
        world.init_component::<IntComponent>();
        let entity = Entity::new(1, 1);
        world.set_component(entity, IntComponent(5));

        world.init_component::<IntComponent>();

        assert_eq!(world.get_component::<IntComponent>(entity).unwrap().0, 5);
    }

    #[test]
    #[should_panic(expected = "not initialized")]
    fn get_component_panics_when_not_initialized() {
        let world = World::new();
        world.get_component::<IntComponent>(Entity::new(1, 1));
    }

    #[test]
    fn save_is_stable() {
        let mut world = World::new();
        world.insert_resource(IntResource(5));

        assert_eq!(world.save(), world.save());
    }

    #[test]
    fn load_restores_resources_and_components() {
        let mut world = World::new();
        world.insert_resource(IntResource(5));
        world.init_component::<IntComponent>();
        let entity = world.entity_manager().spawn();
        world.set_component(entity, IntComponent(9));
        let save = world.save();

        world.insert_resource(IntResource(6));
        world.set_component(entity, IntComponent(10));
        world.load(&save);

        assert_eq!(world.get_resource::<IntResource>().unwrap().0, 5);
        assert_eq!(world.get_component::<IntComponent>(entity).unwrap().0, 9);
        assert!(world.entity_manager().is_alive(entity));
        assert_eq!(world.save(), save);
    }

    #[test]
    fn load_adds_resources_the_world_does_not_have() {
        let mut source = World::new();
        source.insert_resource(OtherResource(true));
        let save = source.save();

        let mut target = World::new();
        target.load(&save);

        assert!(target.get_resource::<OtherResource>().unwrap().0);
    }

    #[test]
    fn load_resets_current_system() {
        let mut world = World::new();
        world.insert_resource(crate::schedule::CurrentSystem(77));
        let save = world.save();

        world.load(&save);

        assert_eq!(
            world
                .get_resource::<crate::schedule::CurrentSystem>()
                .unwrap()
                .0,
            0
        );
    }

    #[test]
    #[should_panic(expected = "is missing from it")]
    fn load_panics_when_save_misses_a_resource() {
        #[resource]
        struct AddedAfterSave;

        let mut world = World::new();
        let save = world.save();

        world.init_resource::<AddedAfterSave>();
        world.load(&save);
    }

    #[test]
    #[should_panic(expected = "unknown resource")]
    fn load_panics_on_unknown_resource() {
        let mut world = World::new();
        let save = save_with_unknown_resource(&world);

        world.load(&save);
    }

    #[test]
    fn save_starts_with_the_header() {
        let mut world = World::new();
        world.set_app_version(3);

        let header: SaveHeader = bincode::deserialize(&world.save()).unwrap();

        assert_eq!(header.app_version, 3);
    }

    #[test]
    fn app_version_defaults_to_zero() {
        assert_eq!(World::new().app_version(), 0);
    }

    #[test]
    fn load_accepts_same_app_version() {
        let mut world = World::new();
        world.set_app_version(3);
        world.insert_resource(IntResource(5));
        let save = world.save();

        let mut target = World::new();
        target.set_app_version(3);
        target.load(&save);

        assert_eq!(target.get_resource::<IntResource>().unwrap().0, 5);
        assert_eq!(target.app_version(), 3);
    }

    #[test]
    #[should_panic(expected = "it has app version 3, but this world has app version 4")]
    fn load_panics_on_other_app_version() {
        let mut world = World::new();
        world.set_app_version(3);
        let save = world.save();

        world.set_app_version(4);
        world.load(&save);
    }

    #[test]
    fn load_with_other_app_version_keeps_the_world() {
        let mut world = World::new();
        world.set_app_version(3);
        world.insert_resource(IntResource(5));
        let save = world.save();

        world.set_app_version(4);
        world.insert_resource(IntResource(6));
        let result = catch_unwind(AssertUnwindSafe(|| world.load(&save)));

        assert!(result.is_err());
        assert_eq!(world.get_resource::<IntResource>().unwrap().0, 6);
    }

    #[test]
    #[should_panic(expected = "corrupted")]
    fn load_panics_on_truncated_save() {
        let mut world = World::new();
        let save = world.save();

        world.load(&save[..save.len() - 1]);
    }

    #[test]
    #[should_panic(expected = "corrupted")]
    fn load_panics_on_corrupted_save() {
        World::new().load(&[1, 2, 3]);
    }

    #[test]
    fn failed_load_keeps_the_world() {
        let mut world = World::new();
        world.insert_resource(IntResource(5));
        let save = save_with_unknown_resource(&world);

        let result = catch_unwind(AssertUnwindSafe(|| world.load(&save)));

        assert!(result.is_err());
        assert_eq!(world.get_resource::<IntResource>().unwrap().0, 5);
    }

    #[test]
    #[should_panic(expected = "isn't registered")]
    fn save_panics_on_unregistered_resource() {
        #[derive(DeriveHashed)]
        struct Unregistered;

        let mut world = World::new();
        world.insert_resource(Unregistered);

        world.save();
    }

    #[test]
    #[should_panic(expected = "borrowed for writing")]
    fn save_panics_while_resource_is_borrowed_for_writing() {
        let world = {
            let mut world = World::new();
            world.init_resource::<IntResource>();
            world
        };
        let _guard = world.get_resource_mut::<IntResource>().unwrap();

        world.save();
    }

    #[resource(local)]
    struct Untouched(pub u64);

    #[resource(local, on_load = rebuild)]
    struct RebuiltFromSave {
        value: u64,
        runs: u64,
    }

    impl RebuiltFromSave {
        fn rebuild(&mut self, world: &World) {
            self.value = world.get_resource::<IntResource>().unwrap().0;
            self.runs += 1;
        }
    }

    #[test]
    fn load_keeps_local_resources_without_hook() {
        let mut world = World::new();
        let save = world.save();
        world.insert_local_resource(Untouched(3));

        world.load(&save);

        assert_eq!(world.get_local_resource::<Untouched>().unwrap().0, 3);
    }

    #[test]
    fn on_load_runs_once_after_resources_are_restored() {
        let mut world = World::new();
        world.insert_resource(IntResource(5));
        let save = world.save();

        world.insert_resource(IntResource(6));
        world.init_local_resource::<RebuiltFromSave>();
        assert_eq!(
            world.get_local_resource::<RebuiltFromSave>().unwrap().runs,
            0
        );

        world.load(&save);

        let local = world.get_local_resource::<RebuiltFromSave>().unwrap();
        assert_eq!(local.value, 5);
        assert_eq!(local.runs, 1);
    }

    #[test]
    fn on_load_keeps_running_after_replacing_the_local_resource() {
        let mut world = World::new();
        world.insert_resource(IntResource(5));
        let save = world.save();
        world.init_local_resource::<RebuiltFromSave>();
        world.insert_local_resource(RebuiltFromSave { value: 0, runs: 10 });

        world.load(&save);

        assert_eq!(
            world.get_local_resource::<RebuiltFromSave>().unwrap().runs,
            11
        );
    }
}
