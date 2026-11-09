use reacs::prelude::*;

#[signal(input)]
pub struct Order(pub u64);

#[signal(input, entity)]
pub struct Ping(pub Entity);

#[signal(input)]
pub struct Unread(pub u64);

#[signal(input)]
pub struct OpenBurn(pub u64);

#[signal(input)]
pub struct CloseBurn;

#[signal]
pub struct Burn(pub u64);

#[signal]
pub struct Greeting(pub u64);

#[component]
pub struct Health(pub i64);

#[resource]
pub struct Config(pub u64);

#[resource]
pub struct Seen {
    pub orders: Vec<(u64, u64)>,
    pub pings: Vec<Entity>,
    pub burns: Vec<(u64, u64)>,
    pub greetings: Vec<(u64, u64)>,
}

#[resource]
pub struct OpenBurnId(pub Option<u64>);

#[system]
fn reader(
    mut orders: Mut<Signal<Order>>,
    mut pings: Mut<Signal<Ping>>,
    mut greetings: Mut<Signal<Greeting>>,
    mut seen: Mut<Resource<Seen>>,
) {
    let tick = orders.tick();
    for order in orders.receive() {
        seen.orders.push((tick, order.0));
    }
    for ping in pings.receive() {
        seen.pings.push(ping.0);
    }
    for greeting in greetings.receive() {
        seen.greetings.push((tick, greeting.0));
    }
}

#[system]
fn burn_control(
    mut open: Mut<Signal<OpenBurn>>,
    mut close: Mut<Signal<CloseBurn>>,
    mut burns: Mut<Signal<Burn>>,
    mut id: Mut<Resource<OpenBurnId>>,
) {
    let tick = open.tick();
    for request in open.receive() {
        (*id).0 = burns.send(Burn(request.0), tick..);
    }
    if close.receive().next().is_some() {
        if let Some(open) = (*id).0.take() {
            burns.cancel(open);
        }
    }
}

#[system(run_after(burn_control))]
fn burn_reader(mut burns: Mut<Signal<Burn>>, mut seen: Mut<Resource<Seen>>) {
    let tick = burns.tick();
    for burn in burns.receive() {
        seen.burns.push((tick, burn.0));
    }
}

#[system]
fn echo_order(mut orders: Mut<Signal<Order>>) {
    if orders.tick() == 4 {
        orders.emit(Order(40));
    }
}

#[test]
fn an_input_reaches_systems_on_the_next_step_only() {
    let mut world = World::new();
    world.step();
    world.input(Order(7));
    world.input(Order(8));
    world.step();
    world.step();

    assert_eq!(
        world.get_resource::<Seen>().unwrap().orders,
        [(1, 7), (1, 8)]
    );
}

#[test]
fn an_input_can_be_sent_when_no_system_reads_it() {
    let mut world = World::new();
    world.input(Unread(1));
    world.step();
    world.input(Unread(2));
    world.step();

    assert_eq!(world.get_resource::<Tick>().unwrap().0, 2);
}

#[test]
fn an_entity_input_for_a_dead_entity_is_not_delivered() {
    let mut units = (Entity::default(), Entity::default());
    let mut world = World::build(|setup| {
        let mut em = setup.entity_manager();
        let (alive, dead) = (em.spawn(), em.spawn());
        em.despawn(dead);
        units = (alive, dead);
    });
    let (alive, dead) = units;

    world.input(Ping(dead));
    world.input(Ping(alive));
    world.step();

    assert_eq!(world.get_resource::<Seen>().unwrap().pings, [alive]);
}

#[test]
fn a_system_can_send_an_input_signal() {
    let mut world = World::new();
    for _ in 0..7 {
        world.step();
    }

    assert_eq!(world.get_resource::<Seen>().unwrap().orders.len(), 1);
    assert_eq!(world.get_resource::<Seen>().unwrap().orders[0].1, 40);
}

#[test]
fn inputs_can_open_and_close_a_window_through_a_system() {
    let mut world = World::new();
    world.input(OpenBurn(5));
    world.step();
    world.step();
    world.input(CloseBurn);
    world.step();
    world.step();

    assert_eq!(
        world.get_resource::<Seen>().unwrap().burns,
        [(0, 5), (1, 5)]
    );
}

#[test]
fn build_runs_before_the_first_tick() {
    let mut unit = Entity::default();
    let world = World::build(|setup| {
        setup.insert_resource(Config(9));
        setup.init_component::<Health>();
        unit = setup.spawn();
        setup.set_component(unit, Health(30));
    });

    assert_eq!(world.get_resource::<Tick>().unwrap().0, 0);
    assert_eq!(world.get_resource::<Config>().unwrap().0, 9);
    assert_eq!(world.get_component::<Health>(unit).unwrap().0, 30);
    assert!(world.entities().is_alive(unit));
}

#[test]
fn a_non_input_signal_sent_in_build_arrives_on_the_first_tick() {
    let mut world = World::build(|setup| {
        setup.signal::<Greeting>().emit(Greeting(3));
    });
    world.step();
    world.step();

    assert_eq!(world.get_resource::<Seen>().unwrap().greetings, [(0, 3)]);
}

#[test]
fn the_same_inputs_give_the_same_saves() {
    let run = || {
        let mut world = World::build(|setup| setup.insert_resource(Config(1)));
        let mut saves = Vec::new();
        for tick in 0..12 {
            if tick % 3 == 0 {
                world.input(Order(tick));
            }
            if tick == 2 {
                world.input(OpenBurn(tick));
            }
            if tick == 8 {
                world.input(CloseBurn);
            }
            world.step();
            saves.push(world.save());
        }
        saves
    };

    assert_eq!(run(), run());
}

#[test]
fn a_loaded_save_keeps_input_signals_working() {
    let mut original = World::new();
    original.input(Order(1));
    original.step();

    let save = original.save();
    let mut restored = World::new();
    restored.load(&save);

    original.input(Order(2));
    restored.input(Order(2));
    original.step();
    restored.step();

    assert_eq!(restored.save(), original.save());
}
