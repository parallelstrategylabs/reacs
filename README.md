# reacs

[![Crates.io](https://img.shields.io/crates/v/reacs.svg)](https://crates.io/crates/reacs)
[![Documentation](https://docs.rs/reacs/badge.svg)](https://docs.rs/reacs)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)

A reactive, signal-driven ECS on a deterministic, tick-based simulation kernel.

reacs is made to run complex simulations, like those behind RTS and strategy games, within a fixed tick step.
Systems don't query the world. Instead, they react to signals, which name the entities to work on.
The same start state and inputs always give the same results, so deterministic saves and replays come
with the design.

It is a niche tool: if you need queries over every entity with some components, rendering or a
full game engine, see [Is reacs for you?](#is-reacs-for-you).

## Example

```rust,standalone_crate
use reacs::prelude::*;

#[component]
struct Health(i32);

// Safe entity lifecycle: a signal gets dropped if its target is despawned.
#[signal(entity)]
struct Burn {
    target: Entity,
    damage: i32,
}

// Plain signal: news about an entity must outlive it (see "Things that will surprise you").
#[signal]
struct Died(Entity);

#[resource]
struct Graveyard {
    dead: Vec<Entity>,
}

// Where work takes place: a system only awakes when it receives a signal, no observers required.
// Cost scales with signal count, not entities.
#[system]
fn burn(
    mut burns: Mut<Receiver<Burn>>,
    mut health: Mut<Component<Health>>,
    mut entities: Mut<EntityManager>,
    mut died: Mut<Sender<Died>>,
) {
    // Batch logic execution: helps the CPU by processing the same signal one after another.
    for burn in burns.receive() {
        // Friendly fast lookups: random component access is cheap and common.
        let Some(hp) = health.get_mut(burn.target) else { continue };
        hp.0 -= burn.damage;
        if hp.0 <= 0 {
            entities.despawn(burn.target);
            died.emit(Died(burn.target));
        }
    }
}

// Avoid the 1-frame delay: system execution is sequential, and can be customized.
#[system(run_after(burn))]
fn bury(mut deaths: Mut<Receiver<Died>>, mut graveyard: Mut<Resource<Graveyard>>) {
    for died in deaths.receive() {
        graveyard.dead.push(died.0);
    }
}

fn main() {
    let mut unit = Entity::default();
    // Decentralized registration: systems and resources are automatically added to a new world.
    let mut world = World::build(|setup| {
        unit = setup.spawn();
        setup.set_component(unit, Health(10));
        // Declarative signal emission: start next tick, until cancelled or the unit dies.
        setup.signal::<Burn>().send(Burn { target: unit, damage: 4 }, (In(1), ..));
    });

    for _ in 0..5 {
        world.step();
    }

    assert!(!world.entities().is_alive(unit));
    assert_eq!(world.get_resource::<Graveyard>().unwrap().dead, [unit]);
}
```

## How it works

- **Ticks:** `world.step()` runs every system once, in order, then advances the `Tick`. There is one
  tick rate and no frames: renderers read the world after a tick and interpolate on their own.
- **Signals** are messages that name what to work on. `emit` sends one for a single tick. `send`
  takes a window, like `For(5)`, `(At(40), To(50))` or `(In(2), ..)`, and delivers the signal on
  every tick of it. A signal sent before a receiver runs arrives that tick, and one sent after it
  arrives the next tick.
- **Systems** are functions marked `#[system]`. They run one at a time, in an order set with
  `run_before`, `run_after` and groups. A system with a `Mut<Receiver<S>>` sleeps on ticks with nothing
  to receive. A system with a `Mut<Signal<S>>`, or with no receiver, runs every tick.
- **Entities and components:** an `Entity` is an id. Components are small `Copy` structs reached by
  `Entity` in O(1), from a signal. They are never iterated. Your own types used as fields, like an
  enum, are declared with `#[data]`.
- **Resources** hold everything else, one value per type: settings, maps keyed by entity, a seeded
  random generator. `IndexMap` is the map that keeps saves and replays deterministic.
- **The host,** the program around the world, writes into it once, in `World::build`. After that it
  sends input signals, steps, saves and loads, and reads components and resources.

## Getting started

Add reacs to your `Cargo.toml`:

```toml
[dependencies]
reacs = "0.2"
```

`reacs` is the only dependency you need: the macros reach serde and the rest through it.

Systems, resources, components and signals register themselves before `main`, from every crate
linked into the program. `world.print_system_order()` shows everything that runs, in order.

A game's host loop, continuing the example: input signals (`#[signal(input)]`) are the only way to
talk to a running world, and recording them is enough to replay a session.

```rust,ignore
// A player's order, sent by the host.
#[signal(input, entity)]
struct Ignite(Entity);

#[system]
fn ignite(mut orders: Mut<Receiver<Ignite>>, mut burns: Mut<Sender<Burn>>) {
    for order in orders.receive() {
        burns.send(Burn { target: order.0, damage: 4 }, (In(1), ..));
    }
}

fn main() -> std::io::Result<()> {
    let mut unit = Entity::default();
    let mut world = World::build(|setup| {
        unit = setup.spawn();
        setup.set_component(unit, Health(10));
    });
    let mut recorder = world.to_recorder("session.rec")?;

    for tick in 0..100 {
        if tick == 10 {
            world.input(Ignite(unit));
        }
        world.record(&mut recorder).step();
    }
    recorder.finish(&world)?;

    // world.load(&bytes) restores it.
    std::fs::write("game.save", world.save())?;
    Ok(())
}
```

### Plugins

A plugin is just a crate with systems in it: list it in your app's `Cargo.toml` and its systems run.
It orders its systems against the app's, or against another plugin's, by path. Systems and groups
named from another crate must be `pub`:

```rust,ignore
// In a shared crate, game_core: a group is an ordinary empty system.
#[system]
pub fn combat() {}

// In the plugin crate, which depends on game_core:
use game_core::{Health, Poison, combat};
use reacs::prelude::*;

#[system(in_group(combat))]
pub fn poison_ticks(mut poison: Mut<Receiver<Poison>>, mut health: Mut<Component<Health>>) {
    for poison in poison.receive() {
        if let Some(hp) = health.get_mut(poison.target) {
            hp.0 -= poison.damage;
        }
    }
}
```

```toml
[dependencies]
reacs = "0.2"
poison = { path = "plugins/poison" }
```

The app never names `poison_ticks`: listing the crate is enough.

## Features

- **Decentralized registration:** No central App or `add_systems`. Systems, components, resources and
  signals register themselves with an attribute, from your crate and its dependencies, with no extra
  setup.
- **Sleeping systems:** A system is skipped on ticks with no signal to receive. Idle, it costs a few
  nanoseconds.
- **Windowed signals:** One send covers many ticks: `For(5)`, `(At(40), To(50))`, `(In(2), ..)`.
  Work done every tick, like movement, is one signal sent once, not one per entity per tick.
- **Signals bound to entities:** `#[signal(entity)]` signals are dropped when their target entity is
  despawned. No checks for dead entities in your loops.
- **Saves, recordings and replays:** `world.save()` / `load()` for the world state. Record inputs,
  replay with a hash check per resource, and take over a replay at any tick.
- **Fast, portable saves:** Plain data, like most components and signals, is saved and loaded by
  copying memory, a whole component map at once. The same state saves the same bytes on every
  platform, so a save loads anywhere the same app version runs.
- **Tested determinism:** CI compares the state hashes of every tick of a scripted session, saves and
  loads included, across Linux, Windows and macOS, on x86 (32 and 64 bit) and ARM64, in debug and
  release.

## Design principles

- **Process what happens, not what exists:** A tick costs what the signals sent in it cost, not what
  the number of entities or components costs.
- **Push, not pull:** Systems can't query for entities. They react to signals, which name the
  entities to work on.
- **Random access over iteration:** Reaching an entity's data must be O(1) and fast. Only signals
  are iterated, never components.
- **Determinism first:** The same start state and inputs must give the same result on every OS and
  CPU architecture. All world state is saved and hashed, and inputs are the only way in.
- **No cost, then flat, before scaling:** Idle features should cost nothing or close to it. Running
  ones should prefer a fixed cost per tick. Anything that grows with the data needs a reason.
- **Errors as early as possible:** Compile errors first, then panics at startup, and as few runtime
  panics as possible.
- **Memory for speed:** Simulation and strategy users tend to have strong CPUs and plenty of RAM,
  so reacs spends memory where it buys speed.
- **Components are small and Copy:** Each component map has a slot for every entity index, so small
  slots keep memory low, and plain data is cheap to copy, save and hash.

## Is reacs for you?

These limits are deliberate, not missing features.

- **No queries:** There's no way to visit every entity with some components, and an entity's
  components can't be listed.
- **No parallel scheduler:** Systems run one at a time. Parallel work inside a system is planned.
- **Simulation only:** No rendering, no frame rate, no windowing or assets. A renderer reads the world
  after each tick.
- **Plain data:** Components are small and `Copy`. Data whose size varies, like an inventory, lives
  in a resource keyed by `Entity`.

If you need queries, parallel systems or a full engine, [Bevy](https://bevy.org)
([`bevy_ecs`](https://crates.io/crates/bevy_ecs)), [hecs](https://crates.io/crates/hecs) and
[flecs](https://github.com/SanderMertens/flecs) are mature choices.

## Things that will surprise you

- **Signals aren't observers:** Nothing runs the moment a signal is sent. Each system handles all of
  its signals at its place in the order, so a signal sent after a system ran reaches it next tick.
- **Pick the signal kind by who must hear it:** Work on a living entity (`Burn`) uses
  `#[signal(entity)]`. News about an entity (`Died`) uses a plain `#[signal]`, and its receivers
  check `is_alive` when they need to. A `#[signal(entity)] struct Died(Entity)` never arrives, with no
  error, because the entity is dead when it's sent.
- **A sleeping system skips everything it does,** not just its receive loop: a timer counted down in
  a system with a `Receiver` stops on quiet ticks. Use `Mut<Signal<S>>`, a `..` signal or
  `#[param(wake = always)]` for work that must run every tick.
- **Field order sets save speed:** A type whose fields are numbers, `bool`, `Entity` or enums without
  data, with no padding between them, is saved and loaded by copying memory. Anything else is saved
  field by field, which is slower but gives the same bytes. The macros add `#[repr(C)]`, so fields
  stay in the order you write: put the largest first. Prefer `u32` or `u64` to `usize`, which is
  saved as 64-bit everywhere. See `reacs::save` for the rules.
- **Long signals stay until they end:** A signal with no end, or a long window, keeps being delivered,
  and stays in memory and saves, until it's cancelled. Only `#[signal(entity)]` signals are cleaned up
  when their entity dies.
- **Determinism needs your code too:** In systems and anything they call, use `IndexMap` instead of
  `HashMap` or `HashSet`. Don't call float functions like `sin`, `cos`, `atan2`, `exp` or `powf`
  (`+ - * /`, `sqrt` and `abs` are exact everywhere). Don't read time, unseeded randomness, files
  or the environment.
- **An app with no reacs macros** of its own must call `world!()` instead of `World::new()`, or
  `use plugin as _;`, or plugins it never names won't be linked. The same goes for a crate that only
  passes a plugin through without naming it.
- **One reacs version per build:** Two versions of `reacs` in one dependency tree are a build error
  that mentions a "native library" (`links = "reacs"`), since they would split the system registry.

## Status

reacs is young: the API will change before 1.0. A save only loads into the same app version
(`world.set_app_version`), and a recording only replays in the build that made it. Compatibility
across versions isn't designed yet. CI runs on Linux, Windows and macOS on the latest stable Rust.
The minimum supported Rust version is 1.88.

## License

Licensed under the [Apache License, Version 2.0](https://www.apache.org/licenses/LICENSE-2.0).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache 2.0 license, shall be licensed as above, without any additional terms or conditions.
