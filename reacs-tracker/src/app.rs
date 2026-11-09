use crate::canvas::{self, CanvasOutput, Click};
use crate::graph::{self, Focus, Hit, NodeKey, TickGraph};
use crate::hint::Hints;
use crate::source::{Event, Source};
use crate::trace::{self, Link, NodeLinks};
use eframe::egui::{
    self, Color32, Context, FontId, Key, KeyboardShortcut, Modifiers, Pos2, Rect, ViewportCommand,
};
use reacs::tracking::{TrackedFrame, TrackerMessage, TrackingSave};
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::mpsc::Receiver;
use std::time::Duration;

const MAX_TICKS_SHOWN: u64 = 32;
const HISTORY_LIMIT: usize = 200;
const NAVIGATION_BAR_HEIGHT: f32 = 14.0;
const READS_LOOKED_AHEAD: u64 = 64;
const DEFAULT_UPDATE_RATE: u32 = 10;
const PREVIOUS_TICK: &str = "<";
const NEXT_TICK: &str = ">";

type GraphKey = (u64, Option<u64>, u64, u64, Vec<Focus>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Waiting,
    Connected,
    Closed,
    Offline,
}

#[derive(Clone, PartialEq)]
struct ViewState {
    selected: Option<u64>,
    focus: Vec<Focus>,
}

struct NodeMenu {
    target: Hit,
    at: Pos2,
    links: NodeLinks,
    rect: Option<Rect>,
}

pub struct TrackerApp {
    description: String,
    events: Option<Receiver<Event>>,
    save: Option<TrackingSave>,
    status: Status,
    close_with_game: bool,
    interval: Option<u64>,
    selected: Option<u64>,
    follow: bool,
    ticks_shown: u64,
    scene_rect: Rect,
    version: u64,
    graph: Option<(GraphKey, TickGraph)>,
    static_mesh: canvas::StaticMesh,
    has_outside: bool,
    focus: Vec<Focus>,
    hints: Hints,
    menu: Option<NodeMenu>,
    center_on: Option<NodeKey>,
    update_rate: u32,
    shown_version: u64,
    last_update: f64,
    back: VecDeque<ViewState>,
    forward: Vec<ViewState>,
    frame_selection: bool,
}

fn view_range(save: &TrackingSave, interval: Option<u64>) -> Range<u64> {
    match interval.and_then(|start| save.intervals().get(&start)) {
        Some(interval) => interval.start_tick..interval.end_tick,
        None => save.tick_range(),
    }
}

fn visible_frames(
    save: &TrackingSave,
    interval: Option<u64>,
    ticks: Range<u64>,
) -> Vec<&TrackedFrame> {
    match interval.and_then(|start| save.intervals().get(&start)) {
        Some(interval) => {
            let start = interval.frames.partition_point(|x| x.tick < ticks.start);
            let end = interval.frames.partition_point(|x| x.tick < ticks.end);
            interval.frames[start..end].iter().collect()
        }
        None => save.frames_in(ticks).collect(),
    }
}

fn has_outside_sends(save: &TrackingSave) -> bool {
    let paused = save.intervals().values().flat_map(|x| x.frames.iter());
    save.frames().chain(paused).any(graph::sends_from_outside)
}

fn visible_ticks(range: &Range<u64>, selected: u64, shown: u64) -> Range<u64> {
    let end = (selected + 1).min(range.end);
    end.saturating_sub(shown).max(range.start)..end
}

fn tick_step(modifiers: Modifiers) -> u64 {
    match (modifiers.shift, modifiers.ctrl || modifiers.command) {
        (false, false) => 1,
        (true, false) => 5,
        (false, true) => 25,
        (true, true) => 100,
    }
}

fn selection_centering(tick: u64, shown: u64, range: &Range<u64>) -> u64 {
    (tick + shown / 2).min(range.end - 1)
}

fn navigation_thumb(track: Rect, view: Rect, bounds: Rect) -> Rect {
    let fraction = |x: f32| ((x - bounds.left()) / bounds.width()).clamp(0.0, 1.0);
    let x = |position: f32| track.left() + fraction(position) * track.width();
    Rect::from_min_max(
        Pos2::new(x(view.left()), track.top()),
        Pos2::new(x(view.right()), track.bottom()),
    )
}

fn menu_title(save: &TrackingSave, target: Hit) -> String {
    let signal = |id: u128| graph::short_name(save.signal_name(id)).to_string();
    match target {
        Hit::Node((tick, system)) => {
            format!("{} on tick {tick}", graph::sender_name(save, system))
        }
        Hit::Wire(wire) => match graph::age_text(wire.to.0 - wire.from.0) {
            Some(age) => format!("{}, {age}", signal(wire.signal)),
            None => signal(wire.signal),
        },
        Hit::Stub(stub) => format!("{}, off screen", signal(stub.signal)),
    }
}

fn link_list(ui: &mut egui::Ui, title: &str, links: &[Link], chosen: &mut Option<NodeKey>) {
    if links.is_empty() {
        return;
    }
    ui.weak(title);
    for link in links {
        let color = canvas::signal_color(link.signal);
        let text = egui::RichText::new(link.text.as_str()).color(color);
        let button = egui::Button::new(text)
            .frame_when_inactive(false)
            .frame(true);
        if ui.add_enabled(link.target.is_some(), button).clicked() {
            *chosen = link.target;
        }
    }
}

fn widest_link(ui: &egui::Ui, links: &NodeLinks) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let widest = links
        .inputs
        .iter()
        .chain(&links.outputs)
        .map(|link| {
            let text = link.text.clone();
            let galley = ui
                .painter()
                .layout_no_wrap(text, font.clone(), Color32::WHITE);
            galley.size().x
        })
        .fold(0.0, f32::max);
    widest + ui.spacing().button_padding.x * 2.0
}

fn take_tick_keys(input: &mut egui::InputState) -> Vec<(bool, bool, Modifiers)> {
    let mut keys = Vec::new();
    input.events.retain(|event| match event {
        egui::Event::Key {
            key: key @ (Key::ArrowLeft | Key::ArrowRight),
            pressed: true,
            repeat,
            modifiers,
            ..
        } => {
            keys.push((*key == Key::ArrowRight, *repeat, *modifiers));
            false
        }
        _ => true,
    });
    keys
}

fn stepped(selected: u64, step: u64, forward: bool, range: &Range<u64>) -> u64 {
    let moved = if forward {
        selected.saturating_add(step)
    } else {
        selected.saturating_sub(step)
    };
    moved.clamp(range.start, range.end - 1)
}

impl TrackerApp {
    pub fn new(
        source: Source,
        save: Option<TrackingSave>,
        close_with_game: bool,
        context: Context,
    ) -> Self {
        let status = if save.is_some() {
            Status::Offline
        } else {
            Status::Waiting
        };
        let has_outside = save.as_ref().is_some_and(has_outside_sends);
        Self {
            description: source.describe(),
            events: source.listen(context),
            save,
            status,
            close_with_game,
            interval: None,
            selected: None,
            follow: true,
            ticks_shown: 3,
            scene_rect: Rect::ZERO,
            version: 0,
            graph: None,
            static_mesh: canvas::StaticMesh::default(),
            has_outside,
            focus: Vec::new(),
            hints: Hints::default(),
            menu: None,
            center_on: None,
            update_rate: DEFAULT_UPDATE_RATE,
            shown_version: 0,
            last_update: f64::NEG_INFINITY,
            back: VecDeque::new(),
            forward: Vec::new(),
            frame_selection: false,
        }
    }

    fn apply(&mut self, context: &Context, event: Event) {
        match event {
            Event::Connected => self.status = Status::Connected,
            Event::Message(TrackerMessage::Full(save)) => {
                self.has_outside = has_outside_sends(&save);
                self.save = Some(save);
                self.version += 1;
            }
            Event::Message(TrackerMessage::Frame { tick, frame }) => {
                if let Some(save) = &mut self.save {
                    self.has_outside |= frame.as_ref().is_some_and(graph::sends_from_outside);
                    save.push_frame(tick, frame);
                    self.version += 1;
                }
            }
            Event::Closed(error) => {
                self.status = Status::Closed;
                if let Some(error) = error {
                    eprintln!("tracker connection failed: {error}");
                }
                if self.close_with_game {
                    context.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
    }

    fn drain(&mut self, context: &Context) {
        let Some(events) = self.events.take() else {
            return;
        };
        for event in events.try_iter() {
            self.apply(context, event);
        }
        self.events = Some(events);
    }

    fn update_period(&self) -> f64 {
        1.0 / self.update_rate as f64
    }

    fn take_update(&mut self, now: f64) -> bool {
        if self.version == self.shown_version || now - self.last_update < self.update_period() {
            return false;
        }
        self.shown_version = self.version;
        self.last_update = now;
        true
    }

    fn sync_selection(&mut self, now: f64) {
        let updated = self.take_update(now);
        let nothing_waiting = self.version == self.shown_version;
        let Some(save) = &self.save else {
            return;
        };
        let range = view_range(save, self.interval);
        if range.is_empty() {
            self.selected = None;
        } else if (self.follow && (updated || nothing_waiting)) || self.selected.is_none() {
            self.selected = Some(range.end - 1);
        } else {
            self.selected = self
                .selected
                .map(|tick| tick.clamp(range.start, range.end - 1));
        }

        let visible = self
            .selected
            .map(|selected| visible_ticks(&range, selected, self.ticks_shown));
        self.focus.retain(|focus| {
            let (first, last) = focus.ticks();
            visible
                .as_ref()
                .is_some_and(|x| x.contains(&first) && x.contains(&last))
        });
    }

    fn set_focus(&mut self, focus: Option<Focus>) {
        self.focus = focus.into_iter().collect();
    }

    fn toggle_focus(&mut self, target: Focus) {
        match self.focus.iter().position(|focus| *focus == target) {
            Some(index) => {
                self.focus.remove(index);
            }
            None => self.focus.push(target),
        }
    }

    fn view_state(&self) -> ViewState {
        ViewState {
            selected: self.selected,
            focus: self.focus.clone(),
        }
    }

    fn remember(&mut self) {
        let state = self.view_state();
        if self.back.back() != Some(&state) {
            self.back.push_back(state);
            if self.back.len() > HISTORY_LIMIT {
                self.back.pop_front();
            }
        }
        self.forward.clear();
    }

    fn restore(&mut self, state: ViewState) {
        self.selected = state.selected;
        self.frame_selection = !state.focus.is_empty();
        self.focus = state.focus;
        self.follow = false;
        self.menu = None;
        self.hints.close();
    }

    fn go_back(&mut self) {
        if let Some(state) = self.back.pop_back() {
            self.forward.push(self.view_state());
            self.restore(state);
        }
    }

    fn go_forward(&mut self) {
        if let Some(state) = self.forward.pop() {
            self.back.push_back(self.view_state());
            self.restore(state);
        }
    }

    fn move_tick(&mut self, step: u64, forward: bool) {
        let (Some(save), Some(selected)) = (&self.save, self.selected) else {
            return;
        };
        let range = view_range(save, self.interval);
        if range.is_empty() {
            return;
        }
        self.selected = Some(stepped(selected, step, forward, &range));
        self.follow = false;
        self.menu = None;
        self.hints.close();
    }

    fn fit_view(&mut self) {
        if !self.focus.is_empty() {
            self.frame_selection = true;
        } else {
            self.scene_rect = Rect::ZERO;
        }
        self.menu = None;
        self.hints.close();
    }

    fn apply_shortcuts(&mut self, ui: &egui::Ui) {
        let context = ui.ctx();
        if context.text_edit_focused() {
            return;
        }
        let forward = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
        let back = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
        let (go_forward, go_back, fit, tick_keys) = context.input_mut(|input| {
            (
                input.consume_shortcut(&forward),
                input.consume_shortcut(&back),
                input.consume_key(Modifiers::NONE, Key::F),
                take_tick_keys(input),
            )
        });
        if go_forward {
            self.go_forward();
        }
        if go_back {
            self.go_back();
        }
        if fit {
            self.fit_view();
        }
        for (forward, repeat, modifiers) in tick_keys {
            if !repeat {
                self.remember();
            }
            self.move_tick(tick_step(modifiers), forward);
        }
    }

    fn click(&mut self, click: Click) {
        match click {
            Click::On(target) if self.focus == [target] => {
                self.remember();
                self.set_focus(None);
            }
            Click::On(target) => {
                self.remember();
                self.follow = false;
                self.set_focus(Some(target));
            }
            Click::Toggle(target) => {
                self.remember();
                self.follow = false;
                self.toggle_focus(target);
            }
            Click::Menu(target, at) => self.open_menu(target, at),
            Click::Background => {
                if !self.focus.is_empty() {
                    self.remember();
                }
                self.set_focus(None);
            }
        }
    }

    fn open_menu(&mut self, target: Hit, at: Pos2) {
        let Some(save) = &self.save else {
            return;
        };
        let range = view_range(save, self.interval);
        let frames = visible_frames(save, self.interval, range.clone());
        let links = match target {
            Hit::Node(node) => trace::links_of(save, &frames, node),
            Hit::Wire(wire) => trace::links_of_wire(save, wire),
            Hit::Stub(stub) => {
                let selected = self.selected.unwrap_or(stub.node.0);
                let shown = visible_ticks(&range, selected, self.ticks_shown);
                trace::links_of_stub(save, &frames, stub, &shown)
            }
        };
        self.menu = Some(NodeMenu {
            target,
            at,
            links,
            rect: None,
        });
    }

    fn jump_to(&mut self, target: NodeKey) {
        self.remember();
        let Some(save) = &self.save else {
            return;
        };
        let range = view_range(save, self.interval);
        let (tick, _) = target;
        let on_screen = self.selected.is_some_and(|selected| {
            visible_ticks(&range, selected, self.ticks_shown).contains(&tick)
        });
        if !on_screen {
            self.selected = Some(selection_centering(tick, self.ticks_shown, &range));
        }
        self.follow = false;
        self.menu = None;
        self.center_on = Some(target);
        self.set_focus(Some(Focus::Node(target)));
    }

    fn center_camera(&mut self) {
        let Some(target) = self.center_on.take() else {
            return;
        };
        let Some((_, built)) = &self.graph else {
            return;
        };
        let node = built
            .nodes
            .iter()
            .find(|node| (node.tick, node.system) == target);
        if let (Some(node), true) = (node, self.scene_rect.is_positive()) {
            let center = Rect::from_min_size(node.position, node.size()).center();
            self.scene_rect = self.scene_rect.translate(center - self.scene_rect.center());
        }
    }

    fn draw_menu(&mut self, ui: &egui::Ui) {
        let (Some(menu), Some(save)) = (&mut self.menu, &self.save) else {
            return;
        };
        let title = menu_title(save, menu.target);
        let links = &menu.links;
        let mut chosen = None;
        let screen = ui.ctx().content_rect().size();
        let area = egui::Area::new(egui::Id::unique("node menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.at)
            .constrain(true)
            .default_size(screen)
            .sizing_pass(menu.rect.is_none())
            .show(ui.ctx(), |ui| {
                let frame = egui::Frame::popup(ui.style());
                let spacing = ui.spacing();
                let title_row = spacing.interact_size.y + spacing.item_spacing.y;
                let room = screen - frame.total_margin().sum() - egui::vec2(0.0, title_row);
                frame.show(ui, |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
                    ui.strong(title);
                    egui::ScrollArea::new([widest_link(ui, links) > room.x, true])
                        .max_height(room.y)
                        .max_width(room.x)
                        .show(ui, |ui| {
                            link_list(ui, "Jump to input", &links.inputs, &mut chosen);
                            if !links.inputs.is_empty() && !links.outputs.is_empty() {
                                ui.separator();
                            }
                            link_list(ui, "Jump to output", &links.outputs, &mut chosen);
                        });
                });
            });
        menu.rect = Some(area.response.rect);

        if let Some(target) = chosen {
            self.jump_to(target);
            ui.ctx().request_repaint();
        }
    }

    fn apply_click(&mut self, ui: &egui::Ui, click: Option<Click>) {
        let was_open = self.menu.is_some();
        let (released, pointer) =
            ui.input(|input| (input.pointer.any_released(), input.pointer.latest_pos()));
        let inside = self
            .menu
            .as_ref()
            .and_then(|menu| menu.rect)
            .zip(pointer)
            .is_some_and(|(rect, pointer)| rect.contains(pointer));
        if was_open && released && !inside {
            self.menu = None;
        }

        match click {
            Some(click @ Click::Menu(..)) => self.click(click),
            Some(click) if !was_open => self.click(click),
            _ => {}
        }
    }

    fn draw_summary(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(format!("{} ({:?})", self.description, self.status));
            ui.separator();
            let Some(save) = &self.save else {
                ui.label("waiting for the game to start tracking");
                return;
            };

            let range = view_range(save, self.interval);
            ui.label(format!(
                "ticks {}..{} of a {} tick window",
                range.start,
                range.end,
                save.window_ticks()
            ));
            ui.separator();

            let intervals: Vec<(u64, u64)> = save
                .intervals()
                .values()
                .map(|x| (x.start_tick, x.end_tick))
                .collect();
            let current = match self.interval {
                Some(start) => format!("paused interval at {start}"),
                None => "current recording".to_string(),
            };
            let mut choice = self.interval;
            egui::ComboBox::from_id_salt("interval")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut choice, None, "current recording");
                    for (start, end) in intervals {
                        ui.selectable_value(&mut choice, Some(start), format!("{start}..{end}"));
                    }
                });
            if choice != self.interval {
                self.interval = choice;
                self.selected = None;
                self.follow = false;
                self.focus.clear();
            }

            ui.separator();
            ui.label("ticks shown");
            ui.add(egui::DragValue::new(&mut self.ticks_shown).range(1..=MAX_TICKS_SHOWN));
            if ui
                .add_enabled(!self.back.is_empty(), egui::Button::new("back"))
                .on_hover_text("mouse button 4 or Ctrl+Z")
                .clicked()
            {
                self.go_back();
            }
            if ui
                .add_enabled(!self.forward.is_empty(), egui::Button::new("forward"))
                .on_hover_text("mouse button 5 or Ctrl+Shift+Z")
                .clicked()
            {
                self.go_forward();
            }
            ui.checkbox(&mut self.follow, "follow latest tick");
            if self.status != Status::Offline {
                ui.label("updates/s");
                ui.add(egui::DragValue::new(&mut self.update_rate).range(1..=60));
            }
            if ui
                .button("fit view")
                .on_hover_text("F: the selection, or everything when nothing is selected")
                .clicked()
            {
                self.fit_view();
            }
            ui.separator();
            ui.label("solid: one-tick signal, dashed: window");
        });

        let Some(save) = &self.save else {
            return;
        };
        let range = view_range(save, self.interval);
        if let (false, Some(mut tick)) = (range.is_empty(), self.selected) {
            ui.horizontal(|ui| {
                ui.label("last tick shown");
                let step = tick_step(ui.input(|input| input.modifiers));
                let explain = "or the Left and Right keys. Shift: 5 ticks, Ctrl: 25, both: 100";
                if ui.button(PREVIOUS_TICK).on_hover_text(explain).clicked() {
                    self.remember();
                    self.move_tick(step, false);
                }
                let slider = egui::Slider::new(&mut tick, range.start..=range.end - 1);
                let slider = ui.add(slider);
                if slider.drag_started() || (slider.changed() && !slider.dragged()) {
                    self.remember();
                }
                if slider.changed() {
                    self.selected = Some(tick);
                    self.follow = false;
                }
                if ui.button(NEXT_TICK).on_hover_text(explain).clicked() {
                    self.remember();
                    self.move_tick(step, true);
                }
            });
        }
    }

    fn focus_text(&self) -> Option<String> {
        let save = self.save.as_ref()?;
        let focus = match self.focus.as_slice() {
            [] => return None,
            [one] => *one,
            many => return Some(format!("Isolated the chains of {} selections", many.len())),
        };
        let system = |id: u128| graph::short_name(save.system_name(id));
        Some(match focus {
            Focus::Node((tick, id)) => format!("Isolated chain of {} on tick {tick}", system(id)),
            Focus::Wire(wire) => format!(
                "Isolated {} from {} (tick {}) to {} (tick {})",
                graph::short_name(save.signal_name(wire.signal)),
                system(wire.from.1),
                wire.from.0,
                system(wire.to.1),
                wire.to.0
            ),
            Focus::Tick(tick) => format!("Isolated everything on tick {tick}"),
        })
    }

    fn draw_focus_bar(&mut self, ui: &egui::Ui, area: Rect) {
        let Some(text) = self.focus_text() else {
            return;
        };
        let mut clear = false;
        egui::Area::new(egui::Id::unique("focus bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(area.left_top() + egui::vec2(8.0, 8.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong(text);
                        clear = ui.button("show all").clicked();
                        ui.label("click it again, the background or press Esc to leave, Shift+click to add or remove");
                    });
                });
            });
        if clear {
            self.set_focus(None);
        }
    }

    fn draw_navigation_bar(&mut self, ui: &mut egui::Ui) {
        let size = egui::vec2(ui.available_width(), NAVIGATION_BAR_HEIGHT);
        let (track, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let bounds = self
            .graph
            .as_ref()
            .and_then(|(_, built)| canvas::camera_bounds(built));
        let view = self.scene_rect;
        let Some(bounds) = bounds.filter(|_| view.is_positive()) else {
            return;
        };

        let thumb = navigation_thumb(track, view, bounds);
        let visuals = ui.visuals();
        let fill = if response.hovered() || response.dragged() {
            visuals.widgets.hovered.bg_fill
        } else {
            visuals.widgets.inactive.bg_fill
        };
        ui.painter()
            .rect_filled(track, 4.0, visuals.extreme_bg_color);
        ui.painter().rect_filled(thumb, 4.0, fill);

        let per_point = bounds.width() / track.width();
        let mut center = view.center().x;
        if response.clicked() || response.drag_started() {
            if let Some(pointer) = response.interact_pointer_pos() {
                if !thumb.contains(pointer) {
                    center = bounds.left() + (pointer.x - track.left()) * per_point;
                }
            }
        }
        if response.dragged() {
            center += response.drag_delta().x * per_point;
        }
        let pointer = ui.input(|input| input.pointer.latest_pos());
        let over_popup = pointer
            .and_then(|pointer| ui.ctx().layer_id_at(pointer))
            .is_some_and(|layer| layer.order != egui::Order::Background);
        if !over_popup {
            let scroll = ui
                .ctx()
                .input_mut(|input| std::mem::take(&mut input.smooth_scroll_delta));
            let wheel = if scroll.x != 0.0 { scroll.x } else { scroll.y };
            center -= wheel * per_point;
        }

        if center != view.center().x {
            let moved = view.translate(egui::vec2(center - view.center().x, 0.0));
            self.scene_rect = canvas::limit_camera(moved, bounds);
            self.hints.close();
            self.menu = None;
        }
    }

    fn draw_graph(&mut self, ui: &mut egui::Ui) {
        let (Some(save), Some(selected)) = (&self.save, self.selected) else {
            ui.centered_and_justified(|ui| ui.label("nothing recorded yet"));
            return;
        };

        let key = (
            self.shown_version,
            self.interval,
            selected,
            self.ticks_shown,
            self.focus.clone(),
        );
        let without_focus = |key: &GraphKey| (key.0, key.1, key.2, key.3);
        let rebuilt = self.graph.as_ref().is_none_or(|(cached, _)| *cached != key);
        let ticks_changed = self
            .graph
            .as_ref()
            .is_none_or(|(cached, _)| without_focus(cached) != without_focus(&key));
        if rebuilt {
            let keeps_camera = self.scene_rect.is_positive() && self.center_on.is_none();
            let anchor = self
                .graph
                .as_ref()
                .filter(|_| keeps_camera)
                .and_then(|(_, old)| canvas::title_anchor(old, self.scene_rect));
            let range = view_range(save, self.interval);
            let ticks = visible_ticks(&range, selected, self.ticks_shown);
            let frames = visible_frames(save, self.interval, ticks.clone());
            let lookahead = ticks.end..(ticks.end + READS_LOOKED_AHEAD).min(range.end);
            let later = visible_frames(save, self.interval, lookahead);
            let built = graph::build(
                save,
                &frames,
                &later,
                self.has_outside,
                ticks,
                &self.focus,
                &|text: &str, size: f32| {
                    let font = FontId::proportional(size);
                    let painter = ui.painter();
                    let galley = painter.layout_no_wrap(text.to_string(), font, Color32::WHITE);
                    galley.size().x
                },
            );
            if let Some(anchor) = anchor {
                self.scene_rect = canvas::follow_title(&built, self.scene_rect, anchor);
            }
            self.graph = Some((key, built));
            self.static_mesh = canvas::StaticMesh::default();
        }
        if !self.scene_rect.is_positive() {
            let bounds = self
                .graph
                .as_ref()
                .and_then(|(_, built)| canvas::camera_bounds(built));
            if let Some(bounds) = bounds {
                self.scene_rect = canvas::fit_view(ui.available_rect_before_wrap(), bounds);
            }
        }
        let jumping = self.center_on.is_some();
        self.center_camera();
        if let Some((_, built)) = self.graph.as_ref().filter(|_| ticks_changed || jumping) {
            let screen = ui.available_rect_before_wrap();
            self.scene_rect = canvas::keep_on_graph(screen, self.scene_rect, built, self.follow);
        }
        if std::mem::take(&mut self.frame_selection) {
            let screen = ui.available_rect_before_wrap();
            let framed = self
                .graph
                .as_ref()
                .and_then(|(_, built)| canvas::focus_view(screen, built, &self.focus));
            self.scene_rect = framed.unwrap_or(self.scene_rect);
        }
        let camera = self.scene_rect;
        let output = self
            .graph
            .as_ref()
            .map(|(_, built)| canvas::draw(ui, built, &mut self.static_mesh, &mut self.scene_rect));
        let Some(CanvasOutput { click, hovered }) = output else {
            return;
        };
        if self.scene_rect != camera {
            self.hints.close();
            self.menu = None;
        }
        self.apply_click(ui, click);

        let (now, pointer, pressed) = ui.input(|input| {
            (
                input.time,
                input.pointer.hover_pos(),
                input.pointer.any_down() || input.pointer.any_released(),
            )
        });
        let hovered = hovered.filter(|_| self.menu.is_none());
        self.hints.update(now, pointer, pressed, hovered);
        self.hints.show(ui.ctx());
        self.draw_menu(ui);
    }

    fn draw(&mut self, ui: &mut egui::Ui) -> Rect {
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) && self.menu.take().is_none() {
            self.set_focus(None);
        }
        let (go_back, go_forward) = ui.input(|input| {
            (
                input.pointer.button_pressed(egui::PointerButton::Extra1),
                input.pointer.button_pressed(egui::PointerButton::Extra2),
            )
        });
        if go_back {
            self.go_back();
        }
        if go_forward {
            self.go_forward();
        }
        self.apply_shortcuts(ui);

        let now = ui.input(|input| input.time);
        self.sync_selection(now);
        if matches!(self.status, Status::Waiting | Status::Connected) {
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(self.update_period()));
        }
        egui::Panel::top("summary").show(ui, |ui| self.draw_summary(ui));
        egui::Panel::bottom("navigation").show(ui, |ui| self.draw_navigation_bar(ui));
        egui::CentralPanel::default()
            .show(ui, |ui| {
                let area = ui.max_rect();
                self.draw_graph(ui);
                self.draw_focus_bar(ui, area);
                area
            })
            .inner
    }
}

impl eframe::App for TrackerApp {
    fn logic(&mut self, context: &Context, _frame: &mut eframe::Frame) {
        self.drain(context);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        NEXT_TICK, NodeMenu, PREVIOUS_TICK, Status, TrackerApp, navigation_thumb, stepped,
        tick_step, view_range, visible_frames, visible_ticks,
    };
    use crate::canvas::{Click, limit_camera};
    use crate::graph::{Focus, Hit};
    use crate::source::{Event, Source};
    use crate::test_support::{recorded, recorded_paused};
    use crate::trace::{Link, NodeLinks};
    use eframe::egui::{
        self, Context, Event as Event2, FontId, Key, Modifiers, PointerButton, Pos2, RawInput,
        Rect, pos2, vec2,
    };
    use reacs::prelude::*;
    use reacs::tracking::{TrackerMessage, TrackingSave};
    use std::time::{Duration, Instant};

    fn app(save: Option<TrackingSave>, close_with_game: bool) -> TrackerApp {
        TrackerApp::new(
            Source::File("run.bin".into()),
            save,
            close_with_game,
            Context::default(),
        )
    }

    fn draw(app: &mut TrackerApp) {
        let context = Context::default();
        for _ in 0..2 {
            let mut output = context.run_ui(RawInput::default(), |ui| {
                app.draw(ui);
            });
            output.textures_delta.clear();
        }
    }

    #[test]
    fn the_visible_ticks_end_at_the_selected_one() {
        assert_eq!(visible_ticks(&(0..10), 5, 3), 3..6);
        assert_eq!(visible_ticks(&(4..10), 5, 3), 4..6);
        assert_eq!(visible_ticks(&(0..10), 99, 3), 7..10);
    }

    #[test]
    fn a_paused_interval_can_be_viewed() {
        let save = recorded_paused(4, Some(1));
        assert_eq!(view_range(&save, Some(0)), 0..2);
        assert_eq!(view_range(&save, None), 2..4);
    }

    #[test]
    fn only_the_visible_frames_are_read() {
        let save = recorded_paused(6, Some(2));
        let ticks = |interval, range| -> Vec<u64> {
            visible_frames(&save, interval, range)
                .iter()
                .map(|x| x.tick)
                .collect()
        };
        assert_eq!(ticks(None, 4..5), [4]);
        assert_eq!(ticks(Some(0), 2..3), [2]);
    }

    #[test]
    fn the_window_draws_every_state() {
        let mut empty = app(None, false);
        draw(&mut empty);

        let mut filled = app(Some(recorded_paused(4, Some(1))), false);
        assert_eq!(filled.status, Status::Offline);
        draw(&mut filled);
        assert_eq!(filled.selected, Some(3));

        filled.follow = false;
        filled.interval = Some(0);
        filled.selected = Some(99);
        draw(&mut filled);
        assert_eq!(filled.selected, Some(1));
    }

    fn frame(context: &Context, app: &mut TrackerApp, events: Vec<Event2>) -> Rect {
        frame_at(context, app, events, None)
    }

    fn frame_at(
        context: &Context,
        app: &mut TrackerApp,
        events: Vec<Event2>,
        time: Option<f64>,
    ) -> Rect {
        let input = RawInput {
            time,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
            events,
            ..Default::default()
        };
        let mut area = Rect::ZERO;
        let mut output = context.run_ui(input, |ui| area = app.draw(ui));
        output.textures_delta.clear();
        area
    }

    fn aim(context: &Context, window: &mut TrackerApp, area: Rect, point: Pos2) -> Pos2 {
        window.scene_rect = Rect::from_center_size(point, area.size());
        frame(context, window, Vec::new());
        let view = window.scene_rect;
        let scale = (area.width() / view.width())
            .min(area.height() / view.height())
            .min(2.0);
        area.center() + (point - view.center()) * scale
    }

    fn click_at(context: &Context, app: &mut TrackerApp, position: Pos2) {
        press_at(context, app, position, PointerButton::Primary);
    }

    fn press_at(context: &Context, app: &mut TrackerApp, position: Pos2, which: PointerButton) {
        let button = |pressed| Event2::PointerButton {
            pos: position,
            button: which,
            pressed,
            modifiers: Default::default(),
        };
        frame(context, app, vec![Event2::PointerMoved(position)]);
        frame(context, app, vec![Event2::PointerMoved(position)]);
        frame(context, app, vec![button(true)]);
        frame(context, app, vec![button(false)]);
        frame(context, app, Vec::new());
    }

    #[test]
    fn the_isolation_bar_floats_over_the_graph() {
        let save = recorded(4);
        let system = save.system_order()[1];
        let mut window = app(Some(save), false);
        let context = Context::default();

        frame(&context, &mut window, Vec::new());
        let area = frame(&context, &mut window, Vec::new());
        window.click(Click::On(Focus::Node((3, system))));
        frame(&context, &mut window, Vec::new());
        assert_eq!(frame(&context, &mut window, Vec::new()), area);
    }

    #[test]
    fn clicking_a_wire_in_the_window_focuses_it() {
        let save = recorded(4);
        let mut window = app(Some(save), false);
        window.follow = false;
        let context = Context::default();
        frame(&context, &mut window, Vec::new());
        let area = frame(&context, &mut window, Vec::new());

        let (point, key) = {
            let (_, graph) = window.graph.as_ref().unwrap();
            let wire = graph.wires.iter().find(|x| x.route.len() == 6).unwrap();
            (wire.label_at, graph.wire_key(wire))
        };
        let screen = aim(&context, &mut window, area, point);
        click_at(&context, &mut window, screen);
        assert_eq!(window.focus, [Focus::Wire(key)]);

        click_at(&context, &mut window, screen);
        assert!(window.focus.is_empty());
    }

    #[test]
    fn clicking_a_tick_title_selects_the_whole_tick() {
        let save = recorded(4);
        let mut window = app(Some(save), false);
        window.follow = false;
        let context = Context::default();
        frame(&context, &mut window, Vec::new());
        let area = frame(&context, &mut window, Vec::new());

        let (tick, point) = {
            let (_, graph) = window.graph.as_ref().unwrap();
            let column = graph.ticks.iter().find(|tick| !tick.empty).unwrap();
            let middle = (column.left + column.right) / 2.0;
            (column.tick, pos2(middle, column.top + 20.0))
        };
        let screen = aim(&context, &mut window, area, point);
        click_at(&context, &mut window, screen);
        assert_eq!(window.focus, [Focus::Tick(tick)]);

        click_at(&context, &mut window, screen);
        assert!(window.focus.is_empty());
    }

    #[test]
    fn a_tooltip_freezes_while_the_mouse_is_over_it() {
        let save = recorded(4);
        let mut window = app(Some(save), false);
        window.follow = false;
        let context = Context::default();
        frame_at(&context, &mut window, Vec::new(), Some(0.0));
        let area = frame_at(&context, &mut window, Vec::new(), Some(0.01));

        let point = {
            let (_, graph) = window.graph.as_ref().unwrap();
            graph
                .wires
                .iter()
                .find(|x| x.route.len() == 6)
                .unwrap()
                .label_at
        };
        let screen = aim(&context, &mut window, area, point);
        let moved = |position| vec![Event2::PointerMoved(position)];
        for time in [0.1, 0.2, 0.3, 0.6, 0.7] {
            frame_at(&context, &mut window, moved(screen), Some(time));
        }
        let rect = window.hints.rect().expect("the tooltip opened");
        assert!(window.hints.text().unwrap().contains("from"));

        for time in [0.8, 3.0, 9.0] {
            frame_at(&context, &mut window, moved(rect.center()), Some(time));
            assert_eq!(window.hints.rect(), Some(rect));
        }

        let away = area.left_top() + vec2(4.0, 4.0);
        frame_at(&context, &mut window, moved(away), Some(9.1));
        assert!(window.hints.rect().is_some());
        frame_at(&context, &mut window, moved(away), Some(30.0));
        assert!(window.hints.rect().is_none());
    }

    #[test]
    fn right_clicking_a_node_opens_its_menu_and_clicking_away_only_closes_it() {
        let save = recorded(4);
        let mut window = app(Some(save), false);
        window.follow = false;
        let context = Context::default();
        frame(&context, &mut window, Vec::new());
        let area = frame(&context, &mut window, Vec::new());

        let (key, center) = {
            let (_, graph) = window.graph.as_ref().unwrap();
            let node = graph.nodes.last().unwrap();
            let rect = Rect::from_min_size(node.position, node.size());
            ((node.tick, node.system), rect.center())
        };
        let screen = aim(&context, &mut window, area, center);
        click_at(&context, &mut window, screen);
        assert_eq!(window.focus, [Focus::Node(key)]);

        press_at(&context, &mut window, screen, PointerButton::Secondary);
        assert_eq!(
            window.menu.as_ref().map(|menu| menu.target),
            Some(Hit::Node(key))
        );

        let empty = area.left_top() + vec2(5.0, 5.0);
        click_at(&context, &mut window, empty);
        assert!(window.menu.is_none());
        assert_eq!(window.focus, [Focus::Node(key)]);

        click_at(&context, &mut window, empty);
        assert!(window.focus.is_empty());
    }

    #[test]
    fn jumping_to_an_off_screen_tick_moves_the_view_there() {
        let save = recorded(8);
        let system = save.system_order()[1];
        let mut window = app(Some(save), false);
        window.follow = false;
        window.selected = Some(7);

        window.jump_to((1, system));
        assert_eq!(window.selected, Some(2));
        assert_eq!(window.focus, [Focus::Node((1, system))]);
        assert!(!window.follow);
    }

    #[test]
    fn jumping_to_a_tick_on_screen_keeps_the_ticks_shown() {
        let save = recorded(8);
        let system = save.system_order()[1];
        let mut window = app(Some(save), false);
        window.follow = false;
        window.selected = Some(7);

        window.jump_to((6, system));
        assert_eq!(window.selected, Some(7));
    }

    #[test]
    fn a_jump_centers_the_camera_on_its_node_and_keeps_the_zoom() {
        let save = recorded(4);
        let mut window = app(Some(save), false);
        window.follow = false;
        let context = Context::default();
        frame(&context, &mut window, Vec::new());
        let area = frame(&context, &mut window, Vec::new());

        let key = {
            let (_, graph) = window.graph.as_ref().unwrap();
            let node = graph.nodes.first().unwrap();
            (node.tick, node.system)
        };
        let zoom = |view: Rect| (area.size() / view.size()).min_elem();
        let before = zoom(window.scene_rect);
        window.jump_to(key);
        frame(&context, &mut window, Vec::new());

        let (_, graph) = window.graph.as_ref().unwrap();
        let node = graph
            .nodes
            .iter()
            .find(|node| (node.tick, node.system) == key)
            .unwrap();
        let center = Rect::from_min_size(node.position, node.size()).center();
        assert!(window.scene_rect.contains(center));
        assert!((zoom(window.scene_rect) - before).abs() < 0.01);
    }

    #[test]
    fn jumps_near_the_end_stay_inside_the_recording() {
        assert_eq!(super::selection_centering(9, 3, &(0..10)), 9);
        assert_eq!(super::selection_centering(4, 3, &(0..10)), 5);
        assert_eq!(super::selection_centering(4, 8, &(0..6)), 5);
    }

    #[test]
    fn selecting_keeps_the_camera() {
        let save = recorded(4);
        let system = save.system_order()[1];
        let mut window = app(Some(save), false);
        draw(&mut window);
        let camera = eframe::egui::Rect::from_min_size(pos2(10.0, 10.0), vec2(50.0, 50.0));
        window.scene_rect = camera;

        window.click(Click::On(Focus::Node((3, system))));
        draw(&mut window);
        assert_eq!(window.scene_rect, camera);
    }

    fn wire_under_the_pointer(
        context: &Context,
        window: &mut TrackerApp,
        area: Rect,
    ) -> (crate::graph::WireKey, Pos2) {
        let (point, key) = {
            let (_, graph) = window.graph.as_ref().unwrap();
            let wire = graph.wires.iter().find(|x| x.route.len() == 6).unwrap();
            (wire.label_at, graph.wire_key(wire))
        };
        (key, aim(context, window, area, point))
    }

    fn opened_tooltip(window: &mut TrackerApp, context: &Context, area: Rect) -> f64 {
        let (_, screen) = wire_under_the_pointer(context, window, area);
        let moved = || vec![Event2::PointerMoved(screen)];
        for time in [0.1, 0.2, 0.3, 0.6, 0.7] {
            frame_at(context, window, moved(), Some(time));
        }
        assert!(window.hints.rect().is_some(), "the tooltip did not open");
        0.7
    }

    fn window_ready(context: &Context) -> (TrackerApp, Rect) {
        let mut window = app(Some(recorded(4)), false);
        window.follow = false;
        frame_at(context, &mut window, Vec::new(), Some(0.0));
        let area = frame_at(context, &mut window, Vec::new(), Some(0.01));
        (window, area)
    }

    #[test]
    fn right_clicking_a_wire_opens_a_menu_with_both_ends() {
        let context = Context::default();
        let (mut window, area) = window_ready(&context);
        let (key, screen) = wire_under_the_pointer(&context, &mut window, area);

        press_at(&context, &mut window, screen, PointerButton::Secondary);
        let menu = window.menu.as_ref().expect("the menu opened");
        assert_eq!(menu.target, Hit::Wire(key));
        assert_eq!(menu.links.inputs[0].target, Some(key.from));
        assert_eq!(menu.links.outputs[0].target, Some(key.to));
    }

    #[test]
    fn a_triple_click_selects_all_of_a_tooltip_and_it_can_be_copied() {
        let context = Context::default();
        let (mut window, area) = window_ready(&context);
        let start = opened_tooltip(&mut window, &context, area);
        let text = window.hints.text().unwrap().to_string();
        assert!(text.contains('\n'));
        let inside = window.hints.rect().unwrap().center();

        let mut time = start;
        let mut step = |window: &mut TrackerApp, events: Vec<Event2>| {
            time += 0.02;
            frame_at(&context, window, events, Some(time));
        };
        step(&mut window, vec![Event2::PointerMoved(inside)]);
        step(&mut window, vec![Event2::PointerMoved(inside)]);
        let button = |pressed| Event2::PointerButton {
            pos: inside,
            button: PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        for _ in 0..3 {
            step(&mut window, vec![button(true)]);
            step(&mut window, vec![button(false)]);
        }
        step(&mut window, Vec::new());
        assert!(window.hints.selects_everything(&context));

        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0))),
            events: vec![Event2::Copy],
            time: Some(start + 1.0),
            ..Default::default()
        };
        let mut output = context.run_ui(input, |ui| {
            window.draw(ui);
        });
        output.textures_delta.clear();
        assert!(
            output
                .platform_output
                .commands
                .contains(&eframe::egui::OutputCommand::CopyText(text))
        );
    }

    #[test]
    fn moving_the_camera_closes_tooltips_and_menus() {
        let context = Context::default();
        let (mut window, area) = window_ready(&context);
        let start = opened_tooltip(&mut window, &context, area);
        let camera = window.scene_rect;

        let wheel = vec![Event2::MouseWheel {
            unit: eframe::egui::MouseWheelUnit::Point,
            delta: vec2(-80.0, 0.0),
            phase: eframe::egui::TouchPhase::Move,
            source: eframe::egui::MouseWheelSource::Wheel,
            modifiers: Default::default(),
        }];
        frame_at(&context, &mut window, wheel, Some(start + 0.02));
        frame_at(&context, &mut window, Vec::new(), Some(start + 0.04));
        frame_at(&context, &mut window, Vec::new(), Some(start + 0.06));

        assert_ne!(window.scene_rect, camera);
        assert!(window.hints.rect().is_none());
    }

    #[test]
    fn the_camera_cannot_leave_the_graph() {
        let bounds = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0));
        let size = vec2(200.0, 100.0);

        let past = Rect::from_center_size(pos2(-500.0, 5000.0), size);
        assert_eq!(
            limit_camera(past, bounds),
            Rect::from_center_size(pos2(100.0, 750.0), size)
        );

        let inside = Rect::from_center_size(pos2(400.0, 300.0), size);
        assert_eq!(limit_camera(inside, bounds), inside);
    }

    #[test]
    fn zooming_out_stops_when_the_graph_fits() {
        let bounds = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 200.0));
        let huge = Rect::from_center_size(pos2(0.0, 0.0), vec2(4000.0, 2000.0));
        let limited = limit_camera(huge, bounds);

        assert_eq!(limited.width(), 1000.0);
        assert_eq!(limited.height(), 500.0);
        assert_eq!(limited.center().x, bounds.center().x);
    }

    #[test]
    fn a_view_taller_than_the_graph_still_moves_until_the_graph_touches_its_edge() {
        let bounds = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 200.0));
        let size = vec2(400.0, 500.0);
        let at = |y: f32| limit_camera(Rect::from_center_size(pos2(500.0, y), size), bounds);

        assert_eq!(at(100.0).center().y, 100.0);
        assert_eq!(at(180.0).center().y, 180.0);
        assert_eq!(at(9000.0).top(), bounds.top());
        assert_eq!(at(-9000.0).bottom(), bounds.bottom());
    }

    #[test]
    fn panning_past_the_edge_stops_at_the_limit_in_the_window() {
        let context = Context::default();
        let (mut window, area) = window_ready(&context);
        window.scene_rect = Rect::from_center_size(pos2(-9000.0, -9000.0), area.size());
        frame(&context, &mut window, Vec::new());

        let bounds = crate::canvas::camera_bounds(&window.graph.as_ref().unwrap().1).unwrap();
        let view = window.scene_rect;
        let nested = |inner: Rect, outer: Rect, axis: usize| {
            inner.min[axis] >= outer.min[axis] - 0.01 && inner.max[axis] <= outer.max[axis] + 0.01
        };
        for axis in 0..2 {
            assert!(nested(view, bounds, axis) || nested(bounds, view, axis));
        }
    }

    fn assert_inside_limits(window: &TrackerApp) {
        let bounds = crate::canvas::camera_bounds(&window.graph.as_ref().unwrap().1).unwrap();
        let view = window.scene_rect;
        let nested = |inner: Rect, outer: Rect, axis: usize| {
            inner.min[axis] >= outer.min[axis] - 0.01 && inner.max[axis] <= outer.max[axis] + 0.01
        };
        for axis in 0..2 {
            assert!(nested(view, bounds, axis) || nested(bounds, view, axis));
        }
    }

    fn zoomed_in_at_the_far_corner(window: &mut TrackerApp, area: Rect) {
        let (_, graph) = window.graph.as_ref().unwrap();
        let far = pos2(graph.extent.x + 5000.0, graph.extent.y + 5000.0);
        window.scene_rect = Rect::from_center_size(far, area.size() * 0.5);
    }

    #[test]
    fn changing_ticks_stays_inside_the_limits() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        zoomed_in_at_the_far_corner(&mut window, area);
        window.selected = Some(5);
        frame(&context, &mut window, Vec::new());
        assert_inside_limits(&window);
    }

    fn zoomed_out_past_the_graph(window: &mut TrackerApp, area: Rect) {
        let (_, graph) = window.graph.as_ref().unwrap();
        let center = crate::canvas::camera_bounds(graph).unwrap().center();
        window.scene_rect = Rect::from_center_size(center, area.size() * 20.0);
    }

    #[test]
    fn changing_ticks_applies_the_zoom_limit_at_once() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        zoomed_out_past_the_graph(&mut window, area);
        window.selected = Some(5);
        frame(&context, &mut window, Vec::new());

        let bounds = crate::canvas::camera_bounds(&window.graph.as_ref().unwrap().1).unwrap();
        let limited = limit_camera(window.scene_rect, bounds);
        assert!((window.scene_rect.size() - limited.size()).length() < 0.01);
        assert!((window.scene_rect.center() - limited.center()).length() < 0.01);
    }

    #[test]
    fn following_live_data_does_not_change_the_zoom() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        zoomed_out_past_the_graph(&mut window, area);
        let before = (area.size() / window.scene_rect.size()).min_elem();
        window.follow = true;
        window.selected = Some(5);
        frame(&context, &mut window, Vec::new());

        let after = (area.size() / window.scene_rect.size()).min_elem();
        assert!((after - before).abs() < 1e-3);
    }

    #[test]
    fn a_jump_stays_inside_the_limits() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        let key = {
            let (_, graph) = window.graph.as_ref().unwrap();
            let node = graph.nodes.last().unwrap();
            (node.tick, node.system)
        };
        window.scene_rect = Rect::from_center_size(pos2(0.0, 0.0), area.size() * 0.5);
        window.jump_to(key);
        frame(&context, &mut window, Vec::new());
        assert_inside_limits(&window);
    }

    #[test]
    fn the_static_drawing_is_reused_until_the_graph_changes() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        let first = window.static_mesh.mesh_for_tests();
        assert!(!first.is_empty());
        frame(&context, &mut window, Vec::new());
        assert!(std::sync::Arc::ptr_eq(
            &first,
            &window.static_mesh.mesh_for_tests()
        ));

        window.selected = Some(5);
        frame(&context, &mut window, Vec::new());
        assert!(!std::sync::Arc::ptr_eq(
            &first,
            &window.static_mesh.mesh_for_tests()
        ));
    }

    #[test]
    fn the_limits_are_the_blocks_and_the_lowest_edge() {
        let context = Context::default();
        let (window, _) = window_ready(&context);
        let (_, graph) = window.graph.as_ref().unwrap();
        let bounds = crate::canvas::camera_bounds(graph).unwrap();
        let first = &graph.ticks[0];
        let last = graph.ticks.last().unwrap();
        let lowest = graph
            .wires
            .iter()
            .flat_map(|wire| wire.route.iter().map(|point| point.y))
            .fold(first.bottom, f32::max);

        assert_eq!(bounds.top(), first.top - 40.0);
        assert_eq!(bounds.bottom(), lowest + 40.0);
        assert_eq!(
            bounds.left(),
            first.left - crate::graph::BLOCK_PADDING - 40.0
        );
        assert_eq!(
            bounds.right(),
            last.right + crate::graph::BLOCK_PADDING + 40.0
        );
    }

    fn node_rect_of(window: &TrackerApp, key: (u64, u128)) -> Rect {
        let (_, graph) = window.graph.as_ref().unwrap();
        let node = graph
            .nodes
            .iter()
            .find(|node| (node.tick, node.system) == key)
            .unwrap();
        Rect::from_min_size(node.position, node.size())
    }

    fn first_node(window: &TrackerApp) -> (u64, u128) {
        let (_, graph) = window.graph.as_ref().unwrap();
        let node = graph.nodes.first().unwrap();
        (node.tick, node.system)
    }

    fn zoom_of(area: Rect, view: Rect) -> f32 {
        (area.size() / view.size()).min_elem()
    }

    #[test]
    fn f_frames_the_selected_node_with_a_zoom_limit() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        let node = first_node(&window);
        window.scene_rect = Rect::from_center_size(pos2(4000.0, 300.0), area.size() / 8.0);
        window.set_focus(Some(Focus::Node(node)));
        frame(&context, &mut window, vec![key(Key::F, Modifiers::NONE)]);
        frame(&context, &mut window, Vec::new());

        assert!(window.scene_rect.contains_rect(node_rect_of(&window, node)));
        assert!(zoom_of(area, window.scene_rect) <= 1.0 + 1e-3);
        assert_inside_limits(&window);
    }

    fn two_nodes(window: &TrackerApp) -> (Focus, Focus, (u64, u128), (u64, u128)) {
        let (_, graph) = window.graph.as_ref().unwrap();
        let key = |node: &crate::graph::Node| (node.tick, node.system);
        let (first, last) = (
            key(graph.nodes.first().unwrap()),
            key(graph.nodes.last().unwrap()),
        );
        (Focus::Node(first), Focus::Node(last), first, last)
    }

    #[test]
    fn shift_adds_and_removes_and_a_plain_click_replaces() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        let (a, b, _, _) = two_nodes(&window);

        window.click(Click::On(a));
        window.click(Click::Toggle(b));
        assert_eq!(window.focus, [a, b]);
        window.click(Click::Toggle(a));
        assert_eq!(window.focus, [b]);
        window.click(Click::Toggle(a));
        assert_eq!(window.focus, [b, a]);

        window.click(Click::On(a));
        assert_eq!(window.focus, [a]);
    }

    #[test]
    fn f_and_history_cover_every_selected_element() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        let (a, b, first, last) = two_nodes(&window);
        window.click(Click::On(a));
        window.click(Click::Toggle(b));
        frame(&context, &mut window, Vec::new());
        window.scene_rect = Rect::from_center_size(pos2(50.0, 50.0), area.size() / 8.0);
        frame(&context, &mut window, vec![key(Key::F, Modifiers::NONE)]);
        frame(&context, &mut window, Vec::new());
        assert!(
            window
                .scene_rect
                .contains_rect(node_rect_of(&window, first))
        );
        assert!(window.scene_rect.contains_rect(node_rect_of(&window, last)));

        window.remember();
        window.set_focus(None);
        frame(&context, &mut window, Vec::new());
        window.go_back();
        frame(&context, &mut window, Vec::new());
        assert_eq!(window.focus, [a, b]);
    }

    #[test]
    fn a_selection_that_leaves_the_screen_only_drops_that_element() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        let (a, b, _, _) = two_nodes(&window);
        window.focus = vec![a, b];
        if let Focus::Node((tick, _)) = b {
            window.selected = Some(tick);
        }
        window.ticks_shown = 1;
        frame(&context, &mut window, Vec::new());
        assert_eq!(window.focus, [b]);
    }

    fn shift_click(context: &Context, app: &mut TrackerApp, position: Pos2) {
        let button = |pressed| Event2::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::SHIFT,
        };
        frame(
            context,
            app,
            vec![
                Event2::ModifiersChanged(Modifiers::SHIFT),
                Event2::PointerMoved(position),
            ],
        );
        frame(context, app, vec![Event2::PointerMoved(position)]);
        frame(context, app, vec![button(true)]);
        frame(context, app, vec![button(false)]);
        frame(
            context,
            app,
            vec![Event2::ModifiersChanged(Modifiers::NONE)],
        );
    }

    #[test]
    fn shift_clicking_in_the_window_builds_a_selection() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        let (a, _, first, _) = two_nodes(&window);
        let center = node_rect_of(&window, first).center();
        let screen = aim(&context, &mut window, area, center);

        shift_click(&context, &mut window, screen);
        assert_eq!(window.focus, [a]);
        shift_click(&context, &mut window, screen);
        assert!(window.focus.is_empty());
    }

    #[test]
    fn shift_clicking_the_background_keeps_the_selection() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        let (a, _, _, _) = two_nodes(&window);
        window.click(Click::On(a));
        frame(&context, &mut window, Vec::new());
        shift_click(&context, &mut window, area.left_top() + vec2(5.0, 5.0));
        assert_eq!(window.focus, [a]);
    }

    #[test]
    fn f_frames_a_selected_tick_block() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        let (tick, whole) = {
            let (_, graph) = window.graph.as_ref().unwrap();
            let block = &graph.ticks[0];
            let padded =
                Rect::from_min_max(pos2(block.left, block.top), pos2(block.right, block.bottom));
            (block.tick, padded)
        };
        window.scene_rect = Rect::from_center_size(pos2(4000.0, 300.0), area.size() / 8.0);
        window.set_focus(Some(Focus::Tick(tick)));
        frame(&context, &mut window, vec![key(Key::F, Modifiers::NONE)]);
        frame(&context, &mut window, Vec::new());
        assert!(window.scene_rect.contains_rect(whole));
    }

    #[test]
    fn history_frames_an_entry_with_a_selection() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        let node = first_node(&window);
        window.click(Click::On(Focus::Node(node)));
        frame(&context, &mut window, Vec::new());

        window.remember();
        window.selected = Some(6);
        window.set_focus(None);
        frame(&context, &mut window, Vec::new());
        window.scene_rect = Rect::from_center_size(pos2(50.0, 900.0), area.size() / 4.0);
        frame(&context, &mut window, Vec::new());

        window.go_back();
        frame(&context, &mut window, Vec::new());
        assert_eq!(window.focus, [Focus::Node(node)]);
        assert!(window.scene_rect.contains_rect(node_rect_of(&window, node)));
    }

    #[test]
    fn history_without_a_selection_leaves_the_camera_alone() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        let node = first_node(&window);
        window.click(Click::On(Focus::Node(node)));
        frame(&context, &mut window, Vec::new());
        let camera = Rect::from_center_size(pos2(1000.0, 300.0), area.size() / 2.0);
        window.scene_rect = camera;
        frame(&context, &mut window, Vec::new());

        window.go_back();
        frame(&context, &mut window, Vec::new());
        assert!(window.focus.is_empty());
        assert!((window.scene_rect.center() - camera.center()).length() < 0.01);
        assert!((zoom_of(area, window.scene_rect) - zoom_of(area, camera)).abs() < 0.001);
    }

    #[test]
    fn fit_view_shows_the_whole_graph() {
        let context = Context::default();
        let (mut window, area) = window_ready(&context);
        window.scene_rect = Rect::from_center_size(pos2(50.0, 50.0), area.size() / 2.0);
        frame(&context, &mut window, vec![key(Key::F, Modifiers::NONE)]);
        frame(&context, &mut window, Vec::new());

        let bounds = crate::canvas::camera_bounds(&window.graph.as_ref().unwrap().1).unwrap();
        assert!(window.scene_rect.expand(0.01).contains_rect(bounds));
    }

    #[test]
    fn live_data_waits_for_the_update_rate() {
        let context = Context::default();
        let mut live = app(Some(recorded(2)), false);
        live.update_rate = 10;
        let push = |window: &mut TrackerApp, tick| {
            let message = TrackerMessage::Frame { tick, frame: None };
            window.apply(&context, Event::Message(message));
        };

        frame_at(&context, &mut live, Vec::new(), Some(1.0));
        assert_eq!(live.selected, Some(1));

        push(&mut live, 2);
        frame_at(&context, &mut live, Vec::new(), Some(1.01));
        assert_eq!(live.selected, Some(2));

        push(&mut live, 3);
        frame_at(&context, &mut live, Vec::new(), Some(1.05));
        assert_eq!(live.selected, Some(2));

        frame_at(&context, &mut live, Vec::new(), Some(1.2));
        assert_eq!(live.selected, Some(3));
    }

    #[test]
    fn turning_follow_on_without_new_data_jumps_at_once() {
        let context = Context::default();
        let mut window = app(Some(recorded(6)), false);
        window.follow = false;
        window.selected = Some(1);
        frame_at(&context, &mut window, Vec::new(), Some(1.0));
        assert_eq!(window.selected, Some(1));

        window.follow = true;
        frame_at(&context, &mut window, Vec::new(), Some(1.01));
        assert_eq!(window.selected, Some(5));
    }

    #[test]
    fn the_navigation_thumb_shows_where_the_view_is() {
        let track = Rect::from_min_size(pos2(100.0, 0.0), vec2(1000.0, 14.0));
        let bounds = Rect::from_min_size(Pos2::ZERO, vec2(4000.0, 900.0));
        let view = Rect::from_min_size(pos2(2000.0, 0.0), vec2(800.0, 450.0));

        let thumb = navigation_thumb(track, view, bounds);
        assert_eq!(thumb.left(), 600.0);
        assert_eq!(thumb.right(), 800.0);
    }

    #[test]
    fn clicking_the_navigation_bar_moves_the_camera() {
        let context = Context::default();
        let (mut window, area) = window_ready(&context);
        let extent = window.graph.as_ref().unwrap().1.extent;
        window.scene_rect = Rect::from_min_size(Pos2::ZERO, area.size() / 4.0);
        frame(&context, &mut window, Vec::new());
        let before = window.scene_rect.center().x;

        let bar_y = (area.bottom() + 900.0) / 2.0;
        click_at(&context, &mut window, pos2(area.right() - 20.0, bar_y));
        assert!(window.scene_rect.center().x > before + extent.x / 2.0);
    }

    #[test]
    fn the_wheel_moves_along_the_bar_from_anywhere_in_the_window() {
        let context = Context::default();
        let (mut window, area) = window_ready(&context);
        window.scene_rect = Rect::from_min_size(Pos2::ZERO, area.size() / 4.0);
        let over_graph = area.center();
        frame_at(
            &context,
            &mut window,
            vec![Event2::PointerMoved(over_graph)],
            Some(1.0),
        );
        frame_at(
            &context,
            &mut window,
            vec![Event2::PointerMoved(over_graph)],
            Some(1.02),
        );
        let before = window.scene_rect.center();

        let wheel = vec![Event2::MouseWheel {
            unit: eframe::egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -100.0),
            phase: eframe::egui::TouchPhase::Move,
            source: eframe::egui::MouseWheelSource::Wheel,
            modifiers: Default::default(),
        }];
        frame_at(&context, &mut window, wheel, Some(1.04));
        for step in 0..30 {
            frame_at(
                &context,
                &mut window,
                Vec::new(),
                Some(1.06 + step as f64 * 0.02),
            );
        }
        assert!(window.scene_rect.center().x > before.x);
        assert_eq!(window.scene_rect.center().y, before.y);
    }

    fn stepping_window(context: &Context) -> (TrackerApp, Rect) {
        let mut window = app(Some(recorded(8)), false);
        window.follow = false;
        window.selected = Some(4);
        frame(context, &mut window, Vec::new());
        let area = frame(context, &mut window, Vec::new());
        window.scene_rect = Rect::from_center_size(pos2(1200.0, 300.0), area.size() / 2.0);
        frame(context, &mut window, Vec::new());
        (window, area)
    }

    #[test]
    fn changing_ticks_keeps_the_reference_title_and_the_zoom() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        let camera = window.scene_rect;
        let (slot, left) = {
            let (_, graph) = window.graph.as_ref().unwrap();
            crate::canvas::title_anchor(graph, camera).unwrap()
        };
        let on_screen = left - camera.left();

        window.selected = Some(5);
        frame(&context, &mut window, Vec::new());

        let (_, graph) = window.graph.as_ref().unwrap();
        let block_left = graph.ticks[slot].left - crate::graph::BLOCK_PADDING;
        assert!((block_left - window.scene_rect.left() - on_screen).abs() < 0.01);
        assert!((window.scene_rect.size() - camera.size()).length() < 0.01);
        assert!((window.scene_rect.center().y - camera.center().y).abs() < 0.01);
    }

    #[test]
    fn back_and_forward_return_to_each_view() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        let first = window.view_state();
        let system = window.save.as_ref().unwrap().system_order()[1];

        window.jump_to((1, system));
        frame(&context, &mut window, Vec::new());
        let second = window.view_state();
        assert_ne!(first.selected, second.selected);

        window.go_back();
        frame(&context, &mut window, Vec::new());
        assert_eq!(window.selected, first.selected);
        assert_eq!(window.focus, first.focus);

        window.go_forward();
        frame(&context, &mut window, Vec::new());
        assert_eq!(window.selected, second.selected);
        assert_eq!(window.focus, second.focus);
    }

    #[test]
    fn a_new_step_forgets_the_forward_history() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        let system = window.save.as_ref().unwrap().system_order()[1];
        window.jump_to((1, system));
        frame(&context, &mut window, Vec::new());
        window.go_back();
        frame(&context, &mut window, Vec::new());
        assert_eq!(window.forward.len(), 1);

        window.jump_to((2, system));
        assert!(window.forward.is_empty());
    }

    #[test]
    fn the_history_has_a_limit() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        for tick in 0..(super::HISTORY_LIMIT as u64 + 50) {
            window.selected = Some(tick);
            window.remember();
        }
        assert_eq!(window.back.len(), super::HISTORY_LIMIT);
    }

    #[test]
    fn mouse_buttons_four_and_five_move_through_the_history() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        let first = window.selected;
        let system = window.save.as_ref().unwrap().system_order()[1];
        window.jump_to((1, system));
        frame(&context, &mut window, Vec::new());
        let second = window.selected;

        press_at(&context, &mut window, area.center(), PointerButton::Extra1);
        assert_eq!(window.selected, first);
        press_at(&context, &mut window, area.center(), PointerButton::Extra2);
        assert_eq!(window.selected, second);
    }

    #[test]
    fn the_history_keeps_the_zoom_and_the_reference_title() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        let system = window.save.as_ref().unwrap().system_order()[1];
        window.jump_to((1, system));
        frame(&context, &mut window, Vec::new());
        let camera = window.scene_rect;
        let (slot, left) = {
            let (_, graph) = window.graph.as_ref().unwrap();
            crate::canvas::title_anchor(graph, camera).unwrap()
        };

        window.go_back();
        frame(&context, &mut window, Vec::new());

        let (_, graph) = window.graph.as_ref().unwrap();
        let block_left = graph.ticks[slot].left - crate::graph::BLOCK_PADDING;
        assert!((block_left - window.scene_rect.left() - (left - camera.left())).abs() < 0.01);
        assert!((window.scene_rect.size() - camera.size()).length() < 0.01);
        assert!((window.scene_rect.center().y - camera.center().y).abs() < 0.01);
    }

    #[test]
    fn dragging_with_the_side_buttons_does_not_pan() {
        let context = Context::default();
        let (mut window, area) = stepping_window(&context);
        let camera = window.scene_rect;
        let start = area.center();
        let end = start + vec2(120.0, 90.0);
        let button = |pos, which, pressed| Event2::PointerButton {
            pos,
            button: which,
            pressed,
            modifiers: Default::default(),
        };
        for which in [PointerButton::Extra1, PointerButton::Extra2] {
            frame(&context, &mut window, vec![Event2::PointerMoved(start)]);
            frame(&context, &mut window, vec![button(start, which, true)]);
            frame(&context, &mut window, vec![Event2::PointerMoved(end)]);
            frame(&context, &mut window, vec![button(end, which, false)]);
        }
        assert_eq!(window.scene_rect, camera);
    }

    fn key(key: Key, modifiers: Modifiers) -> Event2 {
        Event2::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn the_arrow_keys_step_like_the_arrow_buttons() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        let last = window.selected.unwrap();
        let cases = [
            (Key::ArrowLeft, Modifiers::NONE, 1),
            (Key::ArrowLeft, Modifiers::SHIFT, 5),
        ];
        let mut expected = last;
        for (pressed, modifiers, step) in cases {
            frame(&context, &mut window, vec![key(pressed, modifiers)]);
            expected = expected.saturating_sub(step);
            assert_eq!(window.selected, Some(expected));
        }
        frame(
            &context,
            &mut window,
            vec![key(Key::ArrowRight, Modifiers::NONE)],
        );
        assert_eq!(window.selected, Some(expected + 1));
        assert!(!window.follow);
    }

    #[test]
    fn holding_an_arrow_key_is_one_history_entry() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        let held = |repeat| Event2::Key {
            key: Key::ArrowLeft,
            physical_key: None,
            pressed: true,
            repeat,
            modifiers: Modifiers::NONE,
        };
        frame(&context, &mut window, vec![held(false)]);
        frame(&context, &mut window, vec![held(true)]);
        frame(&context, &mut window, vec![held(true)]);
        assert_eq!(window.back.len(), 1);
    }

    #[test]
    fn ctrl_z_goes_back_and_ctrl_shift_z_goes_forward() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        let first = window.selected;
        let system = window.save.as_ref().unwrap().system_order()[1];
        window.jump_to((1, system));
        frame(&context, &mut window, Vec::new());
        let second = window.selected;

        frame(&context, &mut window, vec![key(Key::Z, Modifiers::COMMAND)]);
        assert_eq!(window.selected, first);
        let forward = Modifiers::COMMAND | Modifiers::SHIFT;
        frame(&context, &mut window, vec![key(Key::Z, forward)]);
        assert_eq!(window.selected, second);
    }

    fn links(count: usize, text: &str) -> NodeLinks {
        let link = |_| Link {
            signal: 1,
            text: text.to_string(),
            target: None,
        };
        NodeLinks {
            inputs: (0..count).map(link).collect(),
            outputs: Vec::new(),
        }
    }

    fn open_menu_with(context: &Context, window: &mut TrackerApp, links: NodeLinks) -> Rect {
        let system = window.save.as_ref().unwrap().system_order()[1];
        window.menu = Some(NodeMenu {
            target: Hit::Node((1, system)),
            at: pos2(300.0, 300.0),
            links,
            rect: None,
        });
        for _ in 0..4 {
            frame(context, window, Vec::new());
        }
        window.menu.as_ref().unwrap().rect.unwrap()
    }

    #[test]
    fn the_menu_grows_with_its_contents_even_after_a_small_one() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        open_menu_with(&context, &mut window, links(1, "short"));

        let text = "a very long line of text, ".repeat(5);
        let font = egui::TextStyle::Button.resolve(&context.global_style());
        let width = context.fonts_mut(|fonts| {
            let galley = fonts.layout_no_wrap(text.clone(), font, Default::default());
            galley.size().x
        });
        assert!(width > 600.0);
        let rect = open_menu_with(&context, &mut window, links(1, &text));
        assert!(rect.width() >= width);
    }

    #[test]
    fn a_menu_larger_than_the_screen_stays_on_the_screen() {
        let context = Context::default();
        let (mut window, _) = stepping_window(&context);
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1600.0, 900.0));

        let tall = open_menu_with(&context, &mut window, links(500, "line"));
        assert!(screen.expand(0.5).contains_rect(tall));
        assert!(tall.height() > 800.0);

        let text = "wide ".repeat(800);
        let wide = open_menu_with(&context, &mut window, links(1, &text));
        assert!(screen.expand(0.5).contains_rect(wide));
        assert!(wide.width() > 1500.0);
    }

    #[test]
    fn right_clicking_an_off_screen_edge_opens_its_menu() {
        let context = Context::default();
        let mut window = app(Some(recorded(4)), false);
        window.follow = false;
        window.selected = Some(1);
        window.ticks_shown = 1;
        frame(&context, &mut window, Vec::new());
        let area = frame(&context, &mut window, Vec::new());

        let (point, key) = {
            let (_, graph) = window.graph.as_ref().unwrap();
            let node = graph.nodes.iter().find(|x| !x.inputs.is_empty()).unwrap();
            let port = &node.inputs[0];
            let anchor = crate::graph::input_anchor(node, 0);
            let point = pos2(port.stub_x, anchor.y - 30.0);
            let hit = crate::graph::stub_at(graph, point, 3.0).unwrap();
            (point, graph.stub_key(&hit))
        };
        let screen = aim(&context, &mut window, area, point);
        press_at(&context, &mut window, screen, PointerButton::Secondary);

        let menu = window.menu.as_ref().expect("the menu opened");
        assert_eq!(menu.target, Hit::Stub(key));
        assert!(!menu.links.inputs.is_empty());
    }

    #[test]
    fn the_step_follows_the_modifiers() {
        let keys = |shift, ctrl| Modifiers {
            shift,
            ctrl,
            ..Default::default()
        };
        assert_eq!(tick_step(keys(false, false)), 1);
        assert_eq!(tick_step(keys(true, false)), 5);
        assert_eq!(tick_step(keys(false, true)), 25);
        assert_eq!(tick_step(keys(true, true)), 100);
    }

    #[test]
    fn steps_stop_at_the_ends_of_the_recording() {
        assert_eq!(stepped(50, 25, true, &(10..60)), 59);
        assert_eq!(stepped(50, 5, true, &(10..60)), 55);
        assert_eq!(stepped(12, 100, false, &(10..60)), 10);
        assert_eq!(stepped(12, 1, false, &(0..60)), 11);
    }

    #[test]
    fn the_arrows_have_glyphs_in_the_default_font() {
        let context = Context::default();
        let mut output = context.run_ui(RawInput::default(), |ui| {
            let arrows = format!("{PREVIOUS_TICK}{NEXT_TICK}");
            let font = FontId::proportional(14.0);
            assert!(ui.fonts_mut(|fonts| fonts.has_glyphs(&font, &arrows)));
        });
        output.textures_delta.clear();
    }

    #[test]
    fn clicking_isolates_and_clicking_again_or_elsewhere_leaves() {
        let save = recorded(4);
        let system = save.system_order()[0];
        let mut window = app(Some(save), false);
        draw(&mut window);

        window.click(Click::On(Focus::Node((3, system))));
        assert_eq!(window.focus, [Focus::Node((3, system))]);
        assert!(!window.follow);
        draw(&mut window);

        window.click(Click::On(Focus::Node((3, system))));
        assert!(window.focus.is_empty());

        window.click(Click::On(Focus::Node((3, system))));
        window.click(Click::Background);
        assert!(window.focus.is_empty());
    }

    #[test]
    fn isolation_ends_when_its_tick_leaves_the_screen() {
        let save = recorded(6);
        let system = save.system_order()[0];
        let mut window = app(Some(save), false);
        draw(&mut window);
        window.click(Click::On(Focus::Node((5, system))));
        draw(&mut window);
        assert_eq!(window.focus, [Focus::Node((5, system))]);

        window.selected = Some(2);
        draw(&mut window);
        assert!(window.focus.is_empty());
    }

    #[test]
    fn frames_extend_a_full_save() {
        let context = Context::default();
        let mut live = app(None, false);
        live.apply(&context, Event::Connected);
        assert_eq!(live.status, Status::Connected);

        live.apply(&context, Event::Message(TrackerMessage::Full(recorded(2))));
        live.apply(
            &context,
            Event::Message(TrackerMessage::Frame {
                tick: 2,
                frame: None,
            }),
        );
        assert_eq!(live.save.as_ref().unwrap().tick_range(), 0..3);

        live.apply(&context, Event::Closed(None));
        assert_eq!(live.status, Status::Closed);
        draw(&mut live);
    }

    #[test]
    fn a_real_port_fills_the_window() {
        let mut world = World::new();
        let mut tracker = world.to_tracker();
        let port = tracker.listen(0).unwrap();
        for _ in 0..3 {
            tracker.step(&world);
            world.step();
        }

        let context = Context::default();
        let mut live = TrackerApp::new(Source::Port(port), None, false, context.clone());
        let started = Instant::now();
        while live
            .save
            .as_ref()
            .is_none_or(|save| save.tick_range().end < 6)
        {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no data arrived"
            );
            tracker.step(&world);
            world.step();
            std::thread::sleep(Duration::from_millis(5));
            live.drain(&context);
        }
        assert_eq!(live.status, Status::Connected);

        drop(tracker);
        let started = Instant::now();
        while live.status != Status::Closed {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the connection never closed"
            );
            std::thread::sleep(Duration::from_millis(5));
            live.drain(&context);
        }
        draw(&mut live);
    }
}
