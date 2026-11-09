use crate::graph::{
    BLOCK_PADDING, CAPTION_HEIGHT, Focus, GRID_STEP, HEADER_HEIGHT, Node, PORT_MARGIN,
    PORT_TEXT_SIZE, Port, TICK_HEADER_HEIGHT, TITLE_MARGIN, TITLE_TEXT_SIZE, TickColumn, TickGraph,
    Wire, age_text, input_anchor, label_position, output_anchor, row_center, short_name, wire_at,
};
use crate::graph::{Hit, STUB_RISE, stub_at};
use crate::hint::Hovered;
use eframe::egui::{
    self, Align2, Color32, Context, DragPanButtons, FontId, Galley, Id, LayerId, Mesh, Order,
    Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    ecolor::Hsva,
    epaint::Tessellator,
    layers::ShapeIdx,
    text::{LayoutJob, TextWrapping},
    vec2,
};
use eframe::emath::TSTransform;
use std::sync::Arc;

const TICK_LINE: Color32 = Color32::from_rgb(70, 74, 84);
const NODE_FILL: Color32 = Color32::from_rgb(47, 47, 47);
const HEADER_FILL: Color32 = Color32::from_rgb(33, 33, 33);
const NODE_BORDER: Color32 = Color32::from_rgb(20, 20, 20);
const TEXT: Color32 = Color32::from_rgb(222, 222, 222);
const DIM_TEXT: Color32 = Color32::from_rgb(150, 150, 150);
const SOCKET_RADIUS: f32 = 4.5;
const FADED_OPACITY: f32 = 0.18;
const FOCUS_BORDER: Color32 = Color32::from_rgb(255, 196, 64);
const WIRE_CORNER: f32 = 10.0;
const CORNER_STEPS: usize = 6;
const BLOCK_FILL: Color32 = Color32::from_rgb(31, 33, 38);
const BLOCK_ROUNDING: f32 = 8.0;
const WIRE_WIDTH: f32 = 2.0;
const WIRE_EMPHASIS_WIDTH: f32 = 4.0;
const WIRE_REACH: f32 = 7.0;
const MAX_STUB_NOTES: usize = 12;
const DASH_LENGTH: f32 = 8.0;
const DASH_PERIOD: f32 = 14.0;
const OFF_SCREEN_TEXT: f32 = 0.65;
const LABEL_PLATE: Color32 = Color32::from_rgba_premultiplied(20, 21, 23, 215);

const CAMERA_MARGIN: f32 = 40.0;
const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 2.0;
const FOCUS_MAX_ZOOM: f32 = 1.0;
const FOCUS_ROOM: f32 = 80.0;
const TICK_LINE_COLOR: Color32 = Color32::from_rgba_premultiplied(150, 150, 150, 150);
const TICK_LINE_WIDTH: f32 = 1.0;
const GRID_LINE_WIDTH: f32 = 1.0;
const GRID_FADE_START: f32 = 6.0;
const GRID_FADE_RANGE: f32 = 10.0;
const TICK_LINE_TEXT: f32 = 11.0;
const MIN_TICK_OPACITY: f32 = 0.02;
const MIN_MITER: f32 = 0.5;
const MIN_DASH_STEP: f64 = 0.000001;
const TITLE_TEXT_INSET: f32 = 14.0;
const TICK_LINE_TEXT_INSET: f32 = 6.0;

const CANVAS_FILL: Color32 = Color32::from_rgb(24, 25, 28);
const GRID_MINOR: Color32 = Color32::from_rgb(35, 37, 42);
const GRID_MAJOR: Color32 = Color32::from_rgb(50, 53, 60);
const GRID_MAJOR_EVERY: i64 = 5;

pub fn signal_color(signal: u128) -> Color32 {
    Hsva::new((signal % 997) as f32 / 997.0, 0.6, 0.9, 1.0).into()
}

fn visible_span(from: Pos2, to: Pos2, area: Rect) -> Option<(f32, f32)> {
    let delta = to - from;
    let (mut start, mut end) = (0.0f32, 1.0f32);
    for axis in 0..2 {
        if delta[axis] == 0.0 {
            if from[axis] < area.min[axis] || from[axis] > area.max[axis] {
                return None;
            }
            continue;
        }
        let low = (area.min[axis] - from[axis]) / delta[axis];
        let high = (area.max[axis] - from[axis]) / delta[axis];
        start = start.max(low.min(high));
        end = end.min(low.max(high));
    }
    (start < end).then_some((start, end))
}

struct Lines {
    mesh: Mesh,
    zoom: f32,
    pixel: f32,
    visible: Rect,
}

impl Lines {
    fn new(zoom: f32, pixel: f32, visible: Rect) -> Self {
        Self {
            mesh: Mesh::default(),
            zoom,
            pixel,
            visible,
        }
    }

    fn path(&mut self, points: &[Pos2], width: f32, color: impl Fn(Pos2) -> Color32) {
        let half = (width - self.pixel).max(0.0) / 2.0 / self.zoom;
        let feather = self.pixel / self.zoom;
        let first = self.mesh.vertices.len() as u32;
        let direction = |from: usize| (points[from + 1] - points[from]).normalized();
        for (index, point) in points.iter().enumerate() {
            let before = direction(index.saturating_sub(1));
            let after = direction(index.min(points.len() - 2));
            let bend = (before + after).normalized();
            let normal = bend.rot90() / bend.dot(after).max(MIN_MITER);
            let solid = color(*point);
            let clear = Color32::TRANSPARENT;
            let sections = [
                (-half - feather, clear),
                (-half, solid),
                (half, solid),
                (half + feather, clear),
            ];
            for (offset, shade) in sections {
                self.mesh.colored_vertex(*point + normal * offset, shade);
            }
        }
        for segment in 0..points.len() as u32 - 1 {
            for strip in 0..3 {
                let at = first + segment * 4 + strip;
                self.mesh.add_triangle(at, at + 1, at + 4);
                self.mesh.add_triangle(at + 1, at + 5, at + 4);
            }
        }
    }

    fn flush(&mut self, piece: &mut Vec<Pos2>, width: f32, color: &impl Fn(Pos2) -> Color32) {
        if piece.len() >= 2 {
            self.path(piece, width, color);
        }
        piece.clear();
    }

    fn dashed(&mut self, points: &[Pos2], width: f32, color: impl Fn(Pos2) -> Color32) {
        let dash = f64::from(DASH_LENGTH / self.zoom);
        let period = f64::from(DASH_PERIOD / self.zoom);
        let area = self.visible.expand(period as f32);
        let mut piece = Vec::new();
        let mut travelled = 0.0f64;
        for segment in points.windows(2) {
            let (from, to) = (segment[0], segment[1]);
            let length = f64::from(from.distance(to));
            let span = visible_span(from, to, area).filter(|_| length > 0.0);
            let Some((start, end)) = span else {
                self.flush(&mut piece, width, &color);
                travelled += length;
                continue;
            };
            if start > 0.0 {
                self.flush(&mut piece, width, &color);
            }
            let along_line = (to - from) / length as f32;
            let last = f64::from(end) * length;
            let mut along = f64::from(start) * length;
            while along < last {
                let phase = (travelled + along).rem_euclid(period);
                let drawn = phase < dash;
                let step = if drawn { dash - phase } else { period - phase };
                let boundary = along + step.max(MIN_DASH_STEP);
                let next = boundary.min(last);
                if drawn {
                    if piece.is_empty() {
                        piece.push(from + along_line * along as f32);
                    }
                    piece.push(from + along_line * next as f32);
                }
                if !drawn || boundary <= last {
                    self.flush(&mut piece, width, &color);
                }
                along = next;
            }
            if end < 1.0 {
                self.flush(&mut piece, width, &color);
            }
            travelled += length;
        }
        self.flush(&mut piece, width, &color);
    }

    fn into_shape(self) -> Shape {
        Shape::mesh(self.mesh)
    }
}

fn grid_opacity(spacing: f32) -> f32 {
    ((spacing - GRID_FADE_START) / GRID_FADE_RANGE).clamp(0.0, 1.0)
}

fn draw_grid(lines: &mut Lines) {
    let visible = lines.visible;
    let minor = GRID_MINOR.gamma_multiply(grid_opacity(GRID_STEP * lines.zoom));
    let major_spacing = GRID_STEP * GRID_MAJOR_EVERY as f32 * lines.zoom;
    let major = GRID_MAJOR.gamma_multiply(grid_opacity(major_spacing));
    let color = |index: i64| {
        if index.rem_euclid(GRID_MAJOR_EVERY) == 0 {
            major
        } else {
            minor
        }
    };
    let cells =
        |low: f32, high: f32| (low / GRID_STEP).floor() as i64..=(high / GRID_STEP).ceil() as i64;

    for index in cells(visible.left(), visible.right()) {
        let (x, shade) = (index as f32 * GRID_STEP, color(index));
        if shade.a() > 0 {
            let line = [
                egui::pos2(x, visible.top()),
                egui::pos2(x, visible.bottom()),
            ];
            lines.path(&line, GRID_LINE_WIDTH, |_| shade);
        }
    }
    for index in cells(visible.top(), visible.bottom()) {
        let (y, shade) = (index as f32 * GRID_STEP, color(index));
        if shade.a() > 0 {
            let line = [
                egui::pos2(visible.left(), y),
                egui::pos2(visible.right(), y),
            ];
            lines.path(&line, GRID_LINE_WIDTH, |_| shade);
        }
    }
}

fn rounded(points: &[Pos2]) -> Vec<Pos2> {
    let mut path = vec![points[0]];
    for corner in points.windows(3) {
        let (before, at, after) = (corner[0], corner[1], corner[2]);
        let radius = WIRE_CORNER
            .min(before.distance(at) / 2.0)
            .min(at.distance(after) / 2.0);
        let start = at + (before - at).normalized() * radius;
        let end = at + (after - at).normalized() * radius;
        for step in 0..=CORNER_STEPS {
            let t = step as f32 / CORNER_STEPS as f32;
            push_distinct(&mut path, start.lerp(at, t).lerp(at.lerp(end, t), t));
        }
    }
    push_distinct(&mut path, points[points.len() - 1]);
    path
}

fn push_distinct(path: &mut Vec<Pos2>, point: Pos2) {
    if path.last().is_none_or(|last| last.distance(point) > 0.01) {
        path.push(point);
    }
}

fn draw_wire(lines: &mut Lines, color: Color32, wire: &Wire, emphasized: bool) {
    let color = if wire.faded {
        color.gamma_multiply(FADED_OPACITY)
    } else {
        color
    };
    let path = rounded(&wire.route);
    let width = if emphasized {
        WIRE_EMPHASIS_WIDTH
    } else {
        WIRE_WIDTH
    };
    if wire.windowed {
        lines.dashed(&path, width, |_| color);
    } else if Rect::from_points(&path).intersects(lines.visible) {
        lines.path(&path, width, |_| color);
    }
}

fn draw_wire_label(base: &Painter, color: Color32, wire: &Wire, visible: Rect) {
    let mut painter = base.clone();
    if wire.faded {
        painter.multiply_opacity(FADED_OPACITY);
    }
    let label = painter.layout_no_wrap(wire.label.clone(), FontId::proportional(12.0), color);
    let half_width = label.size().x / 2.0;
    if let Some(at) = label_position(&wire.route, wire.label_at, half_width, visible) {
        let top_left = at - vec2(half_width, 8.0 + label.size().y / 2.0);
        let plate = Rect::from_min_size(top_left, label.size()).expand2(vec2(3.0, 0.0));
        painter.rect_filled(plate, 3.0, LABEL_PLATE);
        painter.galley(top_left, label, color);
    }
}

fn elided(painter: &Painter, text: &str, font: FontId, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_string(), font, color);
    job.wrap = TextWrapping::truncate_at_width(width.max(0.0));
    painter.layout_job(job)
}

fn draw_edge_from_above(lines: &mut Lines, anchor: Pos2, track_x: f32, color: Color32, width: f32) {
    let corner = egui::pos2(track_x, anchor.y);
    let path = rounded(&[anchor, corner, corner - vec2(0.0, STUB_RISE)]);
    lines.dashed(&path, width, |point| {
        let risen = ((anchor.y - point.y) / STUB_RISE).clamp(0.0, 1.0);
        color.gamma_multiply(1.0 - risen)
    });
}

fn draw_stubs(lines: &mut Lines, graph: &TickGraph) {
    for node in &graph.nodes {
        let opacity = if node.faded { FADED_OPACITY } else { 1.0 };
        for (input, ports) in [(true, &node.inputs), (false, &node.outputs)] {
            for (index, port) in ports.iter().enumerate().filter(|(_, x)| x.external) {
                let anchor = if input {
                    input_anchor(node, index)
                } else {
                    output_anchor(node, index)
                };
                let color = signal_color(port.signal).gamma_multiply(opacity);
                draw_edge_from_above(lines, anchor, port.stub_x, color, WIRE_WIDTH);
            }
        }
    }
}

fn draw_ports(painter: &Painter, node: &Node, ports: &[Port], inputs: bool) {
    let (align, x) = if inputs {
        (Align2::LEFT_CENTER, node.position.x + PORT_MARGIN)
    } else {
        (
            Align2::RIGHT_CENTER,
            node.position.x + node.width - PORT_MARGIN,
        )
    };

    for (index, port) in ports.iter().enumerate() {
        let anchor = if inputs {
            input_anchor(node, index)
        } else {
            output_anchor(node, index)
        };
        let color = signal_color(port.signal);
        painter.circle_filled(anchor, SOCKET_RADIUS, color);
        painter.text(
            egui::pos2(x, node.position.y + row_center(index)),
            align,
            port.text(),
            FontId::proportional(PORT_TEXT_SIZE),
            if port.external && !port.wired {
                color.gamma_multiply(OFF_SCREEN_TEXT)
            } else {
                color
            },
        );
    }
}

fn draw_node(base: &Painter, node: &Node, position: Pos2, focused: bool) {
    let mut painter = base.clone();
    if node.faded {
        painter.multiply_opacity(FADED_OPACITY);
    }
    let painter = &painter;

    let rect = Rect::from_min_size(position, node.size());
    let border = if focused {
        Stroke::new(3.0, FOCUS_BORDER)
    } else {
        Stroke::new(1.0, NODE_BORDER)
    };
    painter.rect(rect, 4.0, NODE_FILL, border, StrokeKind::Inside);

    let header = Rect::from_min_size(position, vec2(node.width, HEADER_HEIGHT));
    painter.rect_filled(header, 4.0, HEADER_FILL);
    let title_width = node.width - TITLE_MARGIN * 2.0;
    let font = FontId::proportional(TITLE_TEXT_SIZE);
    let mut title = elided(painter, &node.full_name, font.clone(), TEXT, title_width);
    if title.elided {
        let short = short_name(&node.full_name);
        title = elided(painter, short, font, TEXT, title_width);
    }
    let title_top = header.center().y - title.size().y / 2.0;
    painter.galley(
        egui::pos2(header.left() + TITLE_MARGIN, title_top),
        title,
        TEXT,
    );

    let body_top = position.y + HEADER_HEIGHT;
    let middle = position.x + node.divider;
    painter.line_segment(
        [
            egui::pos2(middle, body_top),
            egui::pos2(middle, rect.bottom()),
        ],
        Stroke::new(1.0, NODE_BORDER),
    );
    let caption_y = body_top + CAPTION_HEIGHT / 2.0;
    let caption = FontId::proportional(10.0);
    painter.text(
        egui::pos2(position.x + PORT_MARGIN, caption_y),
        Align2::LEFT_CENTER,
        "INPUTS",
        caption.clone(),
        DIM_TEXT,
    );
    painter.text(
        egui::pos2(position.x + node.width - PORT_MARGIN, caption_y),
        Align2::RIGHT_CENTER,
        "OUTPUTS",
        caption,
        DIM_TEXT,
    );

    draw_ports(painter, node, &node.inputs, true);
    draw_ports(painter, node, &node.outputs, false);
}

fn block_rect(tick: &TickColumn) -> Rect {
    Rect::from_min_max(
        egui::pos2(tick.left - BLOCK_PADDING, tick.top),
        egui::pos2(tick.right + BLOCK_PADDING, tick.bottom),
    )
}

fn tick_title_rect(tick: &TickColumn) -> Rect {
    let block = block_rect(tick);
    Rect::from_min_size(block.min, vec2(block.width(), TICK_HEADER_HEIGHT))
}

fn visible_tick_lines(graph: &TickGraph, visible: Rect) -> Vec<(u64, f32)> {
    if graph.tick_line_gap <= 0.0 {
        return Vec::new();
    }
    let passed = ((visible.top() - graph.first_tick_line) / graph.tick_line_gap)
        .ceil()
        .max(0.0) as u64;
    (passed..)
        .map(|index| {
            let y = graph.first_tick_line + index as f32 * graph.tick_line_gap;
            (index + 1, y)
        })
        .take_while(|(_, y)| *y < visible.bottom())
        .collect()
}

fn blocks_span(graph: &TickGraph) -> Option<(f32, f32)> {
    let first = graph.ticks.first()?;
    let last = graph.ticks.last()?;
    Some((block_rect(first).left(), block_rect(last).right()))
}

fn longest_tick_age(graph: &TickGraph) -> u64 {
    graph.ticks.len().saturating_sub(1) as u64
}

fn draw_tick_lines(lines: &mut Lines, graph: &TickGraph) {
    let Some((left, right)) = blocks_span(graph) else {
        return;
    };
    let longest = longest_tick_age(graph);
    for (age, y) in visible_tick_lines(graph, lines.visible) {
        let opacity = tick_opacity(age, longest);
        if opacity >= MIN_TICK_OPACITY {
            let color = TICK_LINE_COLOR.gamma_multiply(opacity);
            let line = [egui::pos2(left, y), egui::pos2(right, y)];
            lines.path(&line, TICK_LINE_WIDTH, |_| color);
        }
    }
}

fn tick_opacity(age: u64, longest: u64) -> f32 {
    0.5f32.powf(age.saturating_sub(longest) as f32)
}

fn draw_tick_line_labels(painter: &Painter, graph: &TickGraph, visible: Rect) {
    let Some((left, right)) = blocks_span(graph) else {
        return;
    };
    let longest = longest_tick_age(graph);
    for (age, y) in visible_tick_lines(graph, visible) {
        let opacity = tick_opacity(age, longest);
        let (Some(text), true) = (age_text(age), opacity >= MIN_TICK_OPACITY) else {
            continue;
        };
        let color = TEXT.gamma_multiply(opacity);
        let font = FontId::proportional(TICK_LINE_TEXT);
        let label = painter.layout_no_wrap(text, font, color);
        let lowest = left + TICK_LINE_TEXT_INSET;
        let highest = (right - label.size().x - TICK_LINE_TEXT_INSET).max(lowest);
        let x = (visible.left() + TICK_LINE_TEXT_INSET).clamp(lowest, highest);
        let top_left = egui::pos2(x, y - label.size().y - 2.0);
        let plate = Rect::from_min_size(top_left, label.size()).expand2(vec2(3.0, 0.0));
        painter.rect_filled(plate, 3.0, LABEL_PLATE.gamma_multiply(opacity));
        painter.galley(top_left, label, color);
    }
}

fn draw_blocks(painter: &Painter, graph: &TickGraph) {
    for tick in &graph.ticks {
        painter.rect(
            block_rect(tick),
            BLOCK_ROUNDING,
            BLOCK_FILL,
            Stroke::new(1.0, TICK_LINE),
            StrokeKind::Inside,
        );
    }
}

fn draw_tick_titles(painter: &Painter, graph: &TickGraph) {
    for tick in &graph.ticks {
        let title = tick_title_rect(tick);
        let border = if graph.focused_ticks.contains(&tick.tick) {
            Stroke::new(3.0, FOCUS_BORDER)
        } else {
            Stroke::new(1.0, TICK_LINE)
        };
        painter.rect(
            title,
            BLOCK_ROUNDING,
            HEADER_FILL,
            border,
            StrokeKind::Inside,
        );
        let text = if tick.empty {
            format!("Tick {} (no signals)", tick.tick)
        } else {
            format!("Tick {}", tick.tick)
        };
        painter.text(
            title.left_center() + vec2(TITLE_TEXT_INSET, 0.0),
            Align2::LEFT_CENTER,
            text,
            FontId::proportional(16.0),
            TEXT,
        );
    }
}

pub enum Click {
    On(Focus),
    Toggle(Focus),
    Menu(Hit, Pos2),
    Background,
}

pub struct CanvasOutput {
    pub click: Option<Click>,
    pub hovered: Option<Hovered>,
}

fn describe_wire(graph: &TickGraph, wire: &Wire) -> String {
    let source = &graph.nodes[wire.from_node];
    let target = &graph.nodes[wire.to_node];
    let mut text = wire.label.clone();
    if wire.windowed {
        text.push_str(", window");
    }
    if let Some(age) = age_text(target.tick - source.tick) {
        text.push('\n');
        text.push_str(&age);
    }
    text.push_str(&format!(
        "\nfrom {} on tick {}\nto {} on tick {}",
        source.full_name, source.tick, target.full_name, target.tick
    ));
    text
}

fn describe_stub(port: &Port) -> String {
    let mut text = format!("{}, off screen", port.label);
    for note in port.notes.iter().take(MAX_STUB_NOTES) {
        text.push('\n');
        text.push_str(note);
    }
    if port.notes.len() > MAX_STUB_NOTES {
        text.push_str(&format!("\nand {} more", port.notes.len() - MAX_STUB_NOTES));
    }
    text
}

fn describe_node(node: &Node) -> String {
    let mut text = node.full_name.clone();
    for (title, ports) in [("Inputs", &node.inputs), ("Outputs", &node.outputs)] {
        if !ports.is_empty() {
            text.push_str(&format!("\n{title}"));
        }
        for port in ports.iter() {
            text.push_str(&format!("\n  {}", port.text()));
        }
    }
    text
}

pub fn limit_camera(view: Rect, bounds: Rect) -> Rect {
    let shrink = (bounds.width() / view.width())
        .max(bounds.height() / view.height())
        .min(1.0);
    keep_inside(
        Rect::from_center_size(view.center(), view.size() * shrink),
        bounds,
    )
}

fn keep_inside(view: Rect, bounds: Rect) -> Rect {
    let axis = |center: f32, half: f32, low: f32, high: f32| {
        let (near, far) = (low + half, high - half);
        center.clamp(near.min(far), near.max(far))
    };
    let size = view.size();
    let center = egui::pos2(
        axis(view.center().x, size.x / 2.0, bounds.left(), bounds.right()),
        axis(view.center().y, size.y / 2.0, bounds.top(), bounds.bottom()),
    );
    Rect::from_center_size(center, size)
}

fn node_rect(node: &Node) -> Rect {
    Rect::from_min_size(node.position, node.size())
}

fn focus_rect(graph: &TickGraph, focus: Focus) -> Option<Rect> {
    match focus {
        Focus::Node(key) => graph
            .nodes
            .iter()
            .find(|node| (node.tick, node.system) == key)
            .map(node_rect),
        Focus::Wire(key) => {
            let wire = graph.wires.iter().find(|x| graph.wire_key(x) == key)?;
            let ends = node_rect(&graph.nodes[wire.from_node])
                .union(node_rect(&graph.nodes[wire.to_node]));
            Some(wire.route.iter().fold(ends, |all, x| {
                all.union(Rect::from_min_size(*x, vec2(0.0, 0.0)))
            }))
        }
        Focus::Tick(tick) => graph.ticks.iter().find(|x| x.tick == tick).map(block_rect),
    }
}

pub fn focus_view(screen: Rect, graph: &TickGraph, focus: &[Focus]) -> Option<Rect> {
    let target = focus
        .iter()
        .filter_map(|focus| focus_rect(graph, *focus))
        .reduce(Rect::union)?
        .expand(FOCUS_ROOM);
    let scale = (screen.size() / target.size())
        .min_elem()
        .clamp(MIN_ZOOM, FOCUS_MAX_ZOOM);
    let view = Rect::from_center_size(target.center(), screen.size() / scale);
    Some(camera_bounds(graph).map_or(view, |bounds| limit_camera(view, bounds)))
}

pub fn keep_on_graph(screen: Rect, camera: Rect, graph: &TickGraph, keep_zoom: bool) -> Rect {
    let Some(bounds) = camera_bounds(graph).filter(|_| camera.is_positive()) else {
        return camera;
    };
    let view = fit_view(screen, camera);
    let limited = if keep_zoom {
        keep_inside(view, bounds)
    } else {
        limit_camera(view, bounds)
    };
    if limited.min.distance(view.min) + limited.max.distance(view.max) > 0.01 {
        limited
    } else {
        camera
    }
}

pub fn camera_bounds(graph: &TickGraph) -> Option<Rect> {
    let (left, right) = blocks_span(graph)?;
    let block = graph.ticks.first()?;
    let bottom = block.bottom.max(graph.lowest_lane);
    let content = Rect::from_min_max(egui::pos2(left, block.top), egui::pos2(right, bottom));
    Some(content.expand(CAMERA_MARGIN))
}

pub fn fit_view(screen: Rect, bounds: Rect) -> Rect {
    fit_transform(screen, bounds).inverse() * screen
}

fn camera_correction(view: Rect, graph: &TickGraph) -> Option<Rect> {
    if !view.is_finite() || !view.is_positive() {
        return None;
    }
    let limited = limit_camera(view, camera_bounds(graph)?);
    let moved = limited.min.distance(view.min) + limited.max.distance(view.max);
    (moved > 0.01).then_some(limited)
}

fn fit_transform(screen: Rect, view: Rect) -> TSTransform {
    let scale = (screen.size() / view.size())
        .min_elem()
        .clamp(MIN_ZOOM, MAX_ZOOM);
    let translation = screen.center().to_vec2() - scale * view.center().to_vec2();
    TSTransform::from_translation(translation) * TSTransform::from_scaling(scale)
}

fn keep_camera_on_graph(ui: &mut egui::Ui, screen: Rect, graph: &TickGraph, before: Rect) {
    let context = ui.ctx().clone();
    let layer = ui.layer_id();
    let (Some(current), Some(bounds), true) = (
        context.layer_transform_to_global(layer),
        camera_bounds(graph),
        before.is_positive(),
    ) else {
        return;
    };

    let expected = fit_transform(screen, before);
    let moved = (current.translation - expected.translation).length() > 0.01
        || (current.scaling - expected.scaling).abs() > 1e-4;
    let view = current.inverse() * screen;
    if !moved && view.intersects(bounds) {
        return;
    }
    let Some(limited) = camera_correction(view, graph) else {
        return;
    };

    let corrected = fit_transform(screen, limited);
    context.set_transform_layer(layer, corrected);
    ui.set_clip_rect(corrected.inverse() * screen);
}

pub fn title_anchor(graph: &TickGraph, view: Rect) -> Option<(usize, f32)> {
    let center = view.center().x;
    let distance = |tick: &TickColumn| (block_rect(tick).center().x - center).abs();
    graph
        .ticks
        .iter()
        .enumerate()
        .min_by(|a, b| distance(a.1).total_cmp(&distance(b.1)))
        .map(|(slot, tick)| (slot, block_rect(tick).left()))
}

pub fn follow_title(graph: &TickGraph, view: Rect, anchor: (usize, f32)) -> Rect {
    let (slot, left) = anchor;
    match graph.ticks.get(slot).or(graph.ticks.last()) {
        Some(tick) => view.translate(vec2(block_rect(tick).left() - left, 0.0)),
        None => view,
    }
}

fn static_shapes(context: &Context, graph: &TickGraph) -> Vec<Shape> {
    let layer = LayerId::new(Order::Background, Id::unique("tracker static shapes"));
    let painter = Painter::new(context.clone(), layer, Rect::EVERYTHING);
    draw_blocks(&painter, graph);
    draw_tick_titles(&painter, graph);
    for (index, node) in graph.nodes.iter().enumerate() {
        let focused =
            graph.focused_nodes.contains(&index) || graph.focused_ticks.contains(&node.tick);
        draw_node(&painter, node, node.position, focused);
    }
    context.graphics_mut(|layers| {
        let list = layers.entry(layer);
        let count = list.all_entries().len();
        let mut shapes = Vec::with_capacity(count);
        for index in 0..count {
            list.mutate_shape(ShapeIdx(index), |clipped| {
                shapes.push(std::mem::replace(&mut clipped.shape, Shape::Noop));
            });
        }
        shapes
    })
}

#[derive(Default)]
pub struct StaticMesh {
    built_for: Option<(f32, [usize; 2])>,
    mesh: Arc<Mesh>,
}

impl StaticMesh {
    #[cfg(test)]
    pub fn mesh_for_tests(&self) -> Arc<Mesh> {
        self.mesh.clone()
    }

    fn current(&mut self, context: &Context, graph: &TickGraph) -> Arc<Mesh> {
        let pixels_per_point = context.pixels_per_point();
        let font_size = context.fonts(|fonts| fonts.font_image_size());
        let wanted = Some((pixels_per_point, font_size));
        if self.built_for != wanted {
            let shapes = static_shapes(context, graph);
            let options = context.tessellation_options(|options| *options);
            let mut tessellator =
                Tessellator::new(pixels_per_point, options, font_size, Vec::new());
            let mut mesh = Mesh::default();
            for shape in shapes {
                tessellator.tessellate_shape(shape, &mut mesh);
            }
            self.mesh = Arc::new(mesh);
            self.built_for = wanted;
        }
        self.mesh.clone()
    }
}

pub fn draw(
    ui: &mut egui::Ui,
    graph: &TickGraph,
    cache: &mut StaticMesh,
    scene_rect: &mut Rect,
) -> CanvasOutput {
    let screen = ui.available_rect_before_wrap();
    let before = *scene_rect;
    let output = egui::Scene::new()
        .zoom_range(MIN_ZOOM..=MAX_ZOOM)
        .drag_pan_buttons(
            DragPanButtons::PRIMARY | DragPanButtons::SECONDARY | DragPanButtons::MIDDLE,
        )
        .show(ui, scene_rect, |ui| {
            keep_camera_on_graph(ui, screen, graph, before);
            if graph.extent.x > 0.0 && graph.extent.y > 0.0 {
                ui.allocate_rect(
                    Rect::from_min_size(Pos2::ZERO, graph.extent),
                    Sense::hover(),
                );
            }
            let painter = ui.painter().clone();
            let visible = ui.clip_rect();
            let zoom = ui
                .ctx()
                .layer_transform_to_global(ui.layer_id())
                .map_or(1.0, |transform| transform.scaling);
            let pixel = 1.0 / ui.ctx().pixels_per_point();
            painter.rect_filled(visible, 0.0, CANVAS_FILL);
            let mut grid = Lines::new(zoom, pixel, visible);
            draw_grid(&mut grid);
            painter.add(grid.into_shape());
            painter.add(Shape::mesh(cache.current(ui.ctx(), graph)));

            let mut clicked = None;
            let mut hovered_node = None;
            let mut menu = None;
            for (index, node) in graph.nodes.iter().enumerate() {
                let key = (node.tick, node.system);
                let rect = Rect::from_min_size(node.position, node.size());
                let response = ui
                    .interact(rect, Id::unique(("node", key)), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if response.clicked() {
                    clicked = Some(Focus::Node(key));
                }
                if response.hovered() {
                    hovered_node = Some(index);
                }
                if response.secondary_clicked() {
                    let at = ui.input(|input| input.pointer.interact_pos());
                    menu = at.map(|at| (Hit::Node(key), at));
                }
            }
            for tick in graph.ticks.iter().filter(|tick| !tick.empty) {
                let id = Id::unique(("tick", tick.tick));
                let response = ui
                    .interact(tick_title_rect(tick), id, Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if response.clicked() {
                    clicked = Some(Focus::Tick(tick.tick));
                }
            }

            let point = ui.response().hover_pos().filter(|point| {
                !graph
                    .ticks
                    .iter()
                    .any(|tick| tick_title_rect(tick).contains(*point))
            });
            let reach = WIRE_REACH / zoom;
            let hovered = point.and_then(|point| wire_at(graph, point, reach));
            let hovered_stub = point
                .filter(|_| hovered.is_none())
                .and_then(|point| stub_at(graph, point, reach));
            if hovered.is_some() || hovered_stub.is_some() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }

            let layer = |index: usize, wire: &Wire| match wire {
                wire if wire.focused || hovered == Some(index) => 2,
                wire if wire.faded => 0,
                _ => 1,
            };
            let mut lines = Lines::new(zoom, pixel, visible);
            draw_tick_lines(&mut lines, graph);
            draw_stubs(&mut lines, graph);
            for pass in 0..3 {
                for (index, wire) in graph.wires.iter().enumerate() {
                    if layer(index, wire) == pass {
                        let color = signal_color(wire.signal);
                        draw_wire(&mut lines, color, wire, pass == 2);
                    }
                }
            }
            if let Some(hit) = &hovered_stub {
                let (port, anchor) = graph.stub(hit);
                draw_edge_from_above(
                    &mut lines,
                    anchor,
                    port.stub_x,
                    signal_color(port.signal),
                    WIRE_EMPHASIS_WIDTH,
                );
            }
            painter.add(lines.into_shape());
            for pass in 0..3 {
                for (index, wire) in graph.wires.iter().enumerate() {
                    if layer(index, wire) == pass {
                        draw_wire_label(&painter, signal_color(wire.signal), wire, visible);
                    }
                }
            }
            draw_tick_line_labels(&painter, graph, visible);
            (clicked, menu, hovered_node, hovered, hovered_stub)
        });

    let moved = *scene_rect != before;
    let lost = camera_bounds(graph).is_some_and(|bounds| !scene_rect.intersects(bounds));
    if moved || lost {
        if let Some(limited) = camera_correction(*scene_rect, graph) {
            *scene_rect = limited;
        }
    }

    let (clicked, menu, hovered_node, hovered_wire, hovered_stub) = output.inner;
    let pointer = ui.input(|input| input.pointer.interact_pos());
    let under_pointer = hovered_wire
        .map(|index| Hit::Wire(graph.wire_key(&graph.wires[index])))
        .or(hovered_stub
            .as_ref()
            .map(|hit| Hit::Stub(graph.stub_key(hit))));
    let wire_menu = under_pointer
        .filter(|_| output.response.secondary_clicked())
        .and_then(|hit| pointer.map(|at| (hit, at)));
    let shift = ui.input(|input| input.modifiers.shift);
    let select = |focus| {
        if shift {
            Click::Toggle(focus)
        } else {
            Click::On(focus)
        }
    };
    let click = menu
        .or(wire_menu)
        .map(|(target, at)| Click::Menu(target, at))
        .or(clicked.map(select))
        .or_else(|| {
            let clicked = output.response.clicked();
            clicked.then_some(()).and_then(|()| match hovered_wire {
                Some(index) => Some(select(Focus::Wire(graph.wire_key(&graph.wires[index])))),
                None => (!shift).then_some(Click::Background),
            })
        });

    let hovered = match (hovered_node, hovered_wire, &hovered_stub) {
        (Some(index), _, _) => {
            let node = &graph.nodes[index];
            Some(Hovered {
                target: Hit::Node((node.tick, node.system)),
                text: describe_node(node),
            })
        }
        (None, Some(index), _) => {
            let wire = &graph.wires[index];
            Some(Hovered {
                target: Hit::Wire(graph.wire_key(wire)),
                text: describe_wire(graph, wire),
            })
        }
        (None, None, Some(hit)) => Some(Hovered {
            target: Hit::Stub(graph.stub_key(hit)),
            text: describe_stub(graph.stub(hit).0),
        }),
        (None, None, None) => None,
    };
    CanvasOutput { click, hovered }
}

#[cfg(test)]
mod tests {
    use super::{Lines, follow_title, tick_opacity, title_anchor, visible_tick_lines};
    use crate::graph::{BLOCK_PADDING, TickColumn, TickGraph};
    use eframe::egui::{Color32, Pos2, Rect, pos2, vec2};

    const SCREEN: Rect = Rect {
        min: Pos2::ZERO,
        max: pos2(1000.0, 1000.0),
    };

    fn width_on_screen(zoom: f32) -> f32 {
        let mut lines = Lines::new(zoom, 1.0, SCREEN);
        lines.path(&[pos2(0.0, 5.0), pos2(100.0, 5.0)], 2.0, |_| Color32::WHITE);
        let top = lines.mesh.vertices[1].pos.y;
        let bottom = lines.mesh.vertices[2].pos.y;
        (bottom - top).abs() * zoom
    }

    #[test]
    fn lines_keep_their_width_at_any_zoom() {
        for zoom in [0.05, 0.5, 1.0, 2.0] {
            assert!((width_on_screen(zoom) - 1.0).abs() < 0.001);
        }
    }

    fn dashes(zoom: f32, visible: Rect, length: f32) -> usize {
        let mut lines = Lines::new(zoom, 1.0, visible);
        lines.dashed(&[pos2(0.0, 50.0), pos2(length, 50.0)], 2.0, |_| {
            Color32::WHITE
        });
        lines.mesh.vertices.len() / 8
    }

    #[test]
    fn dashes_far_along_a_very_long_edge_finish_at_any_zoom() {
        let visible = Rect::from_min_size(pos2(60_000.0, 0.0), vec2(2000.0, 100.0));
        for step in 0..3000 {
            let zoom = 0.05 + step as f32 * 0.0006;
            let mut lines = Lines::new(zoom, 1.0, visible);
            lines.dashed(&[pos2(0.0, 50.0), pos2(80_000.0, 50.0)], 2.0, |_| {
                Color32::WHITE
            });
            assert!(lines.mesh.vertices.len() < 20_000);
        }
    }

    #[test]
    fn dashes_are_the_same_size_on_screen_at_any_zoom() {
        assert_eq!(dashes(1.0, SCREEN, 140.0), 10);
        assert_eq!(dashes(0.5, SCREEN, 280.0), 10);
    }

    #[test]
    fn dashes_outside_the_view_are_not_built() {
        let far = Rect::from_min_size(pos2(0.0, 900.0), vec2(1000.0, 100.0));
        assert_eq!(dashes(1.0, far, 100_000.0), 0);
        assert!(dashes(1.0, SCREEN, 100_000.0) < 100);
    }

    #[test]
    fn dash_gaps_stay_in_place_when_the_start_is_off_screen() {
        let visible = Rect::from_min_size(pos2(700.0, 0.0), vec2(1000.0, 100.0));
        let mut lines = Lines::new(1.0, 1.0, visible);
        lines.dashed(&[pos2(0.0, 50.0), pos2(2000.0, 50.0)], 2.0, |_| {
            Color32::WHITE
        });
        let start = lines.mesh.vertices[0].pos.x;
        assert_eq!(start.rem_euclid(14.0), 0.0);
    }

    #[test]
    fn tick_lines_fade_past_the_longest_age_shown() {
        assert_eq!(tick_opacity(1, 2), 1.0);
        assert_eq!(tick_opacity(2, 2), 1.0);
        assert_eq!(tick_opacity(3, 2), 0.5);
        assert_eq!(tick_opacity(4, 2), 0.25);
    }

    fn tick(tick: u64, left: f32, right: f32) -> TickColumn {
        TickColumn {
            tick,
            left,
            right,
            top: 40.0,
            bottom: 400.0,
            empty: false,
        }
    }

    fn lined() -> TickGraph {
        TickGraph {
            first_tick_line: 400.0,
            tick_line_gap: 200.0,
            ..Default::default()
        }
    }

    fn view(top: f32, bottom: f32) -> Rect {
        Rect::from_min_max(pos2(0.0, top), pos2(100.0, bottom))
    }

    #[test]
    fn tick_lines_are_numbered_by_age_and_reach_the_bottom() {
        let lines = visible_tick_lines(&lined(), view(0.0, 900.0));
        assert_eq!(lines, [(1, 400.0), (2, 600.0), (3, 800.0)]);
    }

    #[test]
    fn scrolling_down_keeps_the_numbering() {
        let lines = visible_tick_lines(&lined(), view(500.0, 700.0));
        assert_eq!(lines, [(2, 600.0)]);
    }

    #[test]
    fn nothing_is_drawn_above_the_first_line() {
        assert!(visible_tick_lines(&lined(), view(0.0, 300.0)).is_empty());
    }

    #[test]
    fn the_reference_title_is_the_one_nearest_the_camera() {
        let graph = TickGraph {
            ticks: vec![tick(0, 200.0, 400.0), tick(1, 800.0, 1000.0)],
            ..Default::default()
        };
        let camera = Rect::from_center_size(pos2(900.0, 300.0), vec2(400.0, 300.0));
        assert_eq!(
            title_anchor(&graph, camera),
            Some((1, 800.0 - BLOCK_PADDING))
        );
    }

    #[test]
    fn the_camera_keeps_the_title_in_place_when_blocks_change_width() {
        let wider = TickGraph {
            ticks: vec![tick(5, 200.0, 900.0), tick(6, 1300.0, 1500.0)],
            ..Default::default()
        };
        let camera = Rect::from_center_size(pos2(900.0, 300.0), vec2(400.0, 300.0));
        let moved = follow_title(&wider, camera, (1, 800.0 - BLOCK_PADDING));
        assert_eq!(moved.center().x, 1400.0);
        assert_eq!(moved.size(), camera.size());
    }

    #[test]
    fn a_title_that_no_longer_exists_falls_back_to_the_last_one() {
        let shorter = TickGraph {
            ticks: vec![tick(5, 200.0, 400.0)],
            ..Default::default()
        };
        let camera = Rect::from_center_size(pos2(900.0, 300.0), vec2(400.0, 300.0));
        let moved = follow_title(&shorter, camera, (3, 1000.0));
        assert_eq!(moved.center().x, 900.0 + (200.0 - BLOCK_PADDING) - 1000.0);
    }
}
