use std::{
    any::Any,
    cell::{Ref, RefCell, RefMut},
};

use crate::{hashed::Hashed, schedule::system_graph};
pub struct World {
    pub resources: Vec<(u128, Box<RefCell<dyn Any>>)>,
}

impl World {
    #[inline]
    pub fn new() -> Self {
        let mut world = Self {
            resources: Vec::new(),
        };

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
        Ref::filter_map(self.resources[i].1.borrow(), |any| any.downcast_ref::<R>()).ok()
    }

    #[inline]
    pub fn get_resource_mut<R: 'static + Hashed>(&self) -> Option<RefMut<'_, R>> {
        self.get_resource_mut_silent::<R>()
    }

    #[inline]
    pub(crate) fn get_resource_mut_silent<R: 'static + Hashed>(&self) -> Option<RefMut<'_, R>> {
        let id = R::TYPE_UUID;
        let i = self.resources.binary_search_by(|x| x.0.cmp(&id)).ok()?;
        RefMut::filter_map(self.resources[i].1.borrow_mut(), |any| {
            any.downcast_mut::<R>()
        })
        .ok()
    }
}

pub struct StoredSerializer {
    pub serialize_fn: Box<dyn FnMut(&World)>,
}

#[cfg(test)]
mod tests {
    use crate::prelude::*;

    #[resource]
    struct IntResource(pub u64);

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
        const KEY: usize = 18;
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
        const KEY: usize = 7;

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
        world.set_component(1, IntComponent(5));

        world.init_component::<IntComponent>();

        assert_eq!(world.get_component::<IntComponent>(1).unwrap().0, 5);
    }

    #[test]
    #[should_panic(expected = "not initialized")]
    fn get_component_panics_when_not_initialized() {
        let world = World::new();
        world.get_component::<IntComponent>(1);
    }
}
