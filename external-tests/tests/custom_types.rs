use reacs::prelude::*;

#[data]
#[derive(PartialEq, Debug)]
pub enum Element {
    #[default]
    Fire,
    Ice,
    Poison {
        strength: u8,
    },
}

#[data]
#[derive(PartialEq, Debug)]
pub struct Hit {
    pub element: Element,
    pub amount: i64,
}

#[data]
#[derive(PartialEq, Debug)]
pub struct Pair<T> {
    pub first: T,
    pub second: T,
}

#[data]
#[derive(PartialEq, Debug)]
pub struct Entry {
    pub hit: Hit,
    pub tick: u64,
    #[serde(skip)]
    pub scratch: u64,
}

#[component]
pub struct Weakness(pub Element);

#[component]
pub struct Resist(pub Pair<Element>);

#[signal(entity)]
pub struct Damage {
    pub target: Entity,
    pub hit: Hit,
}

#[resource]
pub struct Log(pub Vec<Entry>);

#[resource]
pub struct Last(pub Option<Weakness>);

#[data(no_copy)]
#[derive(PartialEq, Debug)]
pub struct Victims {
    pub elements: Vec<Element>,
    pub names: String,
}

#[resource]
pub struct ByElement(pub IndexMap<u8, Victims>);

#[system]
fn attacker(
    mut em: Mut<EntityManager>,
    mut weakness: Mut<Component<Weakness>>,
    mut resist: Mut<Component<Resist>>,
    mut damage: Mut<Sender<Damage>>,
    tick: Resource<Tick>,
) {
    let tick = (*tick).0;
    let target = em.spawn();
    let element = match tick % 3 {
        0 => Element::Fire,
        1 => Element::Ice,
        _ => Element::Poison {
            strength: tick as u8,
        },
    };
    weakness.insert(target, Weakness(element));
    resist.insert(
        target,
        Resist(Pair {
            first: Element::Ice,
            second: Element::Fire,
        }),
    );
    let hit = Hit {
        element,
        amount: tick as i64,
    };
    damage.send(Damage { target, hit }, (In(1), For(4)));
}

#[system(run_after(attacker))]
fn defender(
    mut damage: Mut<Receiver<Damage>>,
    weakness: Component<Weakness>,
    resist: Component<Resist>,
    mut log: Mut<Resource<Log>>,
    mut last: Mut<Resource<Last>>,
    mut by_element: Mut<Resource<ByElement>>,
    tick: Resource<Tick>,
) {
    for damage in damage.receive() {
        let mut hit = damage.hit;
        let weak = weakness.get(damage.target);
        if weak.is_some_and(|weak| weak.0 == hit.element) {
            hit.amount *= 2;
        }
        if resist
            .get(damage.target)
            .is_some_and(|resist| resist.0.first == hit.element)
        {
            hit.amount /= 2;
        }
        (*last).0 = weak;
        let key = match hit.element {
            Element::Fire => 0,
            Element::Ice => 1,
            Element::Poison { .. } => 2,
        };
        let mut victims = (*by_element).0.get(&key).cloned().unwrap_or_default();
        victims.elements.push(hit.element);
        victims.names.push_str("x");
        (*by_element).0.insert(key, victims);
        (*log).0.push(Entry {
            hit,
            tick: (*tick).0,
            scratch: 99,
        });
    }
}

#[test]
fn custom_field_types_survive_save_and_load() {
    let mut original = World::new();
    for _ in 0..10 {
        original.step();
    }

    let save = original.save();
    let mut restored = World::new();
    restored.load(&save);
    assert_eq!(restored.save(), save);
    assert_eq!(restored.resource_hashes(), original.resource_hashes());

    for _ in 0..20 {
        original.step();
        restored.step();
        assert_eq!(restored.save(), original.save());
    }

    let log = &restored.get_resource::<Log>().unwrap().0;
    assert!(log.iter().any(|entry| entry.hit.element == Element::Ice));
    assert!(
        log.iter()
            .any(|entry| matches!(entry.hit.element, Element::Poison { .. }))
    );
    assert!(log.iter().any(|entry| entry.scratch == 0));
    assert!(log.iter().any(|entry| entry.scratch == 99));
    assert!(restored.get_resource::<Last>().unwrap().0.is_some());
    let by_element = &restored.get_resource::<ByElement>().unwrap().0;
    assert_eq!(by_element.len(), 3);
    let expected = &original.get_resource::<ByElement>().unwrap().0;
    assert_eq!(by_element.as_slice(), expected.as_slice());
}
