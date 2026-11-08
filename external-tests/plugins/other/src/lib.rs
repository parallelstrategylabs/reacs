pub use plugin_leaf;

use plugin_leaf::{Combat, combat_phase};
use reacs::prelude::*;

#[system]
pub fn other_system() {}

#[signal(entity = target)]
pub struct Regen {
    pub target: Entity,
    pub amount: i32,
}

#[system(run_before(combat_phase))]
pub fn regenerate(mut regen: Mut<Signal<Regen>>, mut combat: Mut<Combat>) {
    let tick = (*combat.tick).0;
    for signal in regen.receive() {
        let Some(health) = combat.health.get_mut(signal.target) else {
            continue;
        };
        health.0 += signal.amount;
        let left = health.0;
        combat.log.lines.push(format!(
            "{tick}: {} healed {}, {left} left",
            signal.target.index(),
            signal.amount
        ));
    }
}
