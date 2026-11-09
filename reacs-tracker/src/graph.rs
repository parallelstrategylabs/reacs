use crate::palette::Palette;
use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};
use reacs::tracking::{
    TrackedFrame, TrackedInstance, TrackedSignalFrame, TrackingSave, UNKNOWN_SENDER,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

const OUTSIDE: u128 = 0;

pub const NODE_WIDTH: f32 = 240.0;
pub const STUB_RISE: f32 = 120.0;
pub const PORT_TEXT_SIZE: f32 = 13.0;
pub const TITLE_TEXT_SIZE: f32 = 14.0;
pub const PORT_MARGIN: f32 = 14.0;
pub const TITLE_MARGIN: f32 = 8.0;
const CAPTION_WIDTH: f32 = 50.0;
pub const HEADER_HEIGHT: f32 = 40.0;
pub const CAPTION_HEIGHT: f32 = 40.0;
pub const ROW_HEIGHT: f32 = 40.0;
pub const TICK_HEADER_HEIGHT: f32 = 40.0;
pub const GRID_STEP: f32 = 40.0;
const FIRST_COLUMN_X: f32 = 120.0;
const FIRST_ROW_TOP: f32 = 120.0;
pub const BLOCK_PADDING: f32 = GRID_STEP;
const BLOCK_TOP: f32 = FIRST_ROW_TOP - BLOCK_PADDING - TICK_HEADER_HEIGHT;
const TICK_GAP: f32 = 80.0;
const LANE_CELLS: usize = 5;
const LABEL_PADDING: f32 = 6.0;
const LABEL_TOP_ROOM: f32 = 24.0;
const LABEL_BOTTOM_ROOM: f32 = 8.0;
const PORT_TRACK_STEP: f32 = GRID_STEP / 4.0;

pub struct Port {
    pub signal: u128,
    pub label: String,
    pub count: u64,
    pub cancelled: u64,
    pub modified: u64,
    pub external: bool,
    pub wired: bool,
    pub stub_x: f32,
    pub faded: bool,
    pub notes: Vec<String>,
}

impl Port {
    pub fn text(&self) -> String {
        counted(&self.label, self.count, self.cancelled, self.modified)
    }
}

pub fn counted(label: &str, count: u64, cancelled: u64, modified: u64) -> String {
    let mut parts = Vec::new();
    if count > 0 || (cancelled == 0 && modified == 0) {
        parts.push(count.to_string());
    }
    if cancelled > 0 {
        parts.push(format!("cancel {cancelled}"));
    }
    if modified > 0 {
        parts.push(format!("modify {modified}"));
    }
    format!("{label} ({})", parts.join(", "))
}

pub type Measure<'a> = &'a dyn Fn(&str, f32) -> f32;

pub struct Node {
    pub tick: u64,
    pub system: u128,
    pub full_name: String,
    pub column: usize,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    pub position: Pos2,
    pub width: f32,
    pub divider: f32,
    pub faded: bool,
}

impl Node {
    pub fn size(&self) -> Vec2 {
        let rows = self.inputs.len().max(self.outputs.len()).max(1) as f32;
        vec2(
            self.width,
            snap_up(HEADER_HEIGHT + CAPTION_HEIGHT + rows * ROW_HEIGHT),
        )
    }
}

pub fn input_anchor(node: &Node, index: usize) -> Pos2 {
    node.position + vec2(0.0, row_center(index))
}

pub fn output_anchor(node: &Node, index: usize) -> Pos2 {
    node.position + vec2(node.width, row_center(index))
}

pub fn row_center(index: usize) -> f32 {
    HEADER_HEIGHT + CAPTION_HEIGHT + ROW_HEIGHT * index as f32
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
    pub faded: bool,
    pub focused: bool,
    pub details: Vec<String>,
}

pub type NodeKey = (u64, u128);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WireKey {
    pub from: NodeKey,
    pub to: NodeKey,
    pub signal: u128,
    pub windowed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct StubKey {
    pub node: NodeKey,
    pub signal: u128,
    pub input: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Hit {
    Node(NodeKey),
    Wire(WireKey),
    Stub(StubKey),
}

pub struct StubHit {
    pub node: usize,
    pub port: usize,
    pub input: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Focus {
    Node(NodeKey),
    Wire(WireKey),
    Tick(u64),
}

impl Focus {
    pub fn ticks(&self) -> (u64, u64) {
        match self {
            Focus::Node((tick, _)) => (*tick, *tick),
            Focus::Wire(key) => (key.from.0, key.to.0),
            Focus::Tick(tick) => (*tick, *tick),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    Node(usize),
    Wire(usize),
    Tick(u64),
}

pub struct TickColumn {
    pub tick: u64,
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
    pub empty: bool,
}

#[derive(Default)]
pub struct TickGraph {
    pub ticks: Vec<TickColumn>,
    pub nodes: Vec<Node>,
    pub wires: Vec<Wire>,
    pub extent: Vec2,
    pub first_tick_line: f32,
    pub tick_line_gap: f32,
    pub lowest_lane: f32,
    pub palette: Palette,
    pub focused_nodes: Vec<usize>,
    pub focused_ticks: Vec<u64>,
}

impl TickGraph {
    pub fn stub(&self, hit: &StubHit) -> (&Port, Pos2) {
        let node = &self.nodes[hit.node];
        if hit.input {
            (&node.inputs[hit.port], input_anchor(node, hit.port))
        } else {
            (&node.outputs[hit.port], output_anchor(node, hit.port))
        }
    }

    pub fn stub_key(&self, hit: &StubHit) -> StubKey {
        let node = &self.nodes[hit.node];
        StubKey {
            node: (node.tick, node.system),
            signal: self.stub(hit).0.signal,
            input: hit.input,
        }
    }

    pub fn wire_key(&self, wire: &Wire) -> WireKey {
        let key = |index: usize| (self.nodes[index].tick, self.nodes[index].system);
        WireKey {
            from: key(wire.from_node),
            to: key(wire.to_node),
            signal: wire.signal,
            windowed: wire.windowed,
        }
    }
}

pub fn short_name(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

fn closest_on_segment(point: Pos2, from: Pos2, to: Pos2) -> Pos2 {
    let along = to - from;
    let t = ((point - from).dot(along) / along.length_sq().max(f32::EPSILON)).clamp(0.0, 1.0);
    from + along * t
}

fn distance_to_segment(point: Pos2, from: Pos2, to: Pos2) -> f32 {
    closest_on_segment(point, from, to).distance(point)
}

pub fn label_position(
    path: &[Pos2],
    natural: Pos2,
    half_width: f32,
    visible: Rect,
) -> Option<Pos2> {
    let mut runs: Vec<(f32, f32, f32)> = Vec::new();
    for segment in path.windows(2) {
        if segment[0].y != segment[1].y {
            continue;
        }
        let y = segment[0].y;
        let left = segment[0].x.min(segment[1].x);
        let right = segment[0].x.max(segment[1].x);
        match runs.last_mut() {
            Some(run) if run.0 == y && run.2 == left => run.2 = right,
            _ => runs.push((y, left, right)),
        }
    }

    let reach = half_width + LABEL_PADDING;
    let lowest = visible.bottom() - LABEL_BOTTOM_ROOM;
    runs.into_iter()
        .filter(|(y, _, _)| *y > visible.top() + LABEL_TOP_ROOM && *y < lowest)
        .filter_map(|(y, left, right)| {
            let low = (left + reach).max(visible.left() + reach);
            let high = (right - reach).min(visible.right() - reach);
            (low <= high).then(|| pos2(natural.x.clamp(low, high), y))
        })
        .min_by(|a, b| a.distance(natural).total_cmp(&b.distance(natural)))
}

fn on_a_node(graph: &TickGraph, point: Pos2) -> bool {
    graph
        .nodes
        .iter()
        .any(|node| Rect::from_min_size(node.position, node.size()).contains(point))
}

pub fn stub_at(graph: &TickGraph, point: Pos2, reach: f32) -> Option<StubHit> {
    if on_a_node(graph, point) {
        return None;
    }

    let mut best: Option<(f32, StubHit)> = None;
    for (node_index, node) in graph.nodes.iter().enumerate() {
        for (input, ports) in [(true, &node.inputs), (false, &node.outputs)] {
            for (index, port) in ports.iter().enumerate().filter(|(_, x)| x.external) {
                let anchor = if input {
                    input_anchor(node, index)
                } else {
                    output_anchor(node, index)
                };
                let corner = pos2(port.stub_x, anchor.y);
                let top = corner - vec2(0.0, STUB_RISE);
                let distance = distance_to_segment(point, anchor, corner)
                    .min(distance_to_segment(point, corner, top));
                if distance <= reach && best.as_ref().is_none_or(|(x, _)| distance < *x) {
                    let hit = StubHit {
                        node: node_index,
                        port: index,
                        input,
                    };
                    best = Some((distance, hit));
                }
            }
        }
    }
    best.map(|(_, hit)| hit)
}

pub fn wire_at(graph: &TickGraph, point: Pos2, reach: f32) -> Option<usize> {
    if on_a_node(graph, point) {
        return None;
    }

    graph
        .wires
        .iter()
        .enumerate()
        .filter_map(|(index, wire)| {
            let distance = wire
                .route
                .windows(2)
                .map(|segment| distance_to_segment(point, segment[0], segment[1]))
                .fold(f32::MAX, f32::min);
            (distance <= reach).then_some((wire.faded, distance, index))
        })
        .min_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)))
        .map(|(_, _, index)| index)
}

pub fn age_text(ticks: u64) -> Option<String> {
    match ticks {
        0 => None,
        1 => Some("1 tick old".to_string()),
        ticks => Some(format!("{ticks} ticks old")),
    }
}

pub fn sender_name(save: &TrackingSave, id: u128) -> String {
    match id {
        UNKNOWN_SENDER => "unknown sender".to_string(),
        OUTSIDE => "outside".to_string(),
        id => short_name(save.system_name(id)).to_string(),
    }
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

fn ordered_systems(save: &TrackingSave, has_outside: bool) -> Vec<u128> {
    has_outside
        .then_some(OUTSIDE)
        .into_iter()
        .chain(save.system_order().iter().copied())
        .collect()
}

fn add_input(
    node: &mut Node,
    signal: u128,
    label: &str,
    count: u64,
    off_screen: Option<String>,
) -> usize {
    let index = node
        .inputs
        .iter()
        .position(|port| port.signal == signal)
        .unwrap_or_else(|| {
            node.inputs.push(Port {
                signal,
                label: label.to_string(),
                count: 0,
                cancelled: 0,
                modified: 0,
                external: false,
                wired: false,
                stub_x: 0.0,
                faded: false,
                notes: Vec::new(),
            });
            node.inputs.len() - 1
        });

    let port = &mut node.inputs[index];
    port.count += count;
    port.wired |= off_screen.is_none();
    if let Some(note) = off_screen {
        port.external = true;
        port.notes.push(note);
    }
    index
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
                    cancelled: 0,
                    modified: 0,
                    external: false,
                    wired: false,
                    stub_x: 0.0,
                    faded: false,
                    notes: Vec::new(),
                })
        })
        .collect()
}

fn instance_line(instance: &TrackedInstance, sent_tick: u64) -> String {
    let window = match instance.window {
        None => String::new(),
        Some((start, u64::MAX)) if start <= sent_tick => ", open-ended".to_string(),
        Some((start, u64::MAX)) => format!(", from tick {start}, open-ended"),
        Some((start, end)) => format!(", ticks {start}..{end}"),
    };
    format!("#{} {}{window}", instance.id, instance.payload)
}

fn sent_lines(
    signal: &TrackedSignalFrame,
    sender: u128,
    sent_tick: u64,
    windows_only: bool,
) -> Vec<String> {
    let mut lines: Vec<String> = signal
        .instances
        .iter()
        .filter(|x| x.sender == sender && (!windows_only || x.window.is_some()))
        .map(|instance| instance_line(instance, sent_tick))
        .collect();
    if signal.instances_omitted > 0 {
        lines.push(format!(
            "{} more on this tick not recorded",
            signal.instances_omitted
        ));
    }
    lines
}

fn change_lines(signal: &TrackedSignalFrame, system: u128, sender: u128) -> Vec<String> {
    signal
        .changes
        .iter()
        .filter(|x| x.system == system && x.sender == sender)
        .map(|x| match &x.payload {
            Some(payload) => format!("#{} changed to {payload}", x.id),
            None => format!("#{} cancelled", x.id),
        })
        .collect()
}

fn signal_sent_at<'a>(
    save: &'a TrackingSave,
    by_tick: &BTreeMap<u64, &'a TrackedFrame>,
    tick: u64,
    signal: u128,
) -> Option<&'a TrackedSignalFrame> {
    let frame = by_tick
        .get(&tick)
        .copied()
        .or_else(|| save.frames_in(tick..tick + 1).next())?;
    frame.signals.iter().find(|x| x.signal == signal)
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

                let mut details = received
                    .sent_tick
                    .and_then(|sent| {
                        Some((sent, signal_sent_at(save, by_tick, sent, signal.signal)?))
                    })
                    .map_or_else(Vec::new, |(tick, sent)| {
                        sent_lines(sent, received.sender, tick, received.windowed)
                    });
                details.extend(change_lines(signal, received.receiver, received.sender));

                let to_node = indices[&(*tick, received.receiver)];
                let node = &mut graph.nodes[to_node];
                if let Some((from_node, from_port)) = source {
                    let to_port = add_input(node, signal.signal, &name, received.count, None);
                    node.inputs[to_port].cancelled += received.cancelled;
                    node.inputs[to_port].modified += received.modified;
                    graph.nodes[from_node].outputs[from_port].wired = true;
                    graph.wires.push(Wire {
                        signal: signal.signal,
                        label: counted(
                            &name,
                            received.count,
                            received.cancelled,
                            received.modified,
                        ),
                        windowed: received.windowed,
                        from_node,
                        from_port,
                        to_node,
                        to_port,
                        route: Vec::new(),
                        label_at: Pos2::ZERO,
                        faded: false,
                        focused: false,
                        details,
                    });
                    continue;
                }

                let sender = sender_name(save, received.sender);
                let note = match received.sent_tick {
                    Some(sent_tick) => format!("from {sender}, tick {sent_tick}"),
                    None => format!("from {sender}, sent before recording"),
                };
                let port = add_input(node, signal.signal, &name, received.count, Some(note));
                node.inputs[port].cancelled += received.cancelled;
                node.inputs[port].modified += received.modified;
                node.inputs[port].notes.extend(details);
            }
        }
    }
}

struct Chain {
    causes: Vec<bool>,
    effects: Vec<bool>,
}

impl Chain {
    fn of(graph: &TickGraph, target: Target) -> Self {
        let count = graph.nodes.len();
        let (mut causes, mut effects) = (vec![false; count], vec![false; count]);
        for (index, node) in graph.nodes.iter().enumerate() {
            let (cause, effect) = match target {
                Target::Node(focus) => (index == focus, index == focus),
                Target::Wire(wire) => (
                    index == graph.wires[wire].from_node,
                    index == graph.wires[wire].to_node,
                ),
                Target::Tick(tick) => (node.tick == tick, node.tick == tick),
            };
            causes[index] = cause;
            effects[index] = effect;
        }

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

        Self { causes, effects }
    }

    fn keeps_node(&self, index: usize) -> bool {
        self.causes[index] || self.effects[index]
    }

    fn keeps_wire(&self, wire: &Wire) -> bool {
        wire.focused
            || (self.causes[wire.from_node] && self.causes[wire.to_node])
            || (self.effects[wire.from_node] && self.effects[wire.to_node])
    }
}

fn mark_target(graph: &mut TickGraph, target: Target) {
    match target {
        Target::Node(node) => graph.focused_nodes.push(node),
        Target::Wire(wire) => graph.wires[wire].focused = true,
        Target::Tick(tick) => graph.focused_ticks.push(tick),
    }
}

fn fade(graph: &mut TickGraph, targets: &[Target]) {
    for target in targets {
        mark_target(graph, *target);
    }
    let chains: Vec<Chain> = targets
        .iter()
        .map(|target| Chain::of(graph, *target))
        .collect();
    for (index, node) in graph.nodes.iter_mut().enumerate() {
        node.faded = !chains.iter().any(|chain| chain.keeps_node(index));
    }
    for wire in &mut graph.wires {
        wire.faded = !chains.iter().any(|chain| chain.keeps_wire(wire));
    }
    fade_ports(graph);
}

fn fade_ports(graph: &mut TickGraph) {
    let (mut wired, mut kept) = (BTreeSet::new(), BTreeSet::new());
    for wire in &graph.wires {
        for end in [
            (wire.from_node, false, wire.from_port),
            (wire.to_node, true, wire.to_port),
        ] {
            wired.insert(end);
            if !wire.faded {
                kept.insert(end);
            }
        }
    }
    for (index, node) in graph.nodes.iter_mut().enumerate() {
        let faded = node.faded;
        let mark = |input: bool, ports: &mut [Port]| {
            for (position, port) in ports.iter_mut().enumerate() {
                let end = (index, input, position);
                let lost_its_edges = wired.contains(&end) && !kept.contains(&end);
                port.faded = faded || (lost_its_edges && !port.external);
            }
        };
        mark(true, &mut node.inputs);
        mark(false, &mut node.outputs);
    }
}

fn mark_reads_after_the_view(
    save: &TrackingSave,
    later: &[&TrackedFrame],
    indices: &BTreeMap<(u64, u128), usize>,
    graph: &mut TickGraph,
) {
    for frame in later {
        for signal in &frame.signals {
            for received in &signal.received {
                let source = received
                    .sent_tick
                    .and_then(|sent_tick| indices.get(&(sent_tick, received.sender)));
                let Some(node) = source else {
                    continue;
                };
                let outputs = &mut graph.nodes[*node].outputs;
                if let Some(port) = outputs.iter_mut().find(|x| x.signal == signal.signal) {
                    port.external = true;
                    if !port.notes.iter().any(|note| note.starts_with('#')) {
                        let tick = received.sent_tick.unwrap_or(frame.tick);
                        let sent = save.frames_in(tick..tick + 1).next().and_then(|sent| {
                            sent.signals.iter().find(|x| x.signal == signal.signal)
                        });
                        if let Some(sent) = sent {
                            port.notes
                                .extend(sent_lines(sent, received.sender, tick, false));
                        }
                    }
                    let reader = format!("to {},", sender_name(save, received.receiver));
                    if !port.notes.iter().any(|note| note.starts_with(&reader)) {
                        port.notes.push(format!("{reader} tick {}", frame.tick));
                    }
                }
            }
        }
    }
}

fn fit_nodes_to_text(graph: &mut TickGraph, measure: Measure) {
    for node in &mut graph.nodes {
        let widest = |ports: &[Port]| {
            ports
                .iter()
                .map(|port| measure(&port.text(), PORT_TEXT_SIZE))
                .fold(CAPTION_WIDTH, f32::max)
        };
        let (inputs, outputs) = (widest(&node.inputs), widest(&node.outputs));
        let title = measure(short_name(&node.full_name), TITLE_TEXT_SIZE) + TITLE_MARGIN * 2.0;
        let columns = inputs + outputs + PORT_MARGIN * 4.0;

        node.width = snap_up(columns.max(title)).max(NODE_WIDTH);
        let first = inputs + PORT_MARGIN * 2.0;
        let last = node.width - outputs - PORT_MARGIN * 2.0;
        node.divider = (node.width / 2.0).clamp(first, last);
    }
}

fn snap_up(value: f32) -> f32 {
    (value / GRID_STEP).ceil() * GRID_STEP
}

struct OneRowLayout {
    first_columns: BTreeMap<u64, usize>,
    centers: Vec<f32>,
    leaving: Vec<BTreeSet<usize>>,
    entering: Vec<BTreeSet<usize>>,
    block_bottom: f32,
}

fn half_gap(tracks: usize) -> f32 {
    snap_up((tracks + 1) as f32 * PORT_TRACK_STEP).max(GRID_STEP)
}

fn track_offset(ports: &BTreeSet<usize>, port: usize) -> f32 {
    (ports.range(..port).count() + 1) as f32 * PORT_TRACK_STEP
}

fn place_in_one_row(graph: &mut TickGraph, ticks: Range<u64>) -> OneRowLayout {
    let mut first_columns = BTreeMap::new();
    let mut spans = Vec::new();
    let mut node_at: Vec<Option<usize>> = Vec::new();
    for tick in ticks {
        let first = node_at.len();
        for (index, node) in graph.nodes.iter_mut().enumerate() {
            if node.tick == tick {
                node.column = node_at.len() - first;
                node_at.push(Some(index));
            }
        }
        let empty = node_at.len() == first;
        if empty {
            node_at.push(None);
        }
        first_columns.insert(tick, first);
        spans.push((tick, first, node_at.len() - first, empty));
    }

    let count = graph.nodes.len();
    let (mut leaving, mut entering) = (vec![BTreeSet::new(); count], vec![BTreeSet::new(); count]);
    for wire in &graph.wires {
        let column = |index: usize| {
            let node = &graph.nodes[index];
            first_columns[&node.tick] + node.column
        };
        if column(wire.to_node) != column(wire.from_node) + 1 {
            leaving[wire.from_node].insert(wire.from_port);
            entering[wire.to_node].insert(wire.to_port);
        }
    }
    let tracks = |lanes: usize, ports: &[Port]| {
        let stubs = ports.iter().filter(|port| port.external).count();
        lanes + stubs + usize::from(stubs > 0)
    };

    let halves: Vec<f32> = (0..=node_at.len())
        .map(|gap| {
            let before = gap.checked_sub(1).and_then(|column| node_at[column]);
            let after = node_at.get(gap).copied().flatten();
            let out = before.map_or(0, |node| {
                tracks(leaving[node].len(), &graph.nodes[node].outputs)
            });
            let into = after.map_or(0, |node| {
                tracks(entering[node].len(), &graph.nodes[node].inputs)
            });
            half_gap(out.max(into))
        })
        .collect();

    let mut left = FIRST_COLUMN_X + halves[0] - GRID_STEP;
    let mut centers = vec![left - halves[0] - TICK_GAP / 2.0; halves.len()];
    for (tick, first, columns, empty) in spans {
        let tick_left = left;
        let mut right = left;
        for column in first..first + columns {
            let mut width = NODE_WIDTH;
            if let Some(node) = node_at[column] {
                graph.nodes[node].position = pos2(left, FIRST_ROW_TOP);
                width = graph.nodes[node].width;
            }
            right = left + width;
            let half = halves[column + 1];
            let between_ticks = if column + 1 == first + columns {
                TICK_GAP / 2.0
            } else {
                0.0
            };
            centers[column + 1] = right + half + between_ticks;
            left = right + (half + between_ticks) * 2.0;
        }
        graph.ticks.push(TickColumn {
            tick,
            left: tick_left,
            right,
            top: BLOCK_TOP,
            bottom: 0.0,
            empty,
        });
    }

    let tallest = graph
        .nodes
        .iter()
        .map(|node| node.position.y + node.size().y)
        .fold(FIRST_ROW_TOP, f32::max);
    let block_bottom = tallest + BLOCK_PADDING;
    for tick in &mut graph.ticks {
        tick.bottom = block_bottom;
    }

    graph.extent.x = left;
    OneRowLayout {
        first_columns,
        centers,
        leaving,
        entering,
        block_bottom,
    }
}

struct LaneWire {
    wire: usize,
    age: u64,
    from: Pos2,
    to: Pos2,
    leave: f32,
    arrive: f32,
    lane: usize,
}

impl LaneWire {
    fn left(&self) -> f32 {
        self.leave.min(self.arrive)
    }

    fn right(&self) -> f32 {
        self.leave.max(self.arrive)
    }
}

fn assign_lanes(waiting: &mut [LaneWire]) -> usize {
    let mut most = 0;
    let mut lane_ends: Vec<f32> = Vec::new();
    let mut age = None;
    for entry in waiting.iter_mut() {
        if age != Some(entry.age) {
            age = Some(entry.age);
            lane_ends.clear();
        }
        entry.lane = lane_ends
            .iter()
            .position(|end| *end < entry.left())
            .unwrap_or_else(|| {
                lane_ends.push(f32::MIN);
                lane_ends.len() - 1
            });
        lane_ends[entry.lane] = entry.right();
        most = most.max(lane_ends.len());
    }
    most
}

fn lane_cells(busiest_age: usize) -> usize {
    (busiest_age + 1).div_ceil(LANE_CELLS) * LANE_CELLS
}

fn stub_distance(ports: &[Port], position: usize) -> f32 {
    let nearer = ports[..position]
        .iter()
        .filter(|port| port.external)
        .count();
    (nearer + 2) as f32 * PORT_TRACK_STEP
}

fn place_stubs(graph: &mut TickGraph) {
    for node in &mut graph.nodes {
        let left = node.position.x;
        let right = left + node.width;
        let inputs: Vec<f32> = (0..node.inputs.len())
            .map(|position| left - stub_distance(&node.inputs, position))
            .collect();
        for (port, x) in node.inputs.iter_mut().zip(inputs) {
            port.stub_x = x;
        }
        let outputs: Vec<f32> = (0..node.outputs.len())
            .map(|position| right + stub_distance(&node.outputs, position))
            .collect();
        for (port, x) in node.outputs.iter_mut().zip(outputs) {
            port.stub_x = x;
        }
    }
}

fn route_through_lanes(graph: &mut TickGraph, layout: &OneRowLayout) {
    place_stubs(graph);
    let mut waiting = Vec::new();
    for (index, wire) in graph.wires.iter_mut().enumerate() {
        let source = &graph.nodes[wire.from_node];
        let target = &graph.nodes[wire.to_node];
        let from = output_anchor(source, wire.from_port);
        let to = input_anchor(target, wire.to_port);
        let source_column = layout.first_columns[&source.tick] + source.column;
        let target_column = layout.first_columns[&target.tick] + target.column;

        if target_column == source_column + 1 {
            let x = layout.centers[target_column];
            wire.route = vec![from, pos2(x, from.y), pos2(x, to.y), to];
            wire.label_at = pos2(x, (from.y + to.y) / 2.0);
            continue;
        }

        let leaving = &layout.leaving[wire.from_node];
        let entering = &layout.entering[wire.to_node];
        waiting.push(LaneWire {
            wire: index,
            age: target.tick - source.tick,
            from,
            to,
            leave: layout.centers[source_column + 1] - track_offset(leaving, wire.from_port),
            arrive: layout.centers[target_column] + track_offset(entering, wire.to_port),
            lane: 0,
        });
    }

    waiting.sort_by(|a, b| a.age.cmp(&b.age).then(a.left().total_cmp(&b.left())));
    let cells = lane_cells(assign_lanes(&mut waiting));
    let first_lane = layout.block_bottom + GRID_STEP;
    let slot = cells as f32 * GRID_STEP;
    graph.first_tick_line = first_lane + slot - GRID_STEP;
    graph.tick_line_gap = slot;

    for entry in &waiting {
        let cell = entry.age as usize * cells + entry.lane;
        let y = first_lane + cell as f32 * GRID_STEP;
        graph.lowest_lane = graph.lowest_lane.max(y);
        let wire = &mut graph.wires[entry.wire];
        wire.route = vec![
            entry.from,
            pos2(entry.leave, entry.from.y),
            pos2(entry.leave, y),
            pos2(entry.arrive, y),
            pos2(entry.arrive, entry.to.y),
            entry.to,
        ];
        wire.label_at = pos2((entry.left() + entry.right()) / 2.0, y);
    }

    let oldest = waiting.last().map_or(0, |entry| entry.age);
    graph.extent.y = first_lane + (oldest + 1) as f32 * slot + GRID_STEP / 2.0;
}

pub fn build(
    save: &TrackingSave,
    visible: &[&TrackedFrame],
    later: &[&TrackedFrame],
    has_outside: bool,
    ticks: Range<u64>,
    focus: &[Focus],
    measure: Measure,
) -> TickGraph {
    let by_tick: BTreeMap<u64, &TrackedFrame> =
        visible.iter().map(|frame| (frame.tick, *frame)).collect();

    let systems = ordered_systems(save, has_outside);
    let mut graph = TickGraph {
        palette: Palette::new(save.signal_ids()),
        ..Default::default()
    };
    let mut indices = BTreeMap::new();
    for (tick, frame) in &by_tick {
        let active = systems_in(frame);
        for system in systems.iter().filter(|system| active.contains(system)) {
            indices.insert((*tick, *system), graph.nodes.len());
            graph.nodes.push(Node {
                tick: *tick,
                system: *system,
                full_name: save.system_name(*system).to_string(),
                column: 0,
                inputs: Vec::new(),
                outputs: outputs_of(save, frame, *system),
                position: Pos2::ZERO,
                width: NODE_WIDTH,
                divider: NODE_WIDTH / 2.0,
                faded: false,
            });
        }
    }

    add_inputs_and_wires(save, &by_tick, &indices, &mut graph);
    mark_reads_after_the_view(save, later, &indices, &mut graph);
    let targets: Vec<Target> = focus
        .iter()
        .filter_map(|focus| match *focus {
            Focus::Node(key) => indices.get(&key).copied().map(Target::Node),
            Focus::Wire(key) => graph
                .wires
                .iter()
                .position(|wire| graph.wire_key(wire) == key)
                .map(Target::Wire),
            Focus::Tick(tick) => graph
                .nodes
                .iter()
                .any(|node| node.tick == tick)
                .then_some(Target::Tick(tick)),
        })
        .collect();
    if !targets.is_empty() {
        fade(&mut graph, &targets);
    }

    fit_nodes_to_text(&mut graph, measure);
    let layout = place_in_one_row(&mut graph, ticks);
    route_through_lanes(&mut graph, &layout);
    graph
}

#[cfg(test)]
mod tests {
    use super::counted;
    use super::{
        BLOCK_PADDING, Focus, GRID_STEP, NODE_WIDTH, Node, NodeKey, PORT_MARGIN, PORT_TEXT_SIZE,
        PORT_TRACK_STEP, Port, Target, TickGraph, Wire, build, fade, label_position, lane_cells,
        place_in_one_row, route_through_lanes, sends_from_outside, short_name, wire_at,
    };
    use crate::test_support::{recorded, recorded_with_outside_send};
    use eframe::egui::{Pos2, Rect, pos2};
    use reacs::tracking::TrackingSave;
    use std::ops::Range;

    fn graph_of(save: &TrackingSave, ticks: Range<u64>) -> TickGraph {
        one_row(save, ticks, None)
    }

    fn one_row(save: &TrackingSave, ticks: Range<u64>, focus: Option<NodeKey>) -> TickGraph {
        one_row_on(save, ticks, focus.map(Focus::Node))
    }

    fn one_row_on(save: &TrackingSave, ticks: Range<u64>, focus: Option<Focus>) -> TickGraph {
        let has_outside = save.frames().any(sends_from_outside);
        let visible: Vec<_> = save.frames_in(ticks.clone()).collect();
        let later: Vec<_> = save.frames_in(ticks.end..ticks.end + 64).collect();
        build(
            save,
            &visible,
            &later,
            has_outside,
            ticks,
            focus.as_slice(),
            &|_, _| 0.0,
        )
    }

    pub fn assert_longer_lifespans_are_lower(graph: &TickGraph) {
        let lanes: Vec<(u64, f32)> = graph
            .wires
            .iter()
            .filter(|wire| wire.route.len() == 6)
            .map(|wire| {
                let age = graph.nodes[wire.to_node].tick - graph.nodes[wire.from_node].tick;
                (age, wire.route[2].y)
            })
            .collect();
        for (age, y) in &lanes {
            for (other_age, other_y) in &lanes {
                if age > other_age {
                    assert!(y > other_y, "age {age} is above age {other_age}");
                }
            }
        }
    }

    pub fn assert_routes_follow_the_grid(graph: &TickGraph) {
        let on = |value: f32, step: f32| (value / step).fract() == 0.0;
        for wire in &graph.wires {
            for point in &wire.route {
                assert!(
                    on(point.x, PORT_TRACK_STEP) && on(point.y, GRID_STEP),
                    "{point:?} is off the grid"
                );
            }
        }
    }

    pub fn assert_direct_lines_are_centered(graph: &TickGraph) {
        for wire in graph.wires.iter().filter(|wire| wire.route.len() == 4) {
            let right = graph.nodes[wire.from_node].position.x + graph.nodes[wire.from_node].width;
            let left = graph.nodes[wire.to_node].position.x;
            assert_eq!(wire.route[1].x, (right + left) / 2.0);
            assert_eq!((wire.route[1].x / GRID_STEP).fract(), 0.0);
        }
    }

    pub fn assert_lanes_do_not_overlap(graph: &TickGraph) {
        let spans: Vec<(f32, f32, f32)> = graph
            .wires
            .iter()
            .filter(|wire| wire.route.len() == 6)
            .map(|wire| {
                let (a, b) = (wire.route[2].x, wire.route[3].x);
                (wire.route[2].y, a.min(b), a.max(b))
            })
            .collect();
        for (index, first) in spans.iter().enumerate() {
            for second in &spans[index + 1..] {
                if first.0 == second.0 {
                    assert!(first.2 < second.1 || second.2 < first.1, "lanes overlap");
                }
            }
        }
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
        assert_eq!(inside.label, "FixturePing (2)");
        let across = pings.iter().find(|x| x.to_node == early).unwrap();
        assert_eq!(across.from_node, emit);
        assert_eq!(across.label, "FixturePing (2)");
    }

    #[test]
    fn one_row_puts_every_node_on_one_line_in_order() {
        let save = recorded(4);
        let graph = one_row(&save, 0..4, None);

        let top = graph.nodes[0].position.y;
        for pair in graph.nodes.windows(2) {
            assert_eq!(pair[1].position.y, top);
            assert!(pair[0].position.x + pair[0].size().x < pair[1].position.x);
            assert!(pair[0].tick <= pair[1].tick);
        }
        assert_eq!(graph.ticks.len(), 4);
    }

    #[test]
    fn one_row_wires_stay_below_the_nodes_and_never_behind_them() {
        let save = recorded(4);
        let graph = one_row(&save, 0..4, None);
        assert_no_wire_behind_a_node(&graph);
        assert_lanes_do_not_overlap(&graph);

        let lowest = graph
            .nodes
            .iter()
            .map(|node| node.position.y + node.size().y)
            .fold(0.0, f32::max);
        for wire in graph.wires.iter().filter(|wire| wire.route.len() == 6) {
            assert!(wire.route[2].y > lowest);
        }
    }

    #[test]
    fn longer_lifespans_run_in_lower_lanes() {
        let save = recorded(4);
        assert_longer_lifespans_are_lower(&one_row(&save, 0..4, None));
    }

    #[test]
    fn every_line_follows_the_grid_and_gaps_are_centered() {
        let save = recorded(4);
        for ticks in [0..2, 0..4, 1..4] {
            let graph = one_row(&save, ticks, None);
            assert_routes_follow_the_grid(&graph);
            assert_direct_lines_are_centered(&graph);
        }
    }

    #[test]
    fn the_first_lane_is_one_cell_below_the_tick_blocks() {
        let save = recorded(4);
        let graph = one_row(&save, 0..4, None);
        let tallest = graph
            .nodes
            .iter()
            .map(|node| node.position.y + node.size().y)
            .fold(0.0, f32::max);

        let first = graph
            .wires
            .iter()
            .filter(|wire| wire.route.len() == 6)
            .map(|wire| wire.route[2].y)
            .fold(f32::MAX, f32::min);
        assert_eq!(graph.ticks[0].bottom, tallest + BLOCK_PADDING);
        assert_eq!(first, graph.ticks[0].bottom + GRID_STEP);
    }

    #[test]
    fn a_lifespan_always_sits_at_the_same_depth() {
        let save = recorded(4);
        let short = one_row(&save, 0..2, None);
        let long = one_row(&save, 0..4, None);

        let depth = |graph: &TickGraph, age: u64| {
            let first_lane = graph.ticks[0].bottom + GRID_STEP;
            graph
                .wires
                .iter()
                .filter(|wire| wire.route.len() == 6)
                .filter(|wire| {
                    graph.nodes[wire.to_node].tick - graph.nodes[wire.from_node].tick == age
                })
                .map(|wire| wire.route[2].y - first_lane)
                .fold(f32::MAX, f32::min)
        };
        assert_eq!(depth(&short, 1), depth(&long, 1));
    }

    fn port(signal: u128) -> Port {
        Port {
            signal,
            label: signal.to_string(),
            count: 1,
            cancelled: 0,
            modified: 0,
            external: false,
            wired: false,
            stub_x: 0.0,
            faded: false,
            notes: Vec::new(),
        }
    }

    fn system(tick: u64, id: u128, inputs: Vec<Port>, outputs: Vec<Port>) -> Node {
        Node {
            tick,
            system: id,
            full_name: id.to_string(),
            column: 0,
            inputs,
            outputs,
            position: Pos2::ZERO,
            width: NODE_WIDTH,
            divider: NODE_WIDTH / 2.0,
            faded: false,
        }
    }

    fn link(signal: u128, from: (usize, usize), to: (usize, usize)) -> Wire {
        Wire {
            signal,
            label: signal.to_string(),
            windowed: false,
            from_node: from.0,
            from_port: from.1,
            to_node: to.0,
            to_port: to.1,
            route: Vec::new(),
            label_at: Pos2::ZERO,
            faded: false,
            focused: false,
            details: Vec::new(),
        }
    }

    fn laid_out(nodes: Vec<Node>, wires: Vec<Wire>, ticks: Range<u64>) -> TickGraph {
        let mut graph = TickGraph {
            nodes,
            wires,
            ..Default::default()
        };
        let layout = place_in_one_row(&mut graph, ticks);
        route_through_lanes(&mut graph, &layout);
        graph
    }

    fn two_signals_skipping_a_node() -> TickGraph {
        laid_out(
            vec![
                system(0, 1, vec![], vec![port(10), port(20)]),
                system(0, 2, vec![], vec![]),
                system(0, 3, vec![port(10), port(20)], vec![]),
            ],
            vec![link(10, (0, 0), (2, 0)), link(20, (0, 1), (2, 1))],
            0..1,
        )
    }

    #[test]
    fn leaving_tracks_sit_left_of_the_center_and_entering_ones_right() {
        let graph = two_signals_skipping_a_node();
        let source_right = graph.nodes[0].position.x + NODE_WIDTH;
        let center_after = (source_right + graph.nodes[1].position.x) / 2.0;
        let target_left = graph.nodes[2].position.x;
        let center_before = (graph.nodes[1].position.x + NODE_WIDTH + target_left) / 2.0;

        for wire in &graph.wires {
            assert!(wire.route[1].x > source_right && wire.route[1].x < center_after);
            assert!(wire.route[3].x > center_before && wire.route[3].x < target_left);
        }
    }

    #[test]
    fn the_higher_port_uses_the_track_nearest_the_center() {
        let graph = two_signals_skipping_a_node();
        let (top, bottom) = (&graph.wires[0], &graph.wires[1]);
        assert_eq!(top.route[1].x - bottom.route[1].x, PORT_TRACK_STEP);
        assert_eq!(bottom.route[3].x - top.route[3].x, PORT_TRACK_STEP);
    }

    #[test]
    fn wires_of_one_port_share_a_track() {
        let graph = laid_out(
            vec![
                system(0, 1, vec![], vec![port(10)]),
                system(0, 2, vec![], vec![]),
                system(0, 3, vec![port(10)], vec![]),
                system(0, 4, vec![port(10)], vec![]),
            ],
            vec![link(10, (0, 0), (2, 0)), link(10, (0, 0), (3, 0))],
            0..1,
        );
        assert_eq!(graph.wires[0].route[1].x, graph.wires[1].route[1].x);
    }

    #[test]
    fn a_node_with_many_tracks_widens_its_gap() {
        let outputs: Vec<Port> = (0..9).map(port).collect();
        let inputs: Vec<Port> = (0..9).map(port).collect();
        let wires = (0..9)
            .map(|p| link(p, (0, p as usize), (2, p as usize)))
            .collect();
        let graph = laid_out(
            vec![
                system(0, 1, vec![], outputs),
                system(0, 2, vec![], vec![]),
                system(0, 3, inputs, vec![]),
            ],
            wires,
            0..1,
        );

        assert_no_wire_behind_a_node(&graph);
        assert_routes_follow_the_grid(&graph);
        let lefts: Vec<f32> = graph.nodes.iter().map(|node| node.position.x).collect();
        assert_eq!(lefts[1] - lefts[0] - NODE_WIDTH, 240.0);
        for left in lefts {
            assert_eq!((left / GRID_STEP).fract(), 0.0);
        }
    }

    #[test]
    fn lane_spacing_grows_in_fives() {
        assert_eq!(lane_cells(0), 5);
        assert_eq!(lane_cells(4), 5);
        assert_eq!(lane_cells(5), 10);
        assert_eq!(lane_cells(9), 10);
        assert_eq!(lane_cells(10), 15);
    }

    #[test]
    fn a_crowded_tick_stays_in_its_band_without_overlaps() {
        let count = 12u128;
        let outputs: Vec<Port> = (0..count).map(port).collect();
        let inputs: Vec<Port> = (0..count).map(port).collect();
        let mut wires: Vec<Wire> = (0..count as usize)
            .map(|p| link(p as u128, (0, p), (2, p)))
            .collect();
        wires.push(link(99, (3, 0), (4, 0)));
        let graph = laid_out(
            vec![
                system(0, 1, vec![], outputs),
                system(0, 2, vec![], vec![]),
                system(0, 3, inputs, vec![]),
                system(0, 4, vec![], vec![port(99)]),
                system(1, 5, vec![port(99)], vec![]),
            ],
            wires,
            0..2,
        );

        assert_lanes_do_not_overlap(&graph);
        let first_lane = graph.ticks[0].bottom + GRID_STEP;
        for wire in graph.wires.iter().filter(|wire| wire.route.len() == 6) {
            let age = (graph.nodes[wire.to_node].tick - graph.nodes[wire.from_node].tick) as f32;
            let cell = (wire.route[2].y - first_lane) / GRID_STEP;
            assert!(cell >= age * 15.0 && cell < (age + 1.0) * 15.0);
        }
    }

    fn ping_into_late(graph: &TickGraph) -> usize {
        let (emit, late) = (
            node(graph, 0, "fixture_emit"),
            node(graph, 0, "fixture_late"),
        );
        graph
            .wires
            .iter()
            .position(|wire| wire.from_node == emit && wire.to_node == late)
            .unwrap()
    }

    #[test]
    fn focusing_a_wire_keeps_only_what_runs_through_it() {
        let save = recorded(4);
        let plain = one_row(&save, 0..4, None);
        let key = plain.wire_key(&plain.wires[ping_into_late(&plain)]);
        let graph = one_row_on(&save, 0..4, Some(Focus::Wire(key)));

        let focused = &graph.wires[ping_into_late(&graph)];
        assert!(focused.focused && !focused.faded);
        assert_eq!(graph.wires.iter().filter(|wire| wire.focused).count(), 1);
        assert!(!graph.nodes[node(&graph, 0, "fixture_emit")].faded);
        assert!(!graph.nodes[node(&graph, 0, "fixture_late")].faded);
        assert!(!graph.nodes[node(&graph, 0, "fixture_tail")].faded);
        assert!(graph.nodes[node(&graph, 1, "fixture_early")].faded);
        assert!(graph.focused_nodes.is_empty());

        let emit = node(&graph, 0, "fixture_emit");
        let early = node(&graph, 1, "fixture_early");
        let other = graph
            .wires
            .iter()
            .find(|wire| wire.from_node == emit && wire.to_node == early)
            .unwrap();
        assert!(other.faded);
    }

    #[test]
    fn a_point_on_a_wire_finds_it_and_a_node_blocks_it() {
        let save = recorded(4);
        let graph = one_row(&save, 0..4, None);
        let (index, wire) = graph
            .wires
            .iter()
            .enumerate()
            .find(|(_, wire)| wire.route.len() == 6)
            .unwrap();

        assert_eq!(wire_at(&graph, wire.label_at, 5.0), Some(index));
        let far = wire.label_at + eframe::egui::vec2(0.0, 19.0);
        assert_ne!(wire_at(&graph, far, 5.0), Some(index));
        let inside = graph.nodes[0].position + eframe::egui::vec2(120.0, 20.0);
        assert_eq!(wire_at(&graph, inside, 5000.0), None);
        assert_eq!(wire_at(&graph, pos2(-500.0, -500.0), 5.0), None);
    }

    fn rough_width(text: &str, size: f32) -> f32 {
        text.chars().count() as f32 * size * 0.6
    }

    fn view(left: f32, right: f32) -> Rect {
        Rect::from_min_max(pos2(left, 0.0), pos2(right, 900.0))
    }

    #[test]
    fn a_label_rides_its_line_and_stays_whole_on_it() {
        let path = [pos2(0.0, 500.0), pos2(1000.0, 500.0)];
        let natural = pos2(500.0, 500.0);
        let at = |left, right| label_position(&path, natural, 50.0, view(left, right));

        assert_eq!(at(0.0, 900.0), Some(natural));
        assert_eq!(at(700.0, 1200.0), Some(pos2(756.0, 500.0)));
        assert_eq!(at(100.0, 400.0), Some(pos2(344.0, 500.0)));
        assert_eq!(
            label_position(&path, pos2(10.0, 500.0), 50.0, view(-500.0, 900.0)),
            Some(pos2(56.0, 500.0))
        );
        assert_eq!(at(1100.0, 1300.0), None);
    }

    #[test]
    fn a_label_too_wide_for_its_line_is_hidden() {
        let between_nodes = [
            pos2(0.0, 500.0),
            pos2(40.0, 500.0),
            pos2(40.0, 500.0),
            pos2(80.0, 500.0),
        ];
        let natural = pos2(40.0, 500.0);
        assert_eq!(
            label_position(&between_nodes, natural, 60.0, view(-500.0, 900.0)),
            None
        );
        assert_eq!(
            label_position(&between_nodes, natural, 20.0, view(-500.0, 900.0)),
            Some(natural)
        );
    }

    #[test]
    fn a_label_never_sits_on_a_vertical_part() {
        let path = [
            pos2(0.0, 0.0),
            pos2(100.0, 0.0),
            pos2(100.0, 800.0),
            pos2(600.0, 800.0),
        ];
        let area = Rect::from_min_max(pos2(0.0, 100.0), pos2(400.0, 400.0));
        assert_eq!(label_position(&path, pos2(350.0, 800.0), 20.0, area), None);
    }

    #[test]
    fn outputs_read_after_the_view_are_marked_and_get_a_track() {
        let save = recorded(4);
        let graph = one_row(&save, 0..1, None);
        let emit = &graph.nodes[node(&graph, 0, "fixture_emit")];

        let ping = emit
            .outputs
            .iter()
            .find(|x| x.label == "FixturePing")
            .unwrap();
        assert!(ping.external);
        let beam = emit
            .outputs
            .iter()
            .find(|x| x.label == "FixtureBeam")
            .unwrap();
        assert!(beam.external);
        assert!(ping.stub_x > emit.position.x + emit.width);
        assert_ne!(ping.stub_x, beam.stub_x);
    }

    #[test]
    fn nothing_is_marked_when_every_read_is_on_screen() {
        let save = recorded(1);
        let graph = one_row(&save, 0..1, None);
        let emit = &graph.nodes[node(&graph, 0, "fixture_emit")];
        assert!(emit.outputs.iter().all(|port| !port.external));
    }

    #[test]
    fn selecting_a_tick_keeps_all_of_its_systems_and_their_chains() {
        let save = recorded(4);
        let graph = one_row_on(&save, 0..4, Some(Focus::Tick(1)));

        assert_eq!(graph.focused_ticks, [1]);
        for node in graph.nodes.iter().filter(|node| node.tick == 1) {
            assert!(!node.faded);
        }
        assert!(!graph.nodes[node(&graph, 0, "fixture_emit")].faded);
        assert!(graph.nodes[node(&graph, 0, "fixture_late")].faded);
    }

    #[test]
    fn selecting_an_empty_tick_selects_nothing() {
        let mut save = recorded(4);
        save.push_frame(4, None);
        let graph = one_row_on(&save, 3..5, Some(Focus::Tick(4)));

        assert!(graph.focused_ticks.is_empty());
        assert!(graph.nodes.iter().all(|node| !node.faded));
    }

    #[test]
    fn every_port_text_fits_inside_its_node() {
        let save = recorded(4);
        let visible: Vec<_> = save.frames_in(1..3).collect();
        let graph = build(&save, &visible, &[], false, 1..3, &[], &rough_width);

        assert!(graph.nodes.iter().any(|node| node.width > NODE_WIDTH));
        assert_no_wire_behind_a_node(&graph);
        for node in &graph.nodes {
            assert_eq!((node.width / GRID_STEP).fract(), 0.0);
            for port in &node.inputs {
                let width = rough_width(&port.text(), PORT_TEXT_SIZE);
                assert!(width + PORT_MARGIN * 2.0 <= node.divider, "{}", port.text());
            }
            for port in &node.outputs {
                let width = rough_width(&port.text(), PORT_TEXT_SIZE);
                assert!(
                    width + PORT_MARGIN * 2.0 <= node.width - node.divider,
                    "{}",
                    port.text()
                );
            }
        }
    }

    #[test]
    fn tick_blocks_contain_their_nodes_and_stay_apart() {
        let save = recorded(4);
        let graph = one_row(&save, 0..4, None);
        for pair in graph.ticks.windows(2) {
            assert!(pair[0].right + BLOCK_PADDING < pair[1].left - BLOCK_PADDING);
        }
        for node in &graph.nodes {
            let tick = graph.ticks.iter().find(|x| x.tick == node.tick).unwrap();
            assert!(node.position.y > tick.top);
            assert!(node.position.y + node.size().y < tick.bottom);
        }
    }

    #[test]
    fn a_system_with_nothing_this_tick_has_no_node() {
        let save = recorded(4);
        let graph = graph_of(&save, 0..2);
        assert!(
            !graph
                .nodes
                .iter()
                .any(|x| x.tick == 1 && short_name(&x.full_name) == "fixture_late")
        );
    }

    #[test]
    fn systems_appear_in_execution_order_inside_a_tick() {
        let save = recorded(4);
        let graph = graph_of(&save, 0..1);
        let position = |name: &str| graph.nodes[node(&graph, 0, name)].position.x;
        assert!(position("fixture_emit") < position("fixture_late"));
        assert!(position("fixture_late") < position("fixture_tail"));
    }

    #[test]
    fn signals_from_outside_come_first() {
        let save = recorded_with_outside_send();
        let graph = graph_of(&save, 0..1);
        let outside = node(&graph, 0, "outside");
        assert_eq!(outside, 0);
        assert_eq!(graph.nodes[outside].outputs[0].count, 1);
    }

    #[test]
    fn several_selections_keep_the_chains_of_all_of_them() {
        let save = recorded(4);
        let plain = one_row(&save, 0..4, None);
        let keys: Vec<Focus> = plain.nodes[..2]
            .iter()
            .map(|node| Focus::Node((node.tick, node.system)))
            .collect();
        let single = |key: Focus| one_row_on(&save, 0..4, Some(key));
        let (first, second) = (single(keys[0]), single(keys[1]));

        let has_outside = save.frames().any(sends_from_outside);
        let visible: Vec<_> = save.frames_in(0..4).collect();
        let both = build(&save, &visible, &[], has_outside, 0..4, &keys, &|_, _| 0.0);

        assert_eq!(both.focused_nodes.len(), 2);
        for index in 0..both.nodes.len() {
            let kept = !first.nodes[index].faded || !second.nodes[index].faded;
            assert_eq!(!both.nodes[index].faded, kept);
        }
    }

    #[test]
    fn a_port_fades_with_its_edges() {
        let mut graph = TickGraph {
            nodes: vec![
                system(0, 1, vec![], vec![port(1), port(2)]),
                system(1, 2, vec![port(1)], vec![]),
                system(1, 3, vec![port(2)], vec![]),
            ],
            wires: vec![link(1, (0, 0), (1, 0)), link(2, (0, 1), (2, 0))],
            ..Default::default()
        };

        fade(&mut graph, &[Target::Node(1)]);
        assert!(!graph.nodes[0].outputs[0].faded);
        assert!(graph.nodes[0].outputs[1].faded);
        assert!(!graph.nodes[1].inputs[0].faded);
        assert!(graph.nodes[2].inputs[0].faded);
    }

    #[test]
    fn a_port_with_an_off_screen_edge_stays_while_its_node_stays() {
        let mut outgoing = port(2);
        outgoing.external = true;
        let mut graph = TickGraph {
            nodes: vec![
                system(0, 1, vec![], vec![port(1), outgoing]),
                system(1, 2, vec![port(1)], vec![]),
                system(1, 3, vec![port(2)], vec![]),
            ],
            wires: vec![link(1, (0, 0), (1, 0)), link(2, (0, 1), (2, 0))],
            ..Default::default()
        };

        fade(&mut graph, &[Target::Node(1)]);
        assert!(!graph.nodes[0].outputs[1].faded);
    }

    #[test]
    fn counts_show_only_what_happened() {
        assert_eq!(counted("Wave", 3, 0, 0), "Wave (3)");
        assert_eq!(counted("Wave", 0, 1, 0), "Wave (cancel 1)");
        assert_eq!(counted("Wave", 0, 0, 2), "Wave (modify 2)");
        assert_eq!(counted("Wave", 3, 1, 2), "Wave (3, cancel 1, modify 2)");
    }

    #[test]
    fn a_wire_that_skips_the_focus_is_not_part_of_its_chain() {
        let mut graph = TickGraph {
            nodes: vec![
                system(0, 0, vec![], vec![port(1)]),
                system(0, 1, vec![port(1)], vec![port(1)]),
                system(0, 2, vec![port(1), port(2)], vec![]),
            ],
            wires: vec![
                link(1, (0, 0), (1, 0)),
                link(1, (1, 0), (2, 0)),
                link(2, (0, 0), (2, 1)),
            ],
            ..Default::default()
        };

        fade(&mut graph, &[Target::Node(1)]);
        assert!(!graph.wires[0].faded && !graph.wires[1].faded);
        assert!(graph.wires[2].faded);
    }

    #[test]
    fn a_signal_has_one_input_whatever_mix_of_senders_it_has() {
        let mut node = system(0, 1, vec![], vec![]);
        let wired = super::add_input(&mut node, 7, "Burn", 2, None);
        let older = super::add_input(&mut node, 7, "Burn", 3, Some("a".to_string()));
        let other = super::add_input(&mut node, 8, "Alert", 1, Some("b".to_string()));

        assert_eq!((wired, older, other), (0, 0, 1));
        assert_eq!(node.inputs[0].text(), "Burn (5)");
        assert!(node.inputs[0].wired && node.inputs[0].external);
        assert!(!node.inputs[1].wired && node.inputs[1].external);
    }

    #[test]
    fn the_higher_off_screen_edge_runs_nearest_the_node() {
        let off_screen = |signal| Port {
            external: true,
            ..port(signal)
        };
        let ports = [off_screen(1), port(2), off_screen(3), off_screen(4)];
        let distance = |position| super::stub_distance(&ports, position);
        assert!(distance(0) < distance(2));
        assert!(distance(2) < distance(3));
    }

    #[test]
    fn off_screen_edges_never_share_a_track_with_lanes() {
        let save = recorded(4);
        let graph = one_row(&save, 1..4, None);
        for wire in &graph.wires {
            if wire.route.len() < 6 {
                continue;
            }
            let node = &graph.nodes[wire.to_node];
            for port in node.inputs.iter().filter(|port| port.external) {
                assert_ne!(port.stub_x, wire.route[4].x);
            }
            let node = &graph.nodes[wire.from_node];
            for port in node.outputs.iter().filter(|port| port.external) {
                assert_ne!(port.stub_x, wire.route[1].x);
            }
        }
    }

    #[test]
    fn off_screen_edges_keep_their_distance_on_every_node() {
        let save = recorded(4);
        let graph = one_row(&save, 1..3, None);
        let mut seen = 0;
        for node in &graph.nodes {
            for port in node.inputs.iter().filter(|port| port.external) {
                let distance = node.position.x - port.stub_x;
                assert!(distance > 0.0 && distance < BLOCK_PADDING);
                seen += 1;
            }
        }
        assert!(seen > 0);
    }

    #[test]
    fn a_point_on_an_off_screen_edge_finds_it() {
        let save = recorded(4);
        let graph = one_row(&save, 1..2, None);
        let early = node(&graph, 1, "fixture_early");
        let port = &graph.nodes[early].inputs[0];
        let anchor = super::input_anchor(&graph.nodes[early], 0);

        let rising = pos2(port.stub_x, anchor.y - super::STUB_RISE / 2.0);
        let hit = super::stub_at(&graph, rising, 3.0).expect("the edge is there");
        assert_eq!(hit.node, early);
        assert!(hit.input);
        assert_eq!(graph.stub_key(&hit).signal, port.signal);

        let beside = rising - eframe::egui::vec2(30.0, 0.0);
        assert!(super::stub_at(&graph, beside, 3.0).is_none());
    }

    #[test]
    fn off_screen_edges_remember_where_they_come_from_and_go() {
        let save = recorded(4);
        let incoming = one_row(&save, 1..2, None);
        let early = &incoming.nodes[node(&incoming, 1, "fixture_early")];
        assert_eq!(
            early.inputs[0].notes,
            [
                "from fixture_emit, tick 0",
                "#0 FixturePing { _: 1 }",
                "#1 FixturePing { _: 2 }"
            ]
        );

        let outgoing = one_row(&save, 0..1, None);
        let emit = &outgoing.nodes[node(&outgoing, 0, "fixture_emit")];
        let ping = emit
            .outputs
            .iter()
            .find(|x| x.label == "FixturePing")
            .unwrap();
        assert_eq!(
            ping.notes,
            [
                "#0 FixturePing { _: 1 }",
                "#1 FixturePing { _: 2 }",
                "to fixture_early, tick 1"
            ]
        );
    }

    #[test]
    fn a_wire_lists_the_signals_it_carries() {
        let save = recorded(4);
        let graph = one_row(&save, 0..2, None);
        let wire = graph
            .wires
            .iter()
            .find(|wire| wire.label.starts_with("FixturePing"))
            .unwrap();
        assert_eq!(wire.details[0], "#0 FixturePing { _: 1 }");
    }

    #[test]
    fn a_window_line_shows_its_ticks() {
        let instance = |window| reacs::tracking::TrackedInstance {
            id: 7,
            sender: 1,
            window,
            entities: Vec::new(),
            payload: "Burn(1)".to_string(),
        };
        assert_eq!(super::instance_line(&instance(None), 3), "#7 Burn(1)");
        assert_eq!(
            super::instance_line(&instance(Some((3, 9))), 3),
            "#7 Burn(1), ticks 3..9"
        );
        assert_eq!(
            super::instance_line(&instance(Some((0, u64::MAX))), 3),
            "#7 Burn(1), open-ended"
        );
        assert_eq!(
            super::instance_line(&instance(Some((8, u64::MAX))), 3),
            "#7 Burn(1), from tick 8, open-ended"
        );
    }

    #[test]
    fn tick_lines_sit_on_the_grid_and_clear_of_the_lanes() {
        let save = recorded(4);
        let graph = one_row(&save, 0..4, None);

        assert_eq!((graph.first_tick_line / GRID_STEP).fract(), 0.0);
        assert_eq!((graph.tick_line_gap / GRID_STEP).fract(), 0.0);
        for wire in graph.wires.iter().filter(|wire| wire.route.len() == 6) {
            let from_line = (wire.route[2].y - graph.first_tick_line) / graph.tick_line_gap;
            assert_ne!(from_line.fract(), 0.0, "a lane sits on a tick line");
        }
    }

    #[test]
    fn ages_read_naturally() {
        assert_eq!(super::age_text(0), None);
        assert_eq!(super::age_text(1).unwrap(), "1 tick old");
        assert_eq!(super::age_text(2).unwrap(), "2 ticks old");
    }

    #[test]
    fn one_row_fades_instead_of_hiding() {
        let save = recorded(4);
        let late = key(&save, 0, "fixture_late");
        let plain = one_row(&save, 0..4, None);
        let graph = one_row(&save, 0..4, Some(late));

        assert_eq!(graph.nodes.len(), plain.nodes.len());
        assert_eq!(graph.wires.len(), plain.wires.len());
        assert!(!graph.nodes[graph.focused_nodes[0]].faded);
        assert!(!graph.nodes[node(&graph, 0, "fixture_emit")].faded);
        assert!(!graph.nodes[node(&graph, 0, "fixture_tail")].faded);
        assert!(graph.nodes[node(&graph, 1, "fixture_early")].faded);
        assert!(graph.wires.iter().any(|wire| wire.faded));
        assert!(graph.wires.iter().any(|wire| !wire.faded));
        for (node, same) in graph.nodes.iter().zip(&plain.nodes) {
            assert_eq!(node.position, same.position);
        }
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
    }

    #[test]
    fn a_sender_off_screen_becomes_a_labeled_input() {
        let save = recorded(4);
        let graph = graph_of(&save, 1..2);

        assert!(graph.wires.is_empty());
        let early = node(&graph, 1, "fixture_early");
        let input = &graph.nodes[early].inputs[0];
        assert!(input.external);
        assert_eq!(input.label, "FixturePing");
        assert!(!input.wired);
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
        assert_eq!(wire.label, "FixtureBeam (1)");
    }

    #[test]
    fn windows_off_screen_become_a_labeled_input_with_their_sender() {
        let save = recorded(4);
        let graph = graph_of(&save, 2..3);

        let reader = node(&graph, 2, "fixture_beam_reader");
        let input = &graph.nodes[reader].inputs[0];
        assert!(input.external);
        assert_eq!(input.label, "FixtureBeam");
        assert!(input.external);
        assert_eq!(input.count, 1);
    }

    #[test]
    fn a_tick_with_no_signals_is_an_empty_column() {
        let mut save = recorded(4);
        save.push_frame(4, None);
        let graph = graph_of(&save, 4..5);

        assert!(graph.nodes.is_empty());
        assert!(graph.ticks[0].empty);
    }
}
