use reacs::prelude::*;

#[resource]
pub struct Gold(pub u32);

#[resource]
pub struct Temporary(pub u32);

#[component]
pub struct Health(pub i32);

#[signal]
pub struct Ping(pub u32);

#[test]
fn setup_can_add_and_remove_resources_and_storage() {
    let mut unit = Entity::default();
    let world = World::build(|setup| {
        setup.init_resource::<Gold>();
        setup.insert_resource(Temporary(5));
        assert!(setup.remove_resource::<Temporary>());
        setup.init_component::<Health>();
        unit = setup.spawn();
        setup.set_component(unit, Health(3));
        setup.init_signal::<Ping>();
        setup.signal::<Ping>().emit(Ping(1));
    });

    assert_eq!(world.get_resource::<Gold>().unwrap().0, 0);
    assert!(world.get_resource::<Temporary>().is_none());
    assert_eq!(world.get_component::<Health>(unit).unwrap().0, 3);
}
