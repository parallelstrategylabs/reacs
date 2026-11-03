use reacs::component::ComponentMap;
use reacs::prelude::*;
use reacs::registry::get_registry_entry;
use reacs::signal::Signals;

#[resource]
pub struct Score(pub u64);

#[component]
pub struct Health(pub i32);

#[signal]
pub struct Hit(pub u64);

#[derive(DeriveHashed)]
pub struct Plain;

#[param]
pub struct ScoreView {
    pub score: Resource<Score>,
}

#[system]
fn group() {}

#[system(in_group(group))]
fn member(_view: ScoreView) {}

#[test]
fn macros_register_types_with_only_reacs_as_dependency() {
    assert!(get_registry_entry(Score::TYPE_UUID).is_some());
    assert!(get_registry_entry(ComponentMap::<Health>::TYPE_UUID).is_some());
    assert!(get_registry_entry(Signals::<Hit>::TYPE_UUID).is_some());
    assert_ne!(Plain::TYPE_UUID, Score::TYPE_UUID);
}

#[test]
fn registered_resource_round_trips_through_serde() {
    let entry = get_registry_entry(Score::TYPE_UUID).unwrap();

    let bytes = (entry.serialize)(&Score(42) as &dyn core::any::Any);
    let restored = (entry.deserialize)(&bytes).downcast::<Score>().unwrap();

    assert_eq!(restored.0, 42);
}

#[test]
fn group_runs_before_member() {
    let names: Vec<_> = system_graph().iter().map(|s| s.name).collect();
    let position = |name| names.iter().position(|n| *n == name).unwrap();

    assert!(position("group") < position("member"));
}
