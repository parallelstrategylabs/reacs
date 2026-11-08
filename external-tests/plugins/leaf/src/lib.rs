use reacs::prelude::*;

#[system]
pub fn leaf_system() {}

#[component]
pub struct Health(pub i32);

#[signal(entity = target)]
pub struct Damage {
    pub target: Entity,
    pub amount: i32,
}

#[resource]
pub struct CombatLog {
    pub lines: Vec<String>,
}

#[param]
pub struct Combat {
    pub health: Component<Health>,
    pub log: Resource<CombatLog>,
    #[read]
    pub tick: Resource<Tick>,
}

#[system]
pub fn combat_phase() {}
