use crate::graph::{
    CAPTION_HEIGHT, GRID_STEP, HEADER_HEIGHT, NODE_WIDTH, Node, NodeKey, ROW_HEIGHT,
    ROW_TITLE_WIDTH, TICK_HEADER_HEIGHT, TickGraph, Wire, input_anchor, output_anchor, row_center,
};
use eframe::egui::{
    self, Align2, Color32, FontId, Id, Painter, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    ecolor::Hsva, vec2,
};

const TICK_LINE: Color32 = Color32::from_rgb(70, 74, 84);
const NODE_FILL: Color32 = Color32::from_rgb(47, 47, 47);
const HEADER_FILL: Color32 = Color32::from_rgb(33, 33, 33);
const NODE_BORDER: Color32 = Color32::from_rgb(20, 20, 20);
const TEXT: Color32 = Color32::from_rgb(222, 222, 222);
const DIM_TEXT: Color32 = Color32::from_rgb(150, 150, 150);
const LABEL_MARGIN: f32 = 14.0;
const SOCKET_RADIUS: f32 = 4.5;
const FOCUS_BORDER: Color32 = Color32::from_rgb(255, 196, 64);
const WIRE_CORNER: f32 = 10.0;
const CORNER_STEPS: usize = 6;
const ROW_BAND: Color32 = Color32::from_rgba_premultiplied(12, 13, 15, 90);
const STICKY_ROW_HEIGHT: f32 = 40.0;
const ROW_BAND_MARGIN: f32 = 20.0;
const TICK_LINE_OFFSET: f32 = 60.0;

const CANVAS_FILL: Color32 = Color32::from_rgb(24, 25, 28);
const GRID_MINOR: Color32 = Color32::from_rgb(35, 37, 42);
const GRID_MAJOR: Color32 = Color32::from_rgb(50, 53, 60);
const GRID_MAJOR_EVERY: i64 = 5;
const MAX_GRID_LINES: f32 = 800.0;

fn signal_color(signal: u128) -> Color32 {
    Hsva::new((signal % 997) as f32 / 997.0, 0.6, 0.9, 1.0).into()
}

fn draw_grid(painter: &Painter, visible: Rect) {
    painter.rect_filled(visible, 0.0, CANVAS_FILL);

    if visible.width().max(visible.height()) / GRID_STEP > MAX_GRID_LINES {
        return;
    }
    let step = GRID_STEP;

    let draw_lines = |low: f32, high: f32, line: &mut dyn FnMut(f32, Stroke)| {
        let first = (low / step).floor() as i64;
        let last = (high / step).ceil() as i64;
        for index in first..=last {
            let color = if index.rem_euclid(GRID_MAJOR_EVERY) == 0 {
                GRID_MAJOR
            } else {
                GRID_MINOR
            };
            line(index as f32 * step, Stroke::new(1.0, color));
        }
    };

    draw_lines(visible.left(), visible.right(), &mut |x, stroke| {
        painter.line_segment(
            [
                egui::pos2(x, visible.top()),
                egui::pos2(x, visible.bottom()),
            ],
            stroke,
        );
    });
    draw_lines(visible.top(), visible.bottom(), &mut |y, stroke| {
        painter.line_segment(
            [
                egui::pos2(visible.left(), y),
                egui::pos2(visible.right(), y),
            ],
            stroke,
        );
    });
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
            path.push(start.lerp(at, t).lerp(at.lerp(end, t), t));
        }
    }
    path.push(points[points.len() - 1]);
    path
}

fn draw_wire(painter: &Painter, color: Color32, wire: &Wire, straight: bool) {
    let (first, last) = (wire.route[0], wire.route[wire.route.len() - 1]);
    let (path, label_at) = if straight {
        (vec![first, last], first.lerp(last, 0.5))
    } else {
        (rounded(&wire.route), wire.label_at)
    };
    let stroke = Stroke::new(2.0, color);
    if wire.windowed {
        painter.extend(Shape::dashed_line(&path, stroke, 8.0, 6.0));
    } else {
        painter.add(Shape::line(path, stroke));
    }

    painter.text(
        label_at - vec2(0.0, 8.0),
        Align2::CENTER_CENTER,
        &wire.label,
        FontId::proportional(12.0),
        color,
    );
}

fn draw_ports(
    painter: &Painter,
    position: Pos2,
    column: Rect,
    ports: &[crate::graph::Port],
    inputs: bool,
) {
    let clip = painter.with_clip_rect(column);
    let (align, x) = if inputs {
        (Align2::LEFT_CENTER, position.x + LABEL_MARGIN)
    } else {
        (Align2::RIGHT_CENTER, position.x + NODE_WIDTH - LABEL_MARGIN)
    };

    for (index, port) in ports.iter().enumerate() {
        let anchor = if inputs {
            input_anchor(position, index)
        } else {
            output_anchor(position, index)
        };
        painter.circle_filled(anchor, SOCKET_RADIUS, signal_color(port.signal));
        clip.text(
            egui::pos2(x, position.y + row_center(index)),
            align,
            format!("{} x{}", port.label, port.count),
            FontId::proportional(13.0),
            if port.external { DIM_TEXT } else { TEXT },
        );
    }
}

fn draw_node(painter: &Painter, node: &Node, position: Pos2, focused: bool) {
    let rect = Rect::from_min_size(position, node.size());
    let border = if focused {
        Stroke::new(3.0, FOCUS_BORDER)
    } else {
        Stroke::new(1.0, NODE_BORDER)
    };
    painter.rect(rect, 4.0, NODE_FILL, border, StrokeKind::Inside);

    let header = Rect::from_min_size(position, vec2(NODE_WIDTH, HEADER_HEIGHT));
    painter.rect_filled(header, 4.0, HEADER_FILL);
    painter.with_clip_rect(header.shrink(6.0)).text(
        header.left_center() + vec2(8.0, 0.0),
        Align2::LEFT_CENTER,
        &node.full_name,
        FontId::proportional(14.0),
        TEXT,
    );

    let body_top = position.y + HEADER_HEIGHT;
    let middle = position.x + NODE_WIDTH / 2.0;
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
        egui::pos2(position.x + LABEL_MARGIN, caption_y),
        Align2::LEFT_CENTER,
        "INPUTS",
        caption.clone(),
        DIM_TEXT,
    );
    painter.text(
        egui::pos2(position.x + NODE_WIDTH - LABEL_MARGIN, caption_y),
        Align2::RIGHT_CENTER,
        "OUTPUTS",
        caption,
        DIM_TEXT,
    );

    let columns_top = body_top + CAPTION_HEIGHT;
    let height = rect.bottom() - columns_top;
    let left = Rect::from_min_size(
        egui::pos2(position.x + 6.0, columns_top),
        vec2(NODE_WIDTH / 2.0 - 10.0, height.max(ROW_HEIGHT)),
    );
    let right = Rect::from_min_size(
        egui::pos2(middle + 4.0, columns_top),
        vec2(NODE_WIDTH / 2.0 - 10.0, height.max(ROW_HEIGHT)),
    );
    draw_ports(painter, position, left, &node.inputs, true);
    draw_ports(painter, position, right, &node.outputs, false);
}

fn draw_guides(painter: &Painter, graph: &TickGraph) {
    let (width, height) = (graph.extent.x, graph.extent.y);
    for (index, row) in graph.rows.iter().enumerate() {
        if index % 2 == 0 {
            let band = Rect::from_min_size(
                egui::pos2(0.0, row.top - ROW_BAND_MARGIN),
                vec2(width, row.height + ROW_BAND_MARGIN * 2.0),
            );
            painter.rect_filled(band, 0.0, ROW_BAND);
        }
    }
    for tick in &graph.ticks {
        painter.line_segment(
            [
                egui::pos2(tick.left - TICK_LINE_OFFSET, 0.0),
                egui::pos2(tick.left - TICK_LINE_OFFSET, height),
            ],
            Stroke::new(1.0, TICK_LINE),
        );
    }
}

fn draw_sticky_titles(painter: &Painter, graph: &TickGraph, visible: Rect) {
    let top = visible.top().max(0.0);
    for tick in &graph.ticks {
        let title = Rect::from_min_size(
            egui::pos2(tick.left, top),
            vec2(tick.right - tick.left, TICK_HEADER_HEIGHT),
        );
        painter.rect_filled(title, 3.0, HEADER_FILL);
        let text = match (tick.empty, tick.columns) {
            (true, _) => format!("Tick {} (no signals)", tick.tick),
            (false, 1) => format!("Tick {}", tick.tick),
            (false, chain) => format!("Tick {} (chain of {chain})", tick.tick),
        };
        painter.text(
            title.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::proportional(16.0),
            TEXT,
        );
    }

    let left = visible.left().max(0.0);
    for row in &graph.rows {
        let title = Rect::from_min_size(
            egui::pos2(left, row.top),
            vec2(ROW_TITLE_WIDTH, STICKY_ROW_HEIGHT),
        );
        painter.rect_filled(title, 3.0, HEADER_FILL);
        painter.with_clip_rect(title.shrink(4.0)).text(
            title.left_center() + vec2(10.0, 0.0),
            Align2::LEFT_CENTER,
            match row.number {
                Some(number) => format!("{number}  {}", row.name),
                None => row.name.clone(),
            },
            FontId::proportional(15.0),
            TEXT,
        );
    }
}

pub enum Click {
    Node(NodeKey),
    Background,
}

pub fn draw(
    ui: &mut egui::Ui,
    graph: &TickGraph,
    scene_rect: &mut Rect,
    straight: bool,
) -> Option<Click> {
    let output = egui::Scene::new()
        .zoom_range(0.05..=2.0)
        .show(ui, scene_rect, |ui| {
            if graph.extent.x > 0.0 && graph.extent.y > 0.0 {
                ui.allocate_rect(
                    Rect::from_min_size(Pos2::ZERO, graph.extent),
                    Sense::hover(),
                );
            }
            let painter = ui.painter().clone();
            let visible = ui.clip_rect();
            draw_grid(&painter, visible);
            draw_guides(&painter, graph);

            let mut clicked = None;
            for (index, node) in graph.nodes.iter().enumerate() {
                draw_node(&painter, node, node.position, graph.focus == Some(index));
                let key = (node.tick, node.system);
                let rect = Rect::from_min_size(node.position, node.size());
                let response = ui
                    .interact(rect, Id::unique(("node", key)), Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(&node.full_name);
                if response.clicked() {
                    clicked = Some(key);
                }
            }
            for wire in &graph.wires {
                draw_wire(&painter, signal_color(wire.signal), wire, straight);
            }
            draw_sticky_titles(&painter, graph, visible);
            clicked
        });

    match output.inner {
        Some(key) => Some(Click::Node(key)),
        None => output.response.clicked().then_some(Click::Background),
    }
}
