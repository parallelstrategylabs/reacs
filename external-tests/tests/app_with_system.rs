use reacs::prelude::*;

mod common;

#[system]
fn app_system() {}

#[test]
fn world_new_registers_app_and_plugin_systems() {
    let _world = World::new();

    assert!(system_graph().iter().any(|s| s.name == "app_system"));
    assert_eq!(
        common::plugin_systems(),
        ["leaf_system", "mid_system", "other_system", "top_system"]
    );
}
