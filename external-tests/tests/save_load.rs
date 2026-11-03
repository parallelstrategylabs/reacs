use reacs::Tick;
use reacs::prelude::*;

#[signal]
pub struct Spawned(pub Entity);

#[signal]
pub struct Burn(pub Entity);

#[signal]
pub struct Late(pub u64);

#[component]
pub struct Health(pub i64);

#[resource]
pub struct Trace(pub u64);

#[resource]
pub struct Living(pub Vec<Entity>);

fn mix(trace: &mut Trace, value: u64) {
    trace.0 = trace.0.wrapping_mul(1_000_003).wrapping_add(value);
}

#[system(run_before(spawner))]
fn late_receiver(mut late: Mut<Signal<Late>>, mut trace: Mut<Resource<Trace>>) {
    for signal in late.receive() {
        mix(&mut trace, signal.0);
    }
}

#[system]
fn spawner(
    mut em: Mut<EntityManager>,
    mut spawned: Mut<Signal<Spawned>>,
    mut burn: Mut<Signal<Burn>>,
    mut late: Mut<Signal<Late>>,
    mut living: Mut<Resource<Living>>,
    tick: Resource<Tick>,
) {
    let tick = (*tick).0;
    let entity = em.spawn();
    (*living).0.push(entity);
    spawned.emit(Spawned(entity));
    burn.send(Burn(entity), tick + 1..tick + 4);
    late.emit(Late(tick));

    if tick % 3 == 2 {
        em.despawn((*living).0.remove(0));
    }
}

#[system(run_after(spawner))]
fn health_setter(mut spawned: Mut<Signal<Spawned>>, mut health: Mut<Component<Health>>) {
    let entities: Vec<_> = spawned.receive().map(|signal| signal.0).collect();
    for entity in entities {
        health.insert(entity, Health(100));
    }
}

#[system(run_after(health_setter))]
fn burner(
    mut burn: Mut<Signal<Burn>>,
    mut health: Mut<Component<Health>>,
    mut trace: Mut<Resource<Trace>>,
) {
    for signal in burn.receive() {
        if let Some(value) = health.get_mut(signal.0) {
            value.0 -= 7;
            mix(&mut trace, value.0 as u64);
        }
    }
}

fn step(world: &mut World) {
    world.run_systems();
    world.cleanup_signals();
    world.get_resource_mut::<Tick>().unwrap().0 += 1;
}

fn new_world() -> World {
    let mut world = World::new();
    world.init_resource::<Tick>();
    world
}

#[test]
fn restored_run_matches_original() {
    let mut original = new_world();
    for _ in 0..10 {
        step(&mut original);
    }

    let save = original.save();
    let mut restored = new_world();
    restored.load(&save);
    assert_eq!(restored.save(), save);

    for _ in 0..20 {
        step(&mut original);
        step(&mut restored);
        assert_eq!(restored.save(), original.save());
    }

    assert_ne!(original.get_resource::<Trace>().unwrap().0, 0);
}

#[test]
fn loading_into_a_used_world_rewinds_it() {
    let mut world = new_world();
    for _ in 0..7 {
        step(&mut world);
    }

    let save = world.save();
    let mut expected = Vec::new();
    for _ in 0..7 {
        step(&mut world);
        expected.push(world.save());
    }

    world.load(&save);
    for bytes in expected {
        step(&mut world);
        assert_eq!(world.save(), bytes);
    }
}
