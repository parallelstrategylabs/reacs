use eframe::egui::{Pos2, Vec2, pos2, vec2};
use reacs::tracking::{TrackedFrame, TrackingSave, UNKNOWN_SENDER};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

const OUTSIDE: u128 = 0;

pub const NODE_WIDTH: f32 = 240.0;
pub const HEADER_HEIGHT: f32 = 40.0;
pub const CAPTION_HEIGHT: f32 = 20.0;
pub const ROW_HEIGHT: f32 = 20.0;
pub const ROW_TITLE_WIDTH: f32 = 200.0;
pub const TICK_HEADER_HEIGHT: f32 = 40.0;
pub const GRID_STEP: f32 = 40.0;
const FIRST_COLUMN_X: f32 = 280.0;
const FIRST_ROW_TOP: f32 = 120.0;
const COLUMN_GAP: f32 = 80.0;
const COLUMN_STEP: f32 = NODE_WIDTH + COLUMN_GAP;
const ROW_GAP: f32 = 80.0;
const TICK_GAP: f32 = 40.0;
const TRACK_SPACING: f32 = 8.0;
const COLUMN_TRACKS: i32 = 8;
const LANE_TRACKS: i32 = 6;

pub struct Port {
    pub signal: u128,
    pub label: String,
    pub count: u64,
    pub external: bool,
}

pub struct Node {
    pub tick: u64,
    pub system: u128,
    pub full_name: String,
    pub row: usize,
    pub column: usize,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    pub position: Pos2,
}

impl Node {
    pub fn size(&self) -> Vec2 {
        let rows = self.inputs.len().max(self.outputs.len()).max(1) as f32;
        vec2(
            NODE_WIDTH,
            snap_up(HEADER_HEIGHT + CAPTION_HEIGHT + rows * ROW_HEIGHT),
        )
    }
}

pub fn input_anchor(top_left: Pos2, index: usize) -> Pos2 {
    top_left + vec2(0.0, row_center(index))
}

pub fn output_anchor(top_left: Pos2, index: usize) -> Pos2 {
    top_left + vec2(NODE_WIDTH, row_center(index))
}

pub fn row_center(index: usize) -> f32 {
    HEADER_HEIGHT + CAPTION_HEIGHT + ROW_HEIGHT * (index as f32 + 0.5)
}

pub struct Wire {
    pub signal: u128,
    pub label: String,
    pub windowed: bool,
    pub from_node: usize,
    pub from_port: usize,
    pub to_node: usize,
    pub to_port: usize,
    pub route: Vec<Pos2>,
    pub label_at: Pos2,
}

pub type NodeKey = (u64, u128);

pub struct Row {
    pub number: Option<usize>,
    pub name: String,
    pub top: f32,
    pub height: f32,
}

pub struct TickColumn {
    pub tick: u64,
    pub left: f32,
    pub right: f32,
    pub columns: usize,
    pub empty: bool,
}

#[derive(Default)]
pub struct TickGraph {
    pub rows: Vec<Row>,
    pub ticks: Vec<TickColumn>,
    pub nodes: Vec<Node>,
    pub wires: Vec<Wire>,
    pub extent: Vec2,
    pub focus: Option<usize>,
}

pub fn short_name(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

fn systems_in(frame: &TrackedFrame) -> BTreeSet<u128> {
    frame
        .signals
        .iter()
        .flat_map(|signal| {
            let senders = signal.sent.iter().map(|x| x.sender);
            let receivers = signal.received.iter().map(|x| x.receiver);
            senders.chain(receivers)
        })
        .collect()
}

pub fn sends_from_outside(frame: &TrackedFrame) -> bool {
    frame
        .signals
        .iter()
        .any(|signal| signal.sent.iter().any(|sent| sent.sender == OUTSIDE))
}

fn ordered_rows(save: &TrackingSave, has_outside: bool) -> Vec<(u128, Option<usize>)> {
    has_outside
        .then_some((OUTSIDE, None))
        .into_iter()
        .chain(
            save.system_order()
                .iter()
                .enumerate()
                .map(|(index, id)| (*id, Some(index + 1))),
        )
        .collect()
}

fn add_input(node: &mut Node, signal: u128, label: String, count: u64, external: bool) -> usize {
    let existing = node
        .inputs
        .iter()
        .position(|port| port.signal == signal && port.external == external && port.label == label);
    match existing {
        Some(index) => {
            node.inputs[index].count += count;
            index
        }
        None => {
            node.inputs.push(Port {
                signal,
                label,
                count,
                external,
            });
            node.inputs.len() - 1
        }
    }
}

fn outputs_of(save: &TrackingSave, frame: &TrackedFrame, system: u128) -> Vec<Port> {
    frame
        .signals
        .iter()
        .flat_map(|signal| {
            let name = short_name(save.signal_name(signal.signal)).to_string();
            signal
                .sent
                .iter()
                .filter(move |sent| sent.sender == system)
                .map(move |sent| Port {
                    signal: signal.signal,
                    label: name.clone(),
                    count: sent.count,
                    external: false,
                })
        })
        .collect()
}

fn add_inputs_and_wires(
    save: &TrackingSave,
    by_tick: &BTreeMap<u64, &TrackedFrame>,
    indices: &BTreeMap<(u64, u128), usize>,
    graph: &mut TickGraph,
) {
    for (tick, frame) in by_tick {
        for signal in &frame.signals {
            let name = short_name(save.signal_name(signal.signal)).to_string();
            for received in &signal.received {
                let source = received
                    .sent_tick
                    .filter(|_| received.sender != UNKNOWN_SENDER)
                    .and_then(|sent_tick| {
                        let from_node = *indices.get(&(sent_tick, received.sender))?;
                        let from_port = graph.nodes[from_node]
                            .outputs
                            .iter()
                            .position(|port| port.signal == signal.signal)?;
                        Some((from_node, from_port))
                    });

                let to_node = indices[&(*tick, received.receiver)];
                let node = &mut graph.nodes[to_node];
                if let Some((from_node, from_port)) = source {
                    let to_port =
                        add_input(node, signal.signal, name.clone(), received.count, false);
                    graph.wires.push(Wire {
                        signal: signal.signal,
                        label: format!("{name} x{}", received.count),
                        windowed: received.windowed,
                        from_node,
                        from_port,
                        to_node,
                        to_port,
                        route: Vec::new(),
                        label_at: Pos2::ZERO,
                    });
                    continue;
                }

                let sender = match received.sender {
                    UNKNOWN_SENDER => "unknown sender".to_string(),
                    OUTSIDE => "outside".to_string(),
                    id => short_name(save.system_name(id)).to_string(),
                };
                let origin = match (received.windowed, received.sent_tick) {
                    (true, Some(_)) => format!("earlier window, {sender}"),
                    (false, Some(sent_tick)) => format!("tick {sent_tick}, {sender}"),
                    (_, None) => format!("window, {sender}"),
                };
                let label = format!("{name} ({origin})");
                add_input(node, signal.signal, label, received.count, true);
            }
        }
    }
}

fn filter_ports(
    ports: Vec<Port>,
    kept: &mut Vec<Port>,
    keep: impl Fn(usize, &Port) -> bool,
) -> Vec<Option<usize>> {
    let mut map = Vec::with_capacity(ports.len());
    for (index, port) in ports.into_iter().enumerate() {
        if keep(index, &port) {
            map.push(Some(kept.len()));
            kept.push(port);
        } else {
            map.push(None);
        }
    }
    map
}

fn isolate(graph: &mut TickGraph, focus: usize) {
    let count = graph.nodes.len();
    let (mut causes, mut effects) = (vec![false; count], vec![false; count]);
    causes[focus] = true;
    effects[focus] = true;

    let mut by_target: Vec<usize> = (0..graph.wires.len()).collect();
    by_target.sort_by_key(|index| std::cmp::Reverse(graph.wires[*index].to_node));
    for index in by_target {
        let wire = &graph.wires[index];
        if wire.from_node != wire.to_node && causes[wire.to_node] {
            causes[wire.from_node] = true;
        }
    }
    let mut by_source: Vec<usize> = (0..graph.wires.len()).collect();
    by_source.sort_by_key(|index| graph.wires[*index].from_node);
    for index in by_source {
        let wire = &graph.wires[index];
        if wire.from_node != wire.to_node && effects[wire.from_node] {
            effects[wire.to_node] = true;
        }
    }

    graph.wires.retain(|wire| {
        (causes[wire.from_node] && causes[wire.to_node])
            || (effects[wire.from_node] && effects[wire.to_node])
    });

    let wires = &graph.wires;
    let mut input_maps = Vec::with_capacity(count);
    let mut output_maps = Vec::with_capacity(count);
    for (index, node) in graph.nodes.iter_mut().enumerate() {
        let inputs = std::mem::take(&mut node.inputs);
        input_maps.push(filter_ports(
            inputs,
            &mut node.inputs,
            |port_index, port| {
                let wired = wires
                    .iter()
                    .any(|wire| wire.to_node == index && wire.to_port == port_index);
                wired || (port.external && causes[index])
            },
        ));
        let outputs = std::mem::take(&mut node.outputs);
        output_maps.push(filter_ports(outputs, &mut node.outputs, |port_index, _| {
            let wired = wires
                .iter()
                .any(|wire| wire.from_node == index && wire.from_port == port_index);
            wired || index == focus
        }));
    }

    let mut new_index = vec![None; count];
    let (mut kept, mut position) = (0, 0);
    graph.nodes.retain(|_| {
        let keep = causes[position] || effects[position];
        if keep {
            new_index[position] = Some(kept);
            kept += 1;
        }
        position += 1;
        keep
    });
    for wire in &mut graph.wires {
        wire.from_port = output_maps[wire.from_node][wire.from_port].expect("the port has a wire");
        wire.to_port = input_maps[wire.to_node][wire.to_port].expect("the port has a wire");
        wire.from_node = new_index[wire.from_node].expect("the wire's node is kept");
        wire.to_node = new_index[wire.to_node].expect("the wire's node is kept");
    }
    graph.focus = new_index[focus];

    let used: BTreeSet<usize> = graph.nodes.iter().map(|node| node.row).collect();
    let row_map: BTreeMap<usize, usize> = used
        .iter()
        .enumerate()
        .map(|(new, old)| (*old, new))
        .collect();
    let mut row_index = 0;
    graph.rows.retain(|_| {
        let keep = used.contains(&row_index);
        row_index += 1;
        keep
    });
    for node in &mut graph.nodes {
        node.row = row_map[&node.row];
    }
}

fn assign_chain_columns(graph: &mut TickGraph) {
    let mut feeders: Vec<Vec<usize>> = vec![Vec::new(); graph.nodes.len()];
    for wire in &graph.wires {
        let same_tick = graph.nodes[wire.from_node].tick == graph.nodes[wire.to_node].tick;
        if same_tick && wire.from_node < wire.to_node {
            feeders[wire.to_node].push(wire.from_node);
        }
    }

    for index in 0..graph.nodes.len() {
        graph.nodes[index].column = feeders[index]
            .iter()
            .map(|feeder| graph.nodes[*feeder].column + 1)
            .max()
            .unwrap_or(0);
    }
}

fn snap_up(value: f32) -> f32 {
    (value / GRID_STEP).ceil() * GRID_STEP
}

fn place(graph: &mut TickGraph, ticks: Vec<u64>) -> BTreeMap<u64, usize> {
    let mut top = FIRST_ROW_TOP;
    for (index, row) in graph.rows.iter_mut().enumerate() {
        let tallest = graph
            .nodes
            .iter()
            .filter(|node| node.row == index)
            .map(|node| node.size().y)
            .fold(GRID_STEP * 2.0, f32::max);
        row.top = top;
        row.height = snap_up(tallest);
        top += row.height + ROW_GAP;
    }

    let mut left = FIRST_COLUMN_X;
    let mut first_columns = BTreeMap::new();
    let mut global_column = 0;
    for tick in ticks {
        let last = graph
            .nodes
            .iter()
            .filter(|node| node.tick == tick)
            .map(|node| node.column)
            .max();
        let columns = last.map_or(1, |last| last + 1);
        let right = left + columns as f32 * COLUMN_STEP - COLUMN_GAP;
        graph.ticks.push(TickColumn {
            tick,
            left,
            right,
            columns,
            empty: last.is_none(),
        });
        first_columns.insert(tick, global_column);
        global_column += columns;

        for node in graph.nodes.iter_mut().filter(|node| node.tick == tick) {
            node.position = pos2(
                left + node.column as f32 * COLUMN_STEP,
                graph.rows[node.row].top,
            );
        }
        left = right + COLUMN_GAP + TICK_GAP;
    }

    graph.extent = vec2(left, top);
    first_columns
}

fn next_track(used: &mut BTreeMap<i64, i32>, key: i64, tracks: i32) -> f32 {
    let count = used.entry(key).or_insert(0);
    let track = *count % tracks;
    *count += 1;
    (track - tracks / 2) as f32 * TRACK_SPACING
}

fn route_wires(graph: &mut TickGraph, first_columns: &BTreeMap<u64, usize>) {
    let mut gaps = BTreeMap::new();
    let mut lanes = BTreeMap::new();

    for wire in &mut graph.wires {
        let source = &graph.nodes[wire.from_node];
        let target = &graph.nodes[wire.to_node];
        let from = output_anchor(source.position, wire.from_port);
        let to = input_anchor(target.position, wire.to_port);
        let source_column = (first_columns[&source.tick] + source.column) as i64;
        let target_column = (first_columns[&target.tick] + target.column) as i64;

        if target_column == source_column + 1 {
            let middle = (source.position.x + NODE_WIDTH + target.position.x) / 2.0;
            let x = middle + next_track(&mut gaps, source_column, COLUMN_TRACKS);
            wire.route = vec![from, pos2(x, from.y), pos2(x, to.y), to];
            wire.label_at = pos2(x, (from.y + to.y) / 2.0);
            continue;
        }

        let leave = source.position.x
            + NODE_WIDTH
            + COLUMN_GAP / 2.0
            + next_track(&mut gaps, source_column, COLUMN_TRACKS);
        let arrive = target.position.x - COLUMN_GAP / 2.0
            + next_track(&mut gaps, target_column - 1, COLUMN_TRACKS);
        let lane = graph.rows[target.row].top - ROW_GAP / 2.0
            + next_track(&mut lanes, target.row as i64, LANE_TRACKS);
        wire.route = vec![
            from,
            pos2(leave, from.y),
            pos2(leave, lane),
            pos2(arrive, lane),
            pos2(arrive, to.y),
            to,
        ];
        wire.label_at = pos2((leave + arrive) / 2.0, lane);
    }
}

pub fn build(
    save: &TrackingSave,
    visible: &[&TrackedFrame],
    has_outside: bool,
    ticks: Range<u64>,
    focus: Option<NodeKey>,
) -> TickGraph {
    let by_tick: BTreeMap<u64, &TrackedFrame> =
        visible.iter().map(|frame| (frame.tick, *frame)).collect();

    let systems = ordered_rows(save, has_outside);
    let mut graph = TickGraph {
        rows: systems
            .iter()
            .map(|(id, number)| Row {
                number: *number,
                name: short_name(save.system_name(*id)).to_string(),
                top: 0.0,
                height: 0.0,
            })
            .collect(),
        ..Default::default()
    };

    let mut indices = BTreeMap::new();
    for (tick, frame) in &by_tick {
        let active = systems_in(frame);
        for (row, (system, _)) in systems.iter().enumerate() {
            if !active.contains(system) {
                continue;
            }
            indices.insert((*tick, *system), graph.nodes.len());
            graph.nodes.push(Node {
                tick: *tick,
                system: *system,
                full_name: save.system_name(*system).to_string(),
                row,
                column: 0,
                inputs: Vec::new(),
                outputs: outputs_of(save, frame, *system),
                position: Pos2::ZERO,
            });
        }
    }

    add_inputs_and_wires(save, &by_tick, &indices, &mut graph);
    let focused = focus.and_then(|key| indices.get(&key).copied());
    if let Some(focused) = focused {
        isolate(&mut graph, focused);
    }
    assign_chain_columns(&mut graph);

    let columns: Vec<u64> = if focused.is_some() {
        let kept: BTreeSet<u64> = graph.nodes.iter().map(|node| node.tick).collect();
        kept.into_iter().collect()
    } else {
        ticks.collect()
    };
    let first_columns = place(&mut graph, columns);
    route_wires(&mut graph, &first_columns);
    graph
}

#[cfg(test)]
mod tests {
    use super::{
        GRID_STEP, Node, NodeKey, Port, Row, TickGraph, Wire, build, isolate, sends_from_outside,
        short_name,
    };
    use crate::test_support::{recorded, recorded_with_outside_send};
    use eframe::egui::{Pos2, Rect};
    use reacs::tracking::TrackingSave;
    use std::ops::Range;

    fn graph_of(save: &TrackingSave, ticks: Range<u64>) -> TickGraph {
        focused(save, ticks, None)
    }

    fn focused(save: &TrackingSave, ticks: Range<u64>, focus: Option<NodeKey>) -> TickGraph {
        let has_outside = save.frames().any(sends_from_outside);
        let visible: Vec<_> = save.frames_in(ticks.clone()).collect();
        build(save, &visible, has_outside, ticks, focus)
    }

    fn key(save: &TrackingSave, tick: u64, name: &str) -> NodeKey {
        let graph = graph_of(save, tick..tick + 1);
        (tick, graph.nodes[node(&graph, tick, name)].system)
    }

    fn node(graph: &TickGraph, tick: u64, name: &str) -> usize {
        graph
            .nodes
            .iter()
            .position(|node| node.tick == tick && short_name(&node.full_name) == name)
            .unwrap_or_else(|| panic!("no {name} on tick {tick}"))
    }

    fn crosses(from: Pos2, to: Pos2, rect: Rect) -> bool {
        let segment = Rect::from_two_pos(from, to);
        segment.left() < rect.right()
            && segment.right() > rect.left()
            && segment.top() < rect.bottom()
            && segment.bottom() > rect.top()
    }

    pub fn assert_no_wire_behind_a_node(graph: &TickGraph) {
        for wire in &graph.wires {
            for segment in wire.route.windows(2) {
                assert!(
                    segment[0].x == segment[1].x || segment[0].y == segment[1].y,
                    "`{}` has a diagonal segment",
                    wire.label
                );
                for node in &graph.nodes {
                    let rect = Rect::from_min_size(node.position, node.size()).shrink(1.0);
                    assert!(
                        !crosses(segment[0], segment[1], rect),
                        "`{}` passes behind {} on tick {}",
                        wire.label,
                        node.full_name,
                        node.tick
                    );
                }
            }
        }
    }

    #[test]
    fn wires_join_senders_and_receivers_inside_and_across_ticks() {
        let save = recorded(4);
        let graph = graph_of(&save, 0..2);

        let pings: Vec<_> = graph
            .wires
            .iter()
            .filter(|x| x.label.starts_with("FixturePing"))
            .collect();
        assert_eq!(pings.len(), 2);
        let emit = node(&graph, 0, "fixture_emit");
        let late = node(&graph, 0, "fixture_late");
        let early = node(&graph, 1, "fixture_early");

        let inside = pings.iter().find(|x| x.to_node == late).unwrap();
        assert_eq!(inside.from_node, emit);
        assert_eq!(inside.label, "FixturePing x2");
        let across = pings.iter().find(|x| x.to_node == early).unwrap();
        assert_eq!(across.from_node, emit);
        assert_eq!(across.label, "FixturePing x2");
    }

    #[test]
    fn each_system_keeps_one_row_in_execution_order() {
        let save = recorded(4);
        let graph = graph_of(&save, 0..4);

        let row_of = |name: &str| graph.rows.iter().position(|row| row.name == name).unwrap();
        assert!(row_of("fixture_early") < row_of("fixture_emit"));
        assert!(row_of("fixture_emit") < row_of("fixture_late"));

        let first = node(&graph, 0, "fixture_emit");
        let later = node(&graph, 2, "fixture_emit");
        assert_eq!(graph.nodes[first].position.y, graph.nodes[later].position.y);
        assert_eq!(
            graph.nodes[first].position.y,
            graph.rows[graph.nodes[first].row].top
        );
    }

    #[test]
    fn a_system_with_nothing_this_tick_leaves_its_cell_empty() {
        let save = recorded(4);
        let graph = graph_of(&save, 0..2);

        assert!(
            !graph
                .nodes
                .iter()
                .any(|x| x.tick == 1 && short_name(&x.full_name) == "fixture_late")
        );
        assert!(graph.rows.iter().any(|row| row.name == "fixture_late"));
    }

    #[test]
    fn a_same_tick_chain_gets_one_column_per_hop() {
        let save = recorded(4);
        let graph = graph_of(&save, 0..2);

        assert_eq!(graph.nodes[node(&graph, 0, "fixture_emit")].column, 0);
        assert_eq!(graph.nodes[node(&graph, 0, "fixture_late")].column, 1);
        assert_eq!(graph.nodes[node(&graph, 0, "fixture_tail")].column, 2);
        assert_eq!(graph.ticks[0].columns, 3);
        assert_eq!(graph.ticks[1].columns, 1);
    }

    #[test]
    fn a_signal_read_next_tick_does_not_add_a_column() {
        let save = recorded(4);
        let graph = graph_of(&save, 0..2);
        assert_eq!(graph.nodes[node(&graph, 1, "fixture_early")].column, 0);
    }

    #[test]
    fn every_node_edge_sits_on_the_grid() {
        let save = recorded(4);
        let graph = graph_of(&save, 0..4);
        let on_grid = |value: f32| (value / GRID_STEP).fract() == 0.0;

        assert!(!graph.nodes.is_empty());
        for node in &graph.nodes {
            let far = node.position + node.size();
            assert!(on_grid(node.position.x) && on_grid(node.position.y));
            assert!(on_grid(far.x) && on_grid(far.y));
        }
    }

    #[test]
    fn every_system_has_a_row_numbered_like_the_system_graph() {
        let save = recorded(4);
        let graph = graph_of(&save, 1..2);

        assert_eq!(graph.rows.len(), save.system_order().len());
        for (index, (row, id)) in graph.rows.iter().zip(save.system_order()).enumerate() {
            assert_eq!(row.number, Some(index + 1));
            assert_eq!(row.name, short_name(save.system_name(*id)));
        }
        assert_eq!(graph.nodes.len(), 1);
    }

    #[test]
    fn isolating_keeps_only_causes_and_effects() {
        let save = recorded(4);
        let late = key(&save, 0, "fixture_late");
        let graph = focused(&save, 0..3, Some(late));

        let names: Vec<&str> = graph.rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["fixture_emit", "fixture_late", "fixture_tail"]);
        assert_eq!(graph.ticks.len(), 1);
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.nodes[graph.focus.unwrap()].system, late.1);
        assert_eq!(graph.nodes[node(&graph, 0, "fixture_tail")].column, 2);
    }

    #[test]
    fn isolating_inlines_rows_and_drops_unrelated_ports() {
        let save = recorded(4);
        let early = key(&save, 1, "fixture_early");
        let graph = focused(&save, 0..3, Some(early));

        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.rows.len(), 2);
        assert_eq!(graph.wires.len(), 1);
        let emit = &graph.nodes[node(&graph, 0, "fixture_emit")];
        assert_eq!(emit.outputs.len(), 1);
        assert_eq!(emit.outputs[0].label, "FixturePing");
        assert_eq!(graph.wires[0].from_port, 0);
        assert_no_wire_behind_a_node(&graph);
    }

    #[test]
    fn isolating_keeps_the_focus_nodes_unread_outputs() {
        let save = recorded(4);
        let emit = key(&save, 0, "fixture_emit");
        let graph = focused(&save, 0..1, Some(emit));

        let focus = &graph.nodes[graph.focus.unwrap()];
        assert!(focus.outputs.iter().any(|port| port.label == "FixtureBeam"));
    }

    #[test]
    fn isolating_a_node_that_is_not_on_screen_shows_everything() {
        let save = recorded(4);
        let late = key(&save, 0, "fixture_late");
        let graph = focused(&save, 2..3, Some(late));

        assert!(graph.focus.is_none());
        assert_eq!(graph.rows.len(), save.system_order().len());
    }

    #[test]
    fn a_wire_that_skips_the_focus_is_not_part_of_its_chain() {
        let port = |signal: u128| Port {
            signal,
            label: signal.to_string(),
            count: 1,
            external: false,
        };
        let plain = |system: u128, inputs, outputs| Node {
            tick: 0,
            system,
            full_name: system.to_string(),
            row: system as usize,
            column: 0,
            inputs,
            outputs,
            position: Pos2::ZERO,
        };
        let wire = |signal, from_node, to_node, to_port| Wire {
            signal,
            label: String::new(),
            windowed: false,
            from_node,
            from_port: 0,
            to_node,
            to_port,
            route: Vec::new(),
            label_at: Pos2::ZERO,
        };
        let row = || Row {
            number: None,
            name: String::new(),
            top: 0.0,
            height: 0.0,
        };

        let mut graph = TickGraph {
            rows: vec![row(), row(), row()],
            nodes: vec![
                plain(0, vec![], vec![port(1)]),
                plain(1, vec![port(1)], vec![port(1)]),
                plain(2, vec![port(1), port(2)], vec![]),
            ],
            wires: vec![wire(1, 0, 1, 0), wire(1, 1, 2, 0), wire(2, 0, 2, 1)],
            ..Default::default()
        };

        isolate(&mut graph, 1);
        assert_eq!(graph.wires.len(), 2);
        assert_eq!(graph.nodes[2].inputs.len(), 1);
    }

    #[test]
    fn ticks_never_overlap_and_later_ticks_are_to_the_right() {
        let save = recorded(4);
        let graph = graph_of(&save, 0..4);
        for pair in graph.ticks.windows(2) {
            assert!(pair[0].right < pair[1].left);
        }
        for node in &graph.nodes {
            let tick = graph.ticks.iter().find(|x| x.tick == node.tick).unwrap();
            assert!(node.position.x >= tick.left);
            assert!(node.position.x + node.size().x <= tick.right);
        }
    }

    #[test]
    fn no_wire_passes_behind_a_node() {
        let save = recorded(4);
        for ticks in [0..2, 0..3, 0..4, 1..4] {
            let graph = graph_of(&save, ticks);
            assert!(!graph.wires.is_empty());
            assert_no_wire_behind_a_node(&graph);
        }
    }

    #[test]
    fn a_system_reading_its_own_signal_routes_around_itself() {
        let save = recorded(4);
        let graph = graph_of(&save, 0..1);
        let tail = node(&graph, 0, "fixture_tail");
        let own = graph
            .wires
            .iter()
            .find(|x| x.from_node == tail && x.to_node == tail)
            .unwrap();
        assert!(own.route.len() > 4);
        assert_eq!(graph.nodes[tail].column, 2);
    }

    #[test]
    fn a_sender_off_screen_becomes_a_labeled_input() {
        let save = recorded(4);
        let graph = graph_of(&save, 1..2);

        assert!(graph.wires.is_empty());
        let early = node(&graph, 1, "fixture_early");
        let input = &graph.nodes[early].inputs[0];
        assert!(input.external);
        assert_eq!(input.label, "FixturePing (tick 0, fixture_emit)");
    }

    #[test]
    fn windows_on_screen_get_a_wire_across_ticks() {
        let save = recorded(4);
        let graph = graph_of(&save, 0..3);

        let emit = node(&graph, 0, "fixture_emit");
        let reader = node(&graph, 2, "fixture_beam_reader");
        let wire = graph.wires.iter().find(|x| x.to_node == reader).unwrap();
        assert!(wire.windowed);
        assert_eq!(wire.from_node, emit);
        assert_eq!(wire.label, "FixtureBeam x1");
    }

    #[test]
    fn windows_off_screen_become_a_labeled_input_with_their_sender() {
        let save = recorded(4);
        let graph = graph_of(&save, 2..3);

        let reader = node(&graph, 2, "fixture_beam_reader");
        let input = &graph.nodes[reader].inputs[0];
        assert!(input.external);
        assert_eq!(input.label, "FixtureBeam (earlier window, fixture_emit)");
        assert_eq!(input.count, 1);
    }

    #[test]
    fn a_tick_with_no_signals_is_an_empty_column() {
        let mut save = recorded(4);
        save.push_frame(4, None);
        let graph = graph_of(&save, 4..5);

        assert!(graph.nodes.is_empty());
        assert!(graph.ticks[0].empty);
        assert_eq!(graph.ticks[0].columns, 1);
    }

    #[test]
    fn signals_from_outside_get_their_own_row_first() {
        let save = recorded_with_outside_send();
        let graph = graph_of(&save, 0..1);

        assert_eq!(graph.rows[0].name, "outside");
        let outside = node(&graph, 0, "outside");
        assert_eq!(graph.nodes[outside].outputs[0].count, 1);
    }
}
