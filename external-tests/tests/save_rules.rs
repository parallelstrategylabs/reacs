//! Each save rule as a user crate meets it: the rule the macros pick for each kind of type, and
//! that every rule saves the same bytes for the same state and resumes a run exactly.

use reacs::prelude::*;
use std::num::Wrapping;
use std::time::Duration;

/// Plain: numbers, no padding.
#[component]
pub struct Position {
    pub x: i32,
    pub y: i32,
}

#[data]
#[derive(PartialEq, Debug)]
pub enum Team {
    #[default]
    Red,
    Green,
    Blue,
}

/// Plain, checked: an enum without data and a `bool`, no padding.
#[component]
pub struct Status {
    pub level: u16,
    pub team: Team,
    pub alive: bool,
}

/// Field by field: padding after `urgent`.
#[component]
pub struct Target {
    pub who: Entity,
    pub urgent: bool,
}

#[data]
#[derive(PartialEq, Debug)]
pub enum Order {
    #[default]
    Idle,
    Move {
        x: i32,
        y: i32,
    },
    Attack(Entity),
}

/// Field by field: an enum with data.
#[component]
pub struct Orders(pub Order);

/// Types from other crates: one copied as memory, one encoded with serde.
#[component]
pub struct Timer {
    #[save(plain)]
    pub ticks: Wrapping<u32>,
    #[save(serde)]
    pub every: Duration,
}

/// A padded payload: a `u32` after the signal's `u64` id.
#[signal]
pub struct Ping(pub u32);

/// A plain payload.
#[signal(entity)]
pub struct Step(pub Entity);

#[data(no_copy)]
#[derive(PartialEq, Debug)]
pub struct Squad {
    pub id: u32,
    pub members: Vec<Entity>,
    pub name: String,
}

#[resource]
pub struct Army {
    pub squads: Vec<Squad>,
    pub statuses: Vec<Status>,
    pub orders: IndexMap<Entity, Order>,
    pub pings: u64,
}

#[signal(input)]
pub struct Spawn(pub u32);

#[param]
pub struct Parts {
    position: Component<Position>,
    status: Component<Status>,
    target: Component<Target>,
    orders: Component<Orders>,
    timer: Component<Timer>,
}

fn team(i: u32) -> Team {
    match i % 3 {
        0 => Team::Red,
        1 => Team::Green,
        _ => Team::Blue,
    }
}

fn order(i: u32, entity: Entity) -> Order {
    match i % 3 {
        0 => Order::Idle,
        1 => Order::Move {
            x: i as i32,
            y: -(i as i32),
        },
        _ => Order::Attack(entity),
    }
}

#[system]
fn spawn(
    mut spawns: Mut<Receiver<Spawn>>,
    mut entities: Mut<EntityManager>,
    mut parts: Mut<Parts>,
    mut pings: Mut<Sender<Ping>>,
    mut steps: Mut<Sender<Step>>,
    mut army: Mut<Resource<Army>>,
) {
    for &Spawn(count) in spawns.receive() {
        let id = army.squads.len() as u32;
        let mut squad = Squad {
            id,
            members: Vec::new(),
            name: format!("squad {id}"),
        };
        for i in 0..count {
            let entity = entities.spawn();
            let status = Status {
                level: (i % 50) as u16,
                team: team(i),
                alive: i % 4 != 0,
            };
            parts.position.insert(
                entity,
                Position {
                    x: i as i32,
                    y: -(i as i32),
                },
            );
            parts.status.insert(entity, status);
            parts.target.insert(
                entity,
                Target {
                    who: entity,
                    urgent: i % 2 == 0,
                },
            );
            parts.orders.insert(entity, Orders(order(i, entity)));
            parts.timer.insert(
                entity,
                Timer {
                    ticks: Wrapping(u32::MAX - i),
                    every: Duration::from_millis(u64::from(i)),
                },
            );
            pings.send(Ping(i), (In(1), ..));
            steps.send(Step(entity), (In(1), ..));
            army.statuses.push(status);
            army.orders.insert(entity, order(i, entity));
            squad.members.push(entity);
        }
        army.squads.push(squad);
    }
}

#[system(run_after(spawn))]
fn walk(
    mut steps: Mut<Receiver<Step>>,
    mut position: Mut<Component<Position>>,
    mut timer: Mut<Component<Timer>>,
) {
    steps.receive().for_each(|&Step(entity)| {
        if let Some(position) = position.get_mut(entity) {
            position.x += 1;
        }
        if let Some(timer) = timer.get_mut(entity) {
            timer.ticks += 1;
        }
    });
}

#[system(run_after(spawn))]
fn listen(mut pings: Mut<Receiver<Ping>>, mut army: Mut<Resource<Army>>) {
    pings
        .receive()
        .for_each(|&Ping(n)| army.pings += u64::from(n));
}

fn new_world() -> World {
    World::build(|setup| {
        setup.signal::<Spawn>().emit(Spawn(300));
    })
}

fn play(world: &mut World, ticks: std::ops::Range<u32>) {
    for tick in ticks {
        if tick % 10 == 5 {
            world.input(Spawn(50 + tick));
        }
        world.step();
    }
}

#[test]
fn the_macros_pick_each_rule() {
    const {
        assert!(<Position as Save>::PLAIN && !<Position as Save>::CHECKED);
        assert!(<Status as Save>::PLAIN && <Status as Save>::CHECKED);
        assert!(<Team as Save>::PLAIN && <Team as Save>::CHECKED);
        assert!(!<Target as Save>::PLAIN);
        assert!(!<Orders as Save>::PLAIN);
        assert!(!<Timer as Save>::PLAIN);
        assert!(!<Squad as Save>::PLAIN);
        assert!(<Ping as Save>::PLAIN && <Step as Save>::PLAIN);
    };
    // `#[data]` gives enums without data a one-byte tag, and the macros keep declaration order.
    assert_eq!(std::mem::size_of::<Team>(), 1);
    assert_eq!(std::mem::offset_of!(Status, alive), 3);
}

#[test]
fn worlds_in_the_same_state_save_the_same_bytes() {
    let mut first = new_world();
    play(&mut first, 0..30);
    let first = first.save();

    // Leave garbage in freed heap memory (small blocks, which the allocator reuses), so padding in
    // the next world's storage differs from the first's: a save that copied padding would differ.
    let garbage: Vec<Vec<u8>> = (0..4096).map(|_| vec![0xa5; 16 << 10]).collect();
    drop(std::hint::black_box(garbage));

    let mut second = new_world();
    play(&mut second, 0..30);
    assert_eq!(second.save(), first);
}

#[test]
fn every_rule_resumes_byte_identical_after_load() {
    let mut original = new_world();
    play(&mut original, 0..20);
    let save = original.save();

    let mut restored = new_world();
    restored.load(&save);
    assert_eq!(restored.save(), save);
    assert_eq!(restored.resource_hashes(), original.resource_hashes());

    for tick in 20..60 {
        play(&mut original, tick..tick + 1);
        play(&mut restored, tick..tick + 1);
        assert_eq!(restored.save(), original.save(), "tick {tick}");
    }
    let army = original.get_resource::<Army>().unwrap();
    assert!(army.squads.len() > 3 && army.pings > 0);
}
