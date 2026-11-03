use reacs::prelude::*;

mod common;

#[resource(local)]
pub struct AppLocal(pub std::rc::Rc<u64>);

#[test]
fn world_new_registers_plugin_systems() {
    let mut world = World::new();
    world.insert_local_resource(AppLocal(std::rc::Rc::new(5)));

    assert_eq!(
        common::plugin_systems(),
        ["leaf_system", "mid_system", "other_system", "top_system"]
    );
    assert_eq!(*world.get_local_resource::<AppLocal>().unwrap().0, 5);
}
