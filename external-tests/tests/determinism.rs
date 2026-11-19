use reacs::prelude::*;
use reacs::registry::get_registry_entry;
use reacs::state_hash::hash_bytes;
use std::fmt::Write as _;

#[component]
pub struct Position(pub f32, pub f32);

#[component]
pub struct Velocity(pub f32, pub f32);

#[component]
pub struct Health(pub i32);

#[component]
pub struct Luck(pub Rng);

#[signal(input)]
pub struct SpawnOrder {
    pub count: u32,
    pub seed: u32,
}

#[signal(input)]
pub struct AttackOrder {
    pub seed: u32,
}

#[signal(entity)]
pub struct Move(pub Entity);

#[signal(entity = target)]
pub struct Damage {
    pub target: Entity,
    pub amount: i32,
}

#[signal]
pub struct Died {
    pub unit: Entity,
    pub x: f32,
    pub y: f32,
}

#[resource]
pub struct Army {
    pub units: Vec<Entity>,
}

#[resource]
pub struct Loot {
    pub carried: IndexMap<Entity, u32>,
}

#[resource]
pub struct Fate {
    pub rng: Rng,
    pub marked: IndexSet<Entity>,
}

#[resource]
pub struct Stats {
    pub spawned: u64,
    pub died: u64,
    pub graves: Vec<(f32, f32)>,
}

fn mix(seed: u32) -> u32 {
    let mut x = seed.wrapping_mul(0x9E37_79B9) ^ 0x85EB_CA6B;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^ (x >> 12)
}

// A system takes one argument per param, so a busy one passes clippy's limit of 7.
#[allow(clippy::too_many_arguments)]
#[system]
fn spawn(
    mut orders: Mut<Receiver<SpawnOrder>>,
    mut entities: Mut<EntityManager>,
    mut position: Mut<Component<Position>>,
    mut velocity: Mut<Component<Velocity>>,
    mut health: Mut<Component<Health>>,
    mut moves: Mut<Sender<Move>>,
    mut army: Mut<Resource<Army>>,
    mut world: Mut<SpawnBook>,
) {
    for order in orders.receive() {
        for i in 0..order.count {
            let r = mix(order.seed.wrapping_add(i));
            let unit = entities.spawn();
            position.insert(
                unit,
                Position((r % 1000) as f32 * 0.37, (r >> 10) as f32 / 7.0),
            );
            velocity.insert(
                unit,
                Velocity((r % 17) as f32 - 8.5, (r % 13) as f32 * 0.25),
            );
            health.insert(unit, Health(20 + (r % 40) as i32));
            moves.send(Move(unit), ..);
            army.units.push(unit);
            world.loot.carried.insert(unit, r % 100);
            world
                .luck
                .insert(unit, Luck(Rng::keyed(order.seed as u64, &[unit.to_bits()])));
            world.stats.spawned += 1;
        }
    }
}

#[param(write)]
pub struct SpawnBook {
    pub loot: Resource<Loot>,
    pub stats: Resource<Stats>,
    pub luck: Component<Luck>,
}

#[system(run_after(spawn))]
fn movement(
    mut moves: Mut<Receiver<Move>>,
    mut position: Mut<Component<Position>>,
    mut velocity: Mut<Component<Velocity>>,
    mut luck: Mut<Component<Luck>>,
) {
    moves.receive().for_each(|&Move(unit)| {
        if let (Some(p), Some(v), Some(l)) = (
            position.get_mut(unit),
            velocity.get_mut(unit),
            luck.get_mut(unit),
        ) {
            p.0 += v.0 * 0.5 + l.0.range(-0.05f32..0.05);
            p.1 += v.1 * 0.5;
            v.0 *= 0.98;
            v.1 = v.1 * 0.97 + 0.01;
        }
    });
}

#[system(run_after(movement))]
fn attack(
    mut orders: Mut<Receiver<AttackOrder>>,
    army: Resource<Army>,
    mut damage: Mut<Sender<Damage>>,
    mut fate: Mut<Resource<Fate>>,
    tick: Tick,
) {
    for order in orders.receive() {
        if army.units.is_empty() {
            continue;
        }
        for i in 0..4 {
            let r = mix(order.seed ^ i);
            let target = army.units[r as usize % army.units.len()];
            let amount = fate.rng.range(1..=2) + (r % 4) as i32;
            if Rng::keyed(order.seed as u64, &[tick.get(), target.to_bits()]).chance(0.3) {
                fate.marked.insert(target);
            }
            damage.send(
                Damage { target, amount },
                (In(r as u64 % 3), For(1 + r as u64 % 20)),
            );
        }
    }
}

#[system(run_after(attack))]
fn apply_damage(
    mut hits: Mut<Receiver<Damage>>,
    mut health: Mut<Component<Health>>,
    position: Component<Position>,
    mut entities: Mut<EntityManager>,
    mut died: Mut<Sender<Died>>,
) {
    for hit in hits.receive() {
        let Some(hp) = health.get_mut(hit.target) else {
            continue;
        };
        hp.0 -= hit.amount;
        if hp.0 <= 0 {
            let Position(x, y) = position.get(hit.target).unwrap_or_default();
            entities.despawn(hit.target);
            died.emit(Died {
                unit: hit.target,
                x,
                y,
            });
        }
    }
}

#[system(run_after(apply_damage))]
fn bury(
    mut deaths: Mut<Receiver<Died>>,
    mut army: Mut<Resource<Army>>,
    mut book: Mut<SpawnBook>,
    mut fate: Mut<Resource<Fate>>,
) {
    for death in deaths.receive() {
        if let Some(i) = army.units.iter().position(|&unit| unit == death.unit) {
            army.units.swap_remove(i);
        }
        book.loot.carried.swap_remove(&death.unit);
        fate.marked.swap_remove(&death.unit);
        book.stats.died += 1;
        book.stats.graves.push((death.x, death.y));
    }
}

const TICKS: u64 = 400;
const SAVE_AT: u64 = 150;
const LOAD_AT: u64 = 190;

fn send_inputs(world: &mut World, tick: u64) {
    let r = mix(tick as u32 ^ 0xA5A5);
    if tick.is_multiple_of(25) && tick < 300 {
        world.input(SpawnOrder {
            count: 8 + r % 24,
            seed: r,
        });
    }
    if r.is_multiple_of(3) {
        world.input(AttackOrder { seed: r });
    }
}

fn tick_line(world: &World, tick: u64) -> String {
    let mut line = format!("{tick} save={:016x}", hash_bytes(&world.save()));
    for (id, hash) in world.resource_hashes() {
        let name = get_registry_entry(id).map_or("?", |entry| entry.type_name);
        write!(line, " {name}={hash:016x}").unwrap();
    }
    line
}

fn run_session() -> Vec<String> {
    let mut world = World::new();
    let mut lines = Vec::new();
    let mut saved = None;
    let mut loaded = false;

    while world.tick() < TICKS {
        let tick = world.tick();
        if tick == SAVE_AT && saved.is_none() {
            saved = Some(world.save());
        }
        if tick == LOAD_AT && !loaded {
            world.load(saved.as_ref().unwrap());
            loaded = true;
            lines.push(format!("load to {SAVE_AT}"));
            continue;
        }
        send_inputs(&mut world, tick);
        world.step();
        lines.push(tick_line(&world, tick));
    }

    let stats = world.get_resource::<Stats>().unwrap();
    assert!(stats.spawned > 100, "the session should spawn units");
    assert!(stats.died > 50, "the session should kill units");
    assert!(stats.spawned > stats.died, "some units should survive");
    let fate = world.get_resource::<Fate>().unwrap();
    assert_ne!(fate.rng, Rng::default(), "the session should draw numbers");
    assert!(!fate.marked.is_empty(), "the session should mark units");
    lines
}

#[test]
#[cfg_attr(miri, ignore)]
fn a_scripted_session_gives_the_same_hashes_every_run() {
    let first = run_session();
    let second = run_session();
    assert_eq!(first, second);

    if let Ok(dir) = std::env::var("REACS_DETERMINISM_DIR") {
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = std::path::Path::new(&dir).join(format!("{profile}.txt"));
        std::fs::write(path, first.join("\n") + "\n").unwrap();
    }
}
