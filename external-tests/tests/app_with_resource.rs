use reacs::prelude::*;

mod common;

#[resource]
pub struct AppResource(pub u64);

#[test]
fn world_new_registers_plugin_systems() {
    let _world = World::new();

    assert_eq!(
        common::plugin_systems(),
        ["leaf_system", "mid_system", "other_system", "top_system"]
    );
}
