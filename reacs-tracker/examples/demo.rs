use reacs::prelude::*;
use std::time::Duration;

const MAX_ALIVE: usize = 60;
const QUIET_EVERY: u64 = 4;
const QUIET_LENGTH: u64 = 40;

#[signal]
struct SpawnRequest {
    kind: u32,
}

#[signal]
struct Order {
    strength: u32,
}

#[signal]
struct Wave {
    power: u32,
}

#[signal]
struct Alert {
    level: u32,
}

#[signal]
struct Echo(u32);

#[signal(entity = target)]
struct Damage {
    target: Entity,
    amount: i32,
}

#[signal(entity)]
struct Burn(Entity);

#[signal(entity)]
struct Regen(Entity);

#[signal(entity)]
struct Died(Entity);

#[component]
struct Health {
    points: i32,
}

#[resource]
struct Rng {
    state: u64,
}

impl Rng {
    fn below(&mut self, limit: u64) -> u64 {
        if self.state == 0 {
            self.state = 0x9E37_79B9_7F4A_7C15;
        }
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state % limit
    }
}

#[resource]
struct Alive {
    entities: Vec<Entity>,
}

#[resource]
struct WaveIds {
    ids: Vec<u64>,
}

#[system]
fn sensors(
    mut alerts: Mut<Signal<Alert>>,
    mut echoes: Mut<Signal<Echo>>,
    mut orders: Mut<Signal<Order>>,
) {
    let heard = alerts.receive().map(|alert| alert.level).sum::<u32>()
        + echoes.receive().map(|echo| echo.0).sum::<u32>();
    if heard > 0 {
        orders.emit(Order { strength: heard });
    }
}

#[system(run_after(sensors))]
fn director(
    mut spawns: Mut<Signal<SpawnRequest>>,
    mut waves: Mut<Signal<Wave>>,
    mut rng: Mut<Resource<Rng>>,
    mut wave_ids: Mut<Resource<WaveIds>>,
) {
    let tick = spawns.tick();
    if tick == 0 {
        waves.send(Wave { power: 1 }, ..);
    }
    if (tick / QUIET_LENGTH) % QUIET_EVERY == QUIET_EVERY - 1 {
        return;
    }

    for kind in 0..rng.below(6) as u32 {
        spawns.emit(SpawnRequest { kind });
    }
    if rng.below(3) == 0 {
        let start = tick + rng.below(5);
        let end = start + 1 + rng.below(12);
        if let Some(id) = waves.send(Wave { power: 2 }, start..end) {
            wave_ids.ids.push(id);
        }
    }
}

#[system(run_after(director))]
fn spawner(
    mut spawns: Mut<Signal<SpawnRequest>>,
    mut manager: Mut<EntityManager>,
    mut health: Mut<Component<Health>>,
    mut alive: Mut<Resource<Alive>>,
    mut regen: Mut<Signal<Regen>>,
) {
    for request in spawns.receive() {
        if alive.entities.len() >= MAX_ALIVE {
            continue;
        }
        let entity = manager.spawn();
        health.insert(
            entity,
            Health {
                points: 10 + request.kind as i32 * 5,
            },
        );
        alive.entities.push(entity);
        if request.kind % 3 == 0 {
            regen.send(Regen(entity), ..);
        }
    }
}

#[system(run_after(spawner))]
fn igniter(mut burns: Mut<Signal<Burn>>, alive: Resource<Alive>, mut rng: Mut<Resource<Rng>>) {
    let tick = burns.tick();
    if alive.entities.is_empty() || rng.below(3) != 0 {
        return;
    }
    let target = alive.entities[rng.below(alive.entities.len() as u64) as usize];
    let end = tick + 2 + rng.below(16);
    burns.send(Burn(target), tick + 1..end);
}

#[system(run_after(igniter))]
fn combat_phase() {}

#[system(in_group(combat_phase))]
fn soldiers(
    mut orders: Mut<Signal<Order>>,
    mut damage: Mut<Signal<Damage>>,
    alive: Resource<Alive>,
    mut rng: Mut<Resource<Rng>>,
) {
    let strength: u32 = orders.receive().map(|order| order.strength).sum();
    if alive.entities.is_empty() {
        return;
    }
    for _ in 0..strength.min(12) + rng.below(3) as u32 {
        let target = alive.entities[rng.below(alive.entities.len() as u64) as usize];
        damage.emit(Damage { target, amount: 3 });
    }
}

#[system(in_group(combat_phase))]
fn flames(mut burns: Mut<Signal<Burn>>, mut damage: Mut<Signal<Damage>>) {
    for &Burn(target) in burns.receive() {
        damage.emit(Damage { target, amount: 1 });
    }
}

#[system(run_after(combat_phase))]
fn medics(mut regen: Mut<Signal<Regen>>, mut health: Mut<Component<Health>>) {
    for &Regen(entity) in regen.receive() {
        if let Some(health) = health.get_mut(entity) {
            health.points += 1;
        }
    }
}

#[system(run_after(medics))]
fn wounds(
    mut damage: Mut<Signal<Damage>>,
    mut health: Mut<Component<Health>>,
    mut died: Mut<Signal<Died>>,
    mut alerts: Mut<Signal<Alert>>,
) {
    let mut hits = 0;
    for hit in damage.receive() {
        hits += 1;
        if let Some(health) = health.get_mut(hit.target) {
            health.points -= hit.amount;
            if health.points <= 0 {
                died.emit(Died(hit.target));
            }
        }
    }
    if hits > 4 {
        alerts.emit(Alert { level: 1 });
    }
}

#[system(run_after(wounds))]
fn reaper(
    mut died: Mut<Signal<Died>>,
    mut manager: Mut<EntityManager>,
    mut alive: Mut<Resource<Alive>>,
    mut echoes: Mut<Signal<Echo>>,
) {
    for &Died(entity) in died.receive() {
        if manager.is_alive(entity) {
            manager.despawn(entity);
            alive.entities.retain(|x| *x != entity);
            echoes.emit(Echo(1));
        }
    }
}

#[system(run_after(reaper))]
fn wave_watch(
    mut waves: Mut<Signal<Wave>>,
    mut alerts: Mut<Signal<Alert>>,
    mut rng: Mut<Resource<Rng>>,
) {
    let total: u32 = waves.receive().map(|wave| wave.power).sum();
    if total > 2 && rng.below(4) == 0 {
        alerts.emit(Alert { level: total });
    }
}

#[system(run_after(wave_watch))]
fn meddler(
    mut waves: Mut<Signal<Wave>>,
    mut wave_ids: Mut<Resource<WaveIds>>,
    mut rng: Mut<Resource<Rng>>,
) {
    if wave_ids.ids.is_empty() || rng.below(5) != 0 {
        return;
    }
    let index = rng.below(wave_ids.ids.len() as u64) as usize;
    let id = wave_ids.ids.swap_remove(index);
    if rng.below(2) == 0 {
        waves.cancel(id);
    } else {
        waves.modify(id, Wave { power: 7 });
    }
}

#[system(run_after(meddler))]
fn auditor(
    mut alerts: Mut<Signal<Alert>>,
    mut echoes: Mut<Signal<Echo>>,
    mut rng: Mut<Resource<Rng>>,
) {
    if alerts.tick() % 2 == 0 {
        let _ = alerts.receive().count();
    }
    if rng.below(6) == 0 {
        echoes.emit(Echo(2));
    }
}

fn main() -> std::io::Result<()> {
    let ticks: u64 = std::env::args()
        .nth(1)
        .and_then(|ticks| ticks.parse().ok())
        .unwrap_or(3600);

    let port: u16 = std::env::args()
        .nth(2)
        .and_then(|port| port.parse().ok())
        .unwrap_or(7878);

    let mut world = World::new();
    let mut tracker = world.to_tracker();
    let port = tracker.listen(port)?;
    println!("running {ticks} ticks, connect with: cargo run -p reacs-tracker -- --port {port}");

    for _ in 0..ticks {
        tracker.step(&world);
        world.step();
        std::thread::sleep(Duration::from_millis(16));
    }

    let path = std::env::temp_dir().join("reacs-tracker-demo.bin");
    tracker.save_to(&path)?;
    println!(
        "saved, open with: cargo run -p reacs-tracker -- {}",
        path.display()
    );
    Ok(())
}
