use std::any::{TypeId, type_name};

use crate::{direct_map::DirectMap, prelude::*, schedule::Access};

pub struct Component<'w, C: Default + Copy> {
    pub map: Resource<'w, DirectMap<C>>,
}

pub struct ComponentMut<'w, C: Default + Copy> {
    pub map: ResourceMut<'w, DirectMap<C>>,
}

impl<C: 'static + Default + Copy + Hashed> SystemParam for Component<'_, C> {
    type Item<'w> = Component<'w, C>;

    fn fetch<'w>(world: &'w World) -> Self::Item<'w> {
        Component {
            map: Resource::<DirectMap<C>>::fetch(world),
        }
    }

    fn access() -> crate::schedule::Access {
        Access {
            type_id: TypeId::of::<C>(),
            type_name: type_name::<C>(),
            mutable: false,
        }
    }
}

impl<C: 'static + Default + Copy + Hashed> SystemParam for ComponentMut<'_, C> {
    type Item<'w> = ComponentMut<'w, C>;

    fn fetch<'w>(world: &'w World) -> Self::Item<'w> {
        ComponentMut {
            map: ResourceMut::<DirectMap<C>>::fetch(world),
        }
    }

    fn access() -> crate::schedule::Access {
        Access {
            type_id: TypeId::of::<C>(),
            type_name: type_name::<C>(),
            mutable: true,
        }
    }
}

#[cfg(test)]

mod tests {
    use crate::prelude::*;

    #[component]
    struct IntComponent(pub i32);

    #[test]
    fn component_mut_inserts() {
        const KEY: usize = 18;

        fn write(mut c_int: ComponentMut<IntComponent>) {
            c_int.map.insert(KEY, IntComponent(967));
        }
        fn check(c: Component<IntComponent>) {
            assert_eq!(c.map.get(KEY).map(|c| c.0), Some(967));
        }

        let mut world = World::new();
        world.init_component::<IntComponent>();

        write.into_system().run(&world);
        check.into_system().run(&world);
    }

    #[test]
    fn component_mut_removes() {
        const KEY: usize = 7;

        fn remove(mut c_int: ComponentMut<IntComponent>) {
            c_int.map.remove(KEY);
        }
        fn check(c: Component<IntComponent>) {
            assert!(c.map.get(KEY).is_none());
        }

        let mut world = World::new();
        world.init_component::<IntComponent>();
        world.set_component(KEY, IntComponent(344));

        remove.into_system().run(&world);
        check.into_system().run(&world);
    }
}
