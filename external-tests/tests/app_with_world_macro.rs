use reacs::prelude::*;

mod common;

#[test]
fn world_macro_registers_plugin_systems() {
    let _world = world!();

    assert_eq!(
        common::plugin_systems(),
        ["leaf_system", "mid_system", "other_system", "top_system"]
    );
}
