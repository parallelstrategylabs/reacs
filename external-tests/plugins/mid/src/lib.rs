pub use plugin_leaf;

use plugin_leaf::{Combat, Damage, combat_phase};
use reacs::prelude::*;

#[system]
pub fn mid_system() {}

#[signal]
pub struct Died(pub Entity);

#[system(in_group(combat_phase))]
pub fn apply_damage(
    mut damage: Mut<Signal<Damage>>,
    mut combat: Mut<Combat>,
    mut died: Mut<Signal<Died>>,
) {
    let tick = combat.tick.get();
    for signal in damage.receive() {
        let Some(health) = combat.health.get_mut(signal.target) else {
            continue;
        };
        health.0 -= signal.amount;
        let left = health.0;
        combat.log.lines.push(format!(
            "{tick}: {} took {}, {left} left",
            signal.target.index(),
            signal.amount
        ));
        if left <= 0 && left + signal.amount > 0 {
            died.emit(Died(signal.target));
        }
    }
}
