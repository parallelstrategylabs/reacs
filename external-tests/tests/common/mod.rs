use reacs::prelude::*;

const PLUGIN_SYSTEMS: [&str; 4] = ["leaf_system", "mid_system", "other_system", "top_system"];

pub fn plugin_systems() -> Vec<&'static str> {
    let mut names: Vec<_> = system_graph()
        .iter()
        .map(|system| system.name)
        .filter(|name| PLUGIN_SYSTEMS.contains(name))
        .collect();
    names.sort_unstable();
    names
}
