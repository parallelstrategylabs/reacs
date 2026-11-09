use reacs::prelude::*;

const UNITS: u64 = 24;
const TICKS: u64 = 1200;
const SMALL_WINDOW: u64 = 64;
const CURSE_TICK: u64 = 5;
const CURSE_STARTS: u64 = 900;
const CURSE_BONUS: i32 = 5;
const BURN_LENGTH: u64 = 40;
const JANITOR_EVERY: u64 = 120;

const WRONG_TARGET: u32 = 1;
const LATE_SHIELD: u32 = 2;
const STRAY_CANCEL: u32 = 3;
const NEVER_READ: u32 = 4;
const ARMOR_MATH: u32 = 5;
const SCORE_DRIFT: u32 = 6;
const OLD_CAUSE: u32 = 7;
const KILLING_BLOW: u32 = 8;

const CASES: [(&str, u32); 8] = [
    ("wrong_target", WRONG_TARGET),
    ("late_shield", LATE_SHIELD),
    ("stray_cancel", STRAY_CANCEL),
    ("never_read", NEVER_READ),
    ("armor_math", ARMOR_MATH),
    ("score_drift", SCORE_DRIFT),
    ("old_cause", OLD_CAUSE),
    ("killing_blow", KILLING_BLOW),
];

#[signal(entity = target)]
struct Hit {
    target: Entity,
    attacker: Entity,
    amount: i32,
}

#[signal(entity)]
struct Burn(Entity);

#[signal(entity)]
struct Died(Entity);

#[signal(entity)]
struct CurseMark(Entity);

#[signal]
struct AlarmOld(u32);

#[signal]
struct AlarmNew(u32);

#[component]
struct Health {
    points: i32,
}

#[component]
struct Armor {
    points: i32,
}

#[component]
struct Shield {
    raised_on: u64,
}

#[component]
struct Cursed {
    bonus: i32,
}

#[resource]
struct Scenario {
    bug: u32,
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
struct Units {
    alive: Vec<Entity>,
    cursed: Entity,
}

#[resource]
struct BurnIds {
    ids: Vec<u64>,
}

#[resource]
struct Score {
    points: u32,
    deaths: u32,
    last_kill: Option<(u64, Entity, u32)>,
    alarms_rung: u32,
}

#[system]
fn setup(
    mut manager: Mut<EntityManager>,
    mut health: Mut<Component<Health>>,
    mut armor: Mut<Component<Armor>>,
    mut units: Mut<Resource<Units>>,
    mut marks: Mut<Signal<CurseMark>>,
) {
    let tick = marks.tick();
    if tick == 0 {
        for index in 0..UNITS {
            let unit = manager.spawn();
            health.insert(unit, Health { points: 300 });
            armor.insert(
                unit,
                Armor {
                    points: (index % 3) as i32,
                },
            );
            units.alive.push(unit);
        }
    }
    if tick == CURSE_TICK {
        let target = units.alive[3];
        units.cursed = target;
        marks.emit(CurseMark(target));
    }
}

#[system(run_after(setup))]
fn shield_early(
    mut hits: Mut<Signal<Hit>>,
    mut shields: Mut<Component<Shield>>,
    scenario: Resource<Scenario>,
) {
    if scenario.bug != LATE_SHIELD {
        return;
    }
    let tick = hits.tick();
    for hit in hits.receive() {
        shields.insert(hit.target, Shield { raised_on: tick });
    }
}

#[system(run_after(shield_early))]
fn attack(
    mut hits: Mut<Signal<Hit>>,
    units: Resource<Units>,
    mut rng: Mut<Resource<Rng>>,
    scenario: Resource<Scenario>,
) {
    if units.alive.len() < 2 || hits.tick() % 2 != 0 {
        return;
    }
    let count = 1 + rng.below(4);
    for _ in 0..count {
        let attacker = units.alive[rng.below(units.alive.len() as u64) as usize];
        let mut target = units.alive[rng.below(units.alive.len() as u64) as usize];
        if scenario.bug == WRONG_TARGET && attacker.index() % 7 == 0 {
            target = attacker;
        }
        let amount = 3 + rng.below(4) as i32;
        hits.emit(Hit {
            target,
            attacker,
            amount,
        });
    }
}

#[system(run_after(attack))]
fn shield_late(
    mut hits: Mut<Signal<Hit>>,
    mut shields: Mut<Component<Shield>>,
    scenario: Resource<Scenario>,
) {
    if scenario.bug == LATE_SHIELD {
        return;
    }
    let tick = hits.tick();
    for hit in hits.receive() {
        shields.insert(hit.target, Shield { raised_on: tick });
    }
}

#[system(run_after(shield_late))]
fn igniter(
    mut burns: Mut<Signal<Burn>>,
    units: Resource<Units>,
    mut ids: Mut<Resource<BurnIds>>,
    mut rng: Mut<Resource<Rng>>,
) {
    if rng.below(30) != 0 || units.alive.is_empty() {
        return;
    }
    let tick = burns.tick();
    let target = units.alive[rng.below(units.alive.len() as u64) as usize];
    if let Some(id) = burns.send(Burn(target), tick..tick + BURN_LENGTH) {
        ids.ids.push(id);
    }
}

#[system(run_after(igniter))]
fn janitor(
    mut burns: Mut<Signal<Burn>>,
    mut ids: Mut<Resource<BurnIds>>,
    scenario: Resource<Scenario>,
) {
    if scenario.bug == STRAY_CANCEL && burns.tick() % JANITOR_EVERY == JANITOR_EVERY - 1 {
        for id in ids.ids.drain(..) {
            burns.cancel(id);
        }
    }
}

#[system(run_after(janitor))]
fn burning(mut burns: Mut<Signal<Burn>>, mut hits: Mut<Signal<Hit>>) {
    let targets: Vec<Entity> = burns.receive().map(|burn| burn.0).collect();
    for target in targets {
        hits.emit(Hit {
            target,
            attacker: target,
            amount: 1,
        });
    }
}

#[system(run_after(burning))]
fn alarm_trigger(
    mut old: Mut<Signal<AlarmOld>>,
    mut new: Mut<Signal<AlarmNew>>,
    scenario: Resource<Scenario>,
) {
    if old.tick() % 50 != 0 {
        return;
    }
    if scenario.bug == NEVER_READ {
        new.emit(AlarmNew(1));
    } else {
        old.emit(AlarmOld(1));
    }
}

#[system(run_after(alarm_trigger))]
fn alarm_bell(mut alarms: Mut<Signal<AlarmOld>>, mut score: Mut<Resource<Score>>) {
    score.alarms_rung += alarms.receive().count() as u32;
}

#[system(run_after(alarm_bell))]
fn curse_keeper(mut marks: Mut<Signal<CurseMark>>, mut cursed: Mut<Component<Cursed>>) {
    let targets: Vec<Entity> = marks.receive().map(|mark| mark.0).collect();
    for target in targets {
        cursed.insert(target, Cursed { bonus: CURSE_BONUS });
    }
}

#[system(run_after(curse_keeper))]
fn apply_damage(
    mut hits: Mut<Signal<Hit>>,
    mut health: Mut<Component<Health>>,
    armor: Component<Armor>,
    cursed: Component<Cursed>,
    mut died: Mut<Signal<Died>>,
    mut score: Mut<Resource<Score>>,
    scenario: Resource<Scenario>,
) {
    let tick = hits.tick();
    let mut blows: Vec<(Entity, u32)> = Vec::new();
    for hit in hits.receive() {
        let plate = armor.get(hit.target).map_or(0, |x| x.points);
        let mut taken = if scenario.bug == ARMOR_MATH {
            hit.amount * (1 + plate * 2)
        } else {
            (hit.amount - plate).max(1)
        };
        if scenario.bug == OLD_CAUSE && tick >= CURSE_STARTS {
            taken += cursed.get(hit.target).map_or(0, |x| x.bonus);
        }
        match blows.iter_mut().find(|blow| blow.0 == hit.target) {
            Some(blow) => blow.1 += 1,
            None => blows.push((hit.target, 1)),
        }
        let Some(target) = health.get_mut(hit.target) else {
            continue;
        };
        let was_alive = target.points > 0;
        target.points -= taken;
        if was_alive && target.points <= 0 {
            died.emit(Died(hit.target));
            let hits_that_tick = blows.iter().find(|x| x.0 == hit.target).map_or(1, |x| x.1);
            if hits_that_tick > 1 || score.last_kill.is_none() {
                score.last_kill = Some((tick, hit.target, hits_that_tick));
            }
        }
    }
}

#[system(run_after(apply_damage))]
fn funeral(
    mut died: Mut<Signal<Died>>,
    mut manager: Mut<EntityManager>,
    mut units: Mut<Resource<Units>>,
    mut score: Mut<Resource<Score>>,
    scenario: Resource<Scenario>,
) {
    let dead: Vec<Entity> = died.receive().map(|x| x.0).collect();
    for entity in dead {
        score.points += 1;
        score.deaths += 1;
        if scenario.bug == SCORE_DRIFT && entity.index() % 2 == 0 {
            score.points += 1;
        }
        units.alive.retain(|unit| *unit != entity);
        manager.despawn(entity);
    }
}

fn entity_name(entity: Entity) -> String {
    format!("{}v{}", entity.index(), entity.to_bits() >> 32)
}

fn symptom(bug: u32, world: &World) -> String {
    let score = world.get_resource::<Score>().unwrap();
    let units = world.get_resource::<Units>().unwrap();
    match bug {
        WRONG_TARGET => "Some units lose health even though no enemy attacked them.".to_string(),
        LATE_SHIELD => {
            "Shields go up one tick after a unit is hit, not on the same tick.".to_string()
        }
        STRAY_CANCEL => {
            format!("Burns are meant to last {BURN_LENGTH} ticks, but some stop early.")
        }
        NEVER_READ => format!(
            "The alarm should ring every 50 ticks. It rang {} times.",
            score.alarms_rung
        ),
        ARMOR_MATH => "Armored units lose far more health than unarmored ones.".to_string(),
        SCORE_DRIFT => format!(
            "The final score is {}, but only {} units died.",
            score.points, score.deaths
        ),
        OLD_CAUSE => format!(
            "From tick {CURSE_STARTS}, unit {} takes extra damage from every hit. Nothing unusual is sent near that tick.",
            entity_name(units.cursed)
        ),
        KILLING_BLOW => match score.last_kill {
            Some((tick, unit, hits)) => format!(
                "Unit {} died on tick {tick}, hit {hits} times that tick. Which attacker landed the killing blow?",
                entity_name(unit)
            ),
            None => "Nobody died in this run.".to_string(),
        },
        _ => String::new(),
    }
}

fn main() -> std::io::Result<()> {
    let name = std::env::args().nth(1).unwrap_or_default();
    let Some((_, bug)) = CASES.iter().find(|case| case.0 == name) else {
        let names: Vec<&str> = CASES.iter().map(|case| case.0).collect();
        eprintln!("usage: bug_cases <case>, one of: {}", names.join(", "));
        std::process::exit(1);
    };

    let mut world = World::new();
    world.insert_resource(Scenario { bug: *bug });
    let tracker = world.to_tracker();
    let mut tracker = if *bug == OLD_CAUSE {
        tracker.with_window(SMALL_WINDOW)
    } else {
        tracker
    };
    for _ in 0..TICKS {
        tracker.step(&world);
        world.step();
    }

    let path = std::env::temp_dir().join(format!("reacs-bug-{name}.bin"));
    tracker.save_to(&path)?;
    println!("recording: {}", path.display());
    println!("symptom: {}", symptom(*bug, &world));
    Ok(())
}
