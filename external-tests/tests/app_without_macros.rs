use reacs::prelude::*;

mod common;

#[test]
fn world_new_without_macros_misses_unnamed_plugins() {
    let _world = World::new();

    assert!(common::plugin_systems().is_empty());
}
