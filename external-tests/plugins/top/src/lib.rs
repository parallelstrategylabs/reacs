pub use plugin_mid;

use plugin_mid::Died;
use plugin_mid::plugin_leaf::{CombatLog, combat_phase};
use reacs::prelude::*;

#[system]
pub fn top_system() {}

#[system(run_after(combat_phase))]
pub fn despawn_dead(
    mut died: Mut<Signal<Died>>,
    mut em: Mut<EntityManager>,
    mut log: Mut<Resource<CombatLog>>,
    tick: Tick,
) {
    let tick = tick.get();
    for signal in died.receive() {
        em.despawn(signal.0);
        log.lines
            .push(format!("{tick}: {} despawned", signal.0.index()));
    }
}
