use crate::graph::Hit;
use eframe::egui::{
    self, Color32, Context, Frame, Id, Margin, Order, Pos2, Rect, TextBuffer, TextEdit, Vec2,
    epaint::text::TextFormat,
    text::{CCursorRange, LayoutJob},
};
use std::time::Duration;

const OPEN_DELAY: f64 = 0.35;
const LINGER_SECONDS: f64 = 0.25;
const FROZEN_LINGER_SECONDS: f64 = 1.0;
const POINTER_GAP: Vec2 = Vec2::splat(12.0);
const REACH: f32 = 4.0;
const MOVE_AFTER_CLICK: f32 = 4.0;
const REDRAW_EVERY: Duration = Duration::from_millis(100);

pub struct Hovered {
    pub target: Hit,
    pub text: String,
}

struct Open {
    target: Hit,
    text: String,
    at: Pos2,
    rect: Option<Rect>,
    last_seen: f64,
    grabbed: bool,
    frozen: bool,
}

impl Open {
    fn text_id(&self) -> Id {
        Id::unique(("hint text", self.target))
    }
}

fn hint_job(text: &str, strong: Color32, normal: Color32) -> LayoutJob {
    let font = egui::FontId::proportional(13.0);
    let (title, body) = text.split_at(text.find('\n').unwrap_or(text.len()));
    let mut job = LayoutJob::default();
    job.append(title, 0.0, TextFormat::simple(font.clone(), strong));
    job.append(body, 0.0, TextFormat::simple(font, normal));
    job
}

#[derive(Default)]
pub struct Hints {
    open: Option<Open>,
    waiting: Option<(Hit, f64)>,
    clicked_at: Option<Pos2>,
}

impl Hints {
    pub fn update(
        &mut self,
        now: f64,
        pointer: Option<Pos2>,
        pressed: bool,
        hovered: Option<Hovered>,
    ) {
        if let Some(open) = &mut self.open {
            let over = open
                .rect
                .zip(pointer)
                .is_some_and(|(rect, pointer)| rect.expand(REACH).contains(pointer));
            open.grabbed = pressed && (open.grabbed || over);
            if over || open.grabbed {
                open.frozen = true;
                open.last_seen = now;
                self.waiting = None;
                return;
            }
            if pressed {
                self.open = None;
            } else if hovered.as_ref().is_some_and(|x| x.target == open.target) {
                open.last_seen = now;
                self.waiting = None;
                return;
            }
        }

        if pressed {
            self.clicked_at = pointer.or(self.clicked_at);
        } else if self
            .clicked_at
            .zip(pointer)
            .is_some_and(|(at, pointer)| at.distance(pointer) > MOVE_AFTER_CLICK)
        {
            self.clicked_at = None;
        }

        let Some(hovered) = hovered.filter(|_| self.clicked_at.is_none()) else {
            self.waiting = None;
            self.close_if_expired(now);
            return;
        };
        let since = match self.waiting {
            Some((target, since)) if target == hovered.target => since,
            _ => now,
        };
        self.waiting = Some((hovered.target, since));

        match pointer {
            Some(pointer) if now - since >= OPEN_DELAY => {
                self.open = Some(Open {
                    target: hovered.target,
                    text: hovered.text,
                    at: pointer + POINTER_GAP,
                    rect: None,
                    last_seen: now,
                    grabbed: false,
                    frozen: false,
                });
                self.waiting = None;
            }
            _ => self.close_if_expired(now),
        }
    }

    fn close_if_expired(&mut self, now: f64) {
        let expired = self.open.as_ref().is_some_and(|open| {
            let linger = if open.frozen {
                FROZEN_LINGER_SECONDS
            } else {
                LINGER_SECONDS
            };
            now - open.last_seen > linger
        });
        if expired {
            self.open = None;
        }
    }

    pub fn close(&mut self) {
        self.open = None;
        self.waiting = None;
    }

    pub fn show(&mut self, context: &Context) {
        if self.open.is_some() || self.waiting.is_some() {
            context.request_repaint_after(REDRAW_EVERY);
        }
        let Some(open) = &mut self.open else {
            return;
        };

        let text = open.text.as_str();
        let id = open.text_id();
        let area = egui::Area::new(egui::Id::unique("hint"))
            .order(Order::Tooltip)
            .fixed_pos(open.at)
            .constrain(true)
            .show(context, |ui| {
                Frame::popup(ui.style()).show(ui, |ui| {
                    let strong = ui.visuals().strong_text_color();
                    let normal = ui.visuals().text_color();
                    let width = ui
                        .fonts_mut(|fonts| fonts.layout_job(hint_job(text, strong, normal)))
                        .size()
                        .x;
                    let mut layouter = |ui: &egui::Ui, buffer: &dyn TextBuffer, _: f32| {
                        let job = hint_job(buffer.as_str(), strong, normal);
                        ui.fonts_mut(|fonts| fonts.layout_job(job))
                    };
                    let mut shown = text;
                    let mut output = TextEdit::multiline(&mut shown)
                        .id(id)
                        .frame(Frame::NONE)
                        .margin(Margin::ZERO)
                        .desired_width(width + 2.0)
                        .desired_rows(1)
                        .clip_text(false)
                        .layouter(&mut layouter)
                        .show(ui);
                    if output.response.triple_clicked() {
                        let everything = CCursorRange::select_all(&output.galley);
                        output.state.cursor.set_char_range(Some(everything));
                        output.state.store(ui.ctx(), id);
                    }
                });
            });
        open.rect = Some(area.response.rect);
    }

    #[cfg(test)]
    pub fn rect(&self) -> Option<Rect> {
        self.open.as_ref().and_then(|open| open.rect)
    }

    #[cfg(test)]
    pub fn selects_everything(&self, context: &Context) -> bool {
        self.open.as_ref().is_some_and(|open| {
            let state = egui::text_edit::TextEditState::load(context, open.text_id());
            let range = state.and_then(|state| state.cursor.char_range());
            range == Some(CCursorRange::select_all_str(&open.text))
        })
    }

    #[cfg(test)]
    pub fn text(&self) -> Option<&str> {
        self.open.as_ref().map(|open| open.text.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::{FROZEN_LINGER_SECONDS, Hints, Hovered, LINGER_SECONDS, OPEN_DELAY};
    use crate::graph::Hit;
    use eframe::egui::{Pos2, Rect, pos2, vec2};

    fn over(tick: u64) -> Option<Hovered> {
        Some(Hovered {
            target: Hit::Node((tick, 0)),
            text: format!("tick {tick}"),
        })
    }

    const POINTER: Option<Pos2> = Some(pos2(0.0, 0.0));
    const INSIDE: Option<Pos2> = Some(pos2(120.0, 120.0));

    fn opened() -> Hints {
        let mut hints = Hints::default();
        hints.update(0.0, POINTER, false, over(1));
        hints.update(OPEN_DELAY, POINTER, false, over(1));
        hints.open.as_mut().unwrap().rect =
            Some(Rect::from_min_size(pos2(100.0, 100.0), vec2(50.0, 50.0)));
        hints
    }

    #[test]
    fn a_hint_opens_only_after_the_pointer_rests() {
        let mut hints = Hints::default();
        hints.update(0.0, POINTER, false, over(1));
        hints.update(OPEN_DELAY / 2.0, POINTER, false, over(2));
        hints.update(OPEN_DELAY, POINTER, false, over(2));
        assert!(hints.open.is_none());
        hints.update(OPEN_DELAY * 2.0, POINTER, false, over(2));
        assert_eq!(hints.text(), Some("tick 2"));
    }

    #[test]
    fn clicking_does_not_open_a_hint_until_the_pointer_moves() {
        let mut hints = Hints::default();
        hints.update(0.0, POINTER, false, over(1));
        hints.update(OPEN_DELAY / 2.0, POINTER, true, over(1));
        hints.update(OPEN_DELAY, POINTER, false, over(1));
        hints.update(OPEN_DELAY * 3.0, POINTER, false, over(1));
        assert!(hints.open.is_none());

        let moved = Some(pos2(20.0, 0.0));
        let start = OPEN_DELAY * 4.0;
        hints.update(start, moved, false, over(1));
        assert!(hints.open.is_none());
        hints.update(start + OPEN_DELAY, moved, false, over(1));
        assert_eq!(hints.text(), Some("tick 1"));
    }

    #[test]
    fn a_small_shake_after_a_click_still_counts_as_resting() {
        let mut hints = Hints::default();
        hints.update(0.0, POINTER, true, over(1));
        let shaken = Some(pos2(2.0, 1.0));
        hints.update(0.1, shaken, false, over(1));
        hints.update(OPEN_DELAY * 3.0, shaken, false, over(1));
        assert!(hints.open.is_none());
    }

    #[test]
    fn a_hint_waits_for_the_mouse_to_reach_it_and_then_stays() {
        let mut hints = opened();
        hints.update(OPEN_DELAY + LINGER_SECONDS / 2.0, POINTER, false, None);
        assert!(hints.open.is_some());

        hints.update(OPEN_DELAY + LINGER_SECONDS / 2.0, INSIDE, false, None);
        hints.update(100.0, INSIDE, false, None);
        assert!(hints.open.is_some());

        hints.update(100.0 + FROZEN_LINGER_SECONDS * 2.0, POINTER, false, None);
        assert!(hints.open.is_none());
    }

    #[test]
    fn a_hint_never_entered_closes_quickly() {
        let mut hints = opened();
        hints.update(OPEN_DELAY + LINGER_SECONDS * 2.0, POINTER, false, None);
        assert!(hints.open.is_none());
    }

    #[test]
    fn a_frozen_hint_lingers_longer_after_the_mouse_leaves() {
        let mut hints = opened();
        hints.update(1.0, INSIDE, false, None);
        hints.update(1.0 + LINGER_SECONDS * 2.0, POINTER, false, None);
        assert!(hints.open.is_some());
    }

    #[test]
    fn closing_also_forgets_what_was_waiting() {
        let mut hints = opened();
        hints.update(OPEN_DELAY + 0.05, POINTER, false, over(2));
        hints.close();
        assert!(hints.open.is_none() && hints.waiting.is_none());
    }

    #[test]
    fn crossing_other_things_on_the_way_does_not_replace_it() {
        let mut hints = opened();
        hints.update(OPEN_DELAY + 0.05, POINTER, false, over(2));
        hints.update(OPEN_DELAY + 0.1, INSIDE, false, over(2));
        hints.update(OPEN_DELAY + 0.8, INSIDE, false, over(2));
        assert_eq!(hints.text(), Some("tick 1"));
    }

    #[test]
    fn resting_on_something_else_replaces_it() {
        let mut hints = opened();
        hints.update(OPEN_DELAY + 0.05, POINTER, false, over(2));
        hints.update(OPEN_DELAY + 0.05 + OPEN_DELAY, POINTER, false, over(2));
        assert_eq!(hints.text(), Some("tick 2"));
    }

    #[test]
    fn dragging_a_selection_out_of_the_hint_keeps_it_open() {
        let mut hints = opened();
        hints.update(5.0, INSIDE, true, None);
        hints.update(50.0, POINTER, true, None);
        assert!(hints.open.is_some());
    }

    #[test]
    fn pressing_elsewhere_closes_it() {
        let mut hints = opened();
        hints.update(OPEN_DELAY + 0.05, POINTER, true, None);
        assert!(hints.open.is_none());
    }
}
