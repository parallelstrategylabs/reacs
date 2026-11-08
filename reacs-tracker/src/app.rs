use crate::canvas::{self, Click};
use crate::graph::{self, NodeKey, TickGraph};
use crate::source::{Event, Source};
use eframe::egui::{self, Context, Rect, ViewportCommand};
use reacs::tracking::{TrackedFrame, TrackerMessage, TrackingSave};
use std::ops::Range;
use std::sync::mpsc::Receiver;

const MAX_TICKS_SHOWN: u64 = 32;

type GraphKey = (u64, Option<u64>, u64, u64, Option<NodeKey>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    Waiting,
    Connected,
    Closed,
    Offline,
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
    has_outside: bool,
    straight_wires: bool,
    focus: Option<NodeKey>,
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
            has_outside,
            straight_wires: true,
            focus: None,
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

    fn sync_selection(&mut self) {
        let Some(save) = &self.save else {
            return;
        };
        let range = view_range(save, self.interval);
        if range.is_empty() {
            self.selected = None;
        } else if self.follow || self.selected.is_none() {
            self.selected = Some(range.end - 1);
        } else {
            self.selected = self
                .selected
                .map(|tick| tick.clamp(range.start, range.end - 1));
        }

        let visible = self
            .selected
            .map(|selected| visible_ticks(&range, selected, self.ticks_shown));
        if !self
            .focus
            .is_some_and(|(tick, _)| visible.is_some_and(|x| x.contains(&tick)))
        {
            self.set_focus(None);
        }
    }

    fn set_focus(&mut self, focus: Option<NodeKey>) {
        if self.focus != focus {
            self.focus = focus;
            self.scene_rect = Rect::ZERO;
        }
    }

    fn click(&mut self, click: Click) {
        match click {
            Click::Node(key) if self.focus == Some(key) => self.set_focus(None),
            Click::Node(key) => {
                self.follow = false;
                self.set_focus(Some(key));
            }
            Click::Background => self.set_focus(None),
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
                self.focus = None;
            }

            ui.separator();
            ui.label("ticks shown");
            ui.add(egui::DragValue::new(&mut self.ticks_shown).range(1..=MAX_TICKS_SHOWN));
            ui.checkbox(&mut self.follow, "follow latest tick");
            if ui.button("fit view").clicked() {
                self.scene_rect = Rect::ZERO;
            }
            ui.checkbox(&mut self.straight_wires, "straight wires");
            ui.separator();
            ui.label("solid: one-tick signal, dashed: window");
        });

        let Some(save) = &self.save else {
            return;
        };
        if let Some((tick, system)) = self.focus {
            let name = graph::short_name(save.system_name(system)).to_string();
            let mut clear = false;
            ui.horizontal(|ui| {
                ui.strong(format!("Isolated chain of {name} on tick {tick}"));
                clear = ui.button("show all").clicked();
                ui.label("click the node, the background or press Esc to leave");
            });
            if clear {
                self.set_focus(None);
            }
        }

        let Some(save) = &self.save else {
            return;
        };
        let range = view_range(save, self.interval);
        if let (false, Some(mut tick)) = (range.is_empty(), self.selected) {
            ui.horizontal(|ui| {
                ui.label("last tick shown");
                let slider = egui::Slider::new(&mut tick, range.start..=range.end - 1);
                if ui.add(slider).changed() {
                    self.selected = Some(tick);
                    self.follow = false;
                }
            });
        }
    }

    fn draw_graph(&mut self, ui: &mut egui::Ui) {
        let (Some(save), Some(selected)) = (&self.save, self.selected) else {
            ui.centered_and_justified(|ui| ui.label("nothing recorded yet"));
            return;
        };

        let key = (
            self.version,
            self.interval,
            selected,
            self.ticks_shown,
            self.focus,
        );
        if self.graph.as_ref().is_none_or(|(cached, _)| *cached != key) {
            let range = view_range(save, self.interval);
            let ticks = visible_ticks(&range, selected, self.ticks_shown);
            let frames = visible_frames(save, self.interval, ticks.clone());
            let built = graph::build(save, &frames, self.has_outside, ticks, self.focus);
            self.graph = Some((key, built));
        }
        let click = self.graph.as_ref().and_then(|(_, built)| {
            canvas::draw(ui, built, &mut self.scene_rect, self.straight_wires)
        });
        if let Some(click) = click {
            self.click(click);
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.set_focus(None);
        }
        self.sync_selection();
        egui::Panel::top("summary").show(ui, |ui| self.draw_summary(ui));
        egui::CentralPanel::default().show(ui, |ui| self.draw_graph(ui));
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
    use super::{Status, TrackerApp, view_range, visible_frames, visible_ticks};
    use crate::canvas::Click;
    use crate::source::{Event, Source};
    use crate::test_support::{recorded, recorded_paused};
    use eframe::egui::{Context, RawInput};
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
            let mut output = context.run_ui(RawInput::default(), |ui| app.draw(ui));
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

    #[test]
    fn clicking_isolates_and_clicking_again_or_elsewhere_leaves() {
        let save = recorded(4);
        let system = save.system_order()[0];
        let mut window = app(Some(save), false);
        draw(&mut window);

        window.click(Click::Node((3, system)));
        assert_eq!(window.focus, Some((3, system)));
        assert!(!window.follow);
        draw(&mut window);

        window.click(Click::Node((3, system)));
        assert_eq!(window.focus, None);

        window.click(Click::Node((3, system)));
        window.click(Click::Background);
        assert_eq!(window.focus, None);
    }

    #[test]
    fn isolation_ends_when_its_tick_leaves_the_screen() {
        let save = recorded(6);
        let system = save.system_order()[0];
        let mut window = app(Some(save), false);
        draw(&mut window);
        window.click(Click::Node((5, system)));
        draw(&mut window);
        assert_eq!(window.focus, Some((5, system)));

        window.selected = Some(2);
        draw(&mut window);
        assert_eq!(window.focus, None);
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
