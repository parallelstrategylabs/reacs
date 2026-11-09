use eframe::egui::{Color32, Rgba};
use std::collections::BTreeMap;

const LIGHTNESS: [f32; 3] = [0.82, 0.87, 0.92];
const CHROMA: f32 = 0.10;
const MIN_GAP_SHARE: f32 = 0.8;
const SPREAD_PASSES: usize = 256;
const SETTLED: f32 = 0.00001;

fn mixed_hash(signal: u128) -> u64 {
    let folded = (signal ^ (signal >> 64)) as u64;
    let first = (folded ^ (folded >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let second = (first ^ (first >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    second ^ (second >> 31)
}

fn hue_of(hash: u64) -> f32 {
    (hash & 0xffff) as f32 / 65536.0
}

fn oklch(lightness: f32, chroma: f32, hue: f32) -> Color32 {
    let angle = hue * std::f32::consts::TAU;
    let (a, b) = (chroma * angle.cos(), chroma * angle.sin());
    let l = (lightness + 0.396_337_78 * a + 0.215_803_76 * b).powi(3);
    let m = (lightness - 0.105_561_35 * a - 0.063_854_17 * b).powi(3);
    let s = (lightness - 0.089_484_18 * a - 1.291_485_5 * b).powi(3);
    let red = 4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s;
    let green = -1.268_438 * l + 2.609_757_4 * m - 0.341_319_4 * s;
    let blue = -0.004_196_09 * l - 0.703_418_6 * m + 1.707_614_7 * s;
    Rgba::from_rgb(
        red.clamp(0.0, 1.0),
        green.clamp(0.0, 1.0),
        blue.clamp(0.0, 1.0),
    )
    .into()
}

fn color_at(hash: u64, hue: f32) -> Color32 {
    let lightness = LIGHTNESS[((hash >> 16) % LIGHTNESS.len() as u64) as usize];
    oklch(lightness, CHROMA, hue)
}

fn spread(hues: &mut [f32]) {
    let count = hues.len();
    if count < 2 {
        return;
    }
    let gap = MIN_GAP_SHARE / count as f32;
    for _ in 0..SPREAD_PASSES {
        let mut moved = false;
        for index in 0..count {
            let next = (index + 1) % count;
            let wrap = if next == 0 { 1.0 } else { 0.0 };
            let missing = gap - (hues[next] + wrap - hues[index]);
            if missing > SETTLED {
                hues[index] -= missing / 2.0;
                hues[next] += missing / 2.0;
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
}

#[derive(Default)]
pub struct Palette {
    colors: BTreeMap<u128, Color32>,
}

impl Palette {
    pub fn new(signals: impl Iterator<Item = u128>) -> Self {
        let mut seeds: Vec<(f32, u128, u64)> = signals
            .map(|signal| {
                let hash = mixed_hash(signal);
                (hue_of(hash), signal, hash)
            })
            .collect();
        seeds.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut hues: Vec<f32> = seeds.iter().map(|seed| seed.0).collect();
        spread(&mut hues);
        let colors = seeds
            .iter()
            .zip(hues)
            .map(|((_, signal, hash), hue)| (*signal, color_at(*hash, hue.rem_euclid(1.0))))
            .collect();
        Self { colors }
    }

    pub fn color(&self, signal: u128) -> Color32 {
        self.colors.get(&signal).copied().unwrap_or_else(|| {
            let hash = mixed_hash(signal);
            color_at(hash, hue_of(hash))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{MIN_GAP_SHARE, Palette, mixed_hash, spread};
    use eframe::egui::Color32;

    fn hue_gaps(mut hues: Vec<f32>) -> Vec<f32> {
        hues.sort_by(f32::total_cmp);
        let first = hues[0];
        hues.push(first + 1.0);
        hues.windows(2).map(|pair| pair[1] - pair[0]).collect()
    }

    #[test]
    fn crowded_hues_end_up_apart() {
        let mut hues = vec![0.100, 0.101, 0.102, 0.103, 0.104, 0.105, 0.5, 0.9];
        spread(&mut hues);
        let wanted = MIN_GAP_SHARE / hues.len() as f32;
        for gap in hue_gaps(hues) {
            assert!(gap >= wanted - 0.001, "{gap}");
        }
    }

    #[test]
    fn hues_that_are_already_apart_do_not_move() {
        let before = vec![0.0, 0.25, 0.5, 0.75];
        let mut hues = before.clone();
        spread(&mut hues);
        assert_eq!(hues, before);
    }

    #[test]
    fn a_new_signal_only_nudges_the_others() {
        let ids = |count: u128| (1..=count).map(|id| id * 0x1234_5678_9abc_def1);
        let small = Palette::new(ids(9));
        let large = Palette::new(ids(10));
        let close = |a: Color32, b: Color32| {
            [
                a.r().abs_diff(b.r()),
                a.g().abs_diff(b.g()),
                a.b().abs_diff(b.b()),
            ]
            .into_iter()
            .all(|difference| difference <= 40)
        };
        let kept = ids(9)
            .filter(|id| close(small.color(*id), large.color(*id)))
            .count();
        assert!(kept >= 7, "{kept}");
    }

    #[test]
    fn a_signal_always_gets_the_same_readable_color() {
        let ids = [1u128, 7, 1 << 70, u128::MAX, 0xdead_beef_1234_5678];
        let first = Palette::new(ids.into_iter());
        let again = Palette::new(ids.into_iter().rev());
        for id in ids {
            let color = first.color(id);
            assert_eq!(color, again.color(id));
            assert_eq!(color.a(), 255);
            assert!(color.r().max(color.g()).max(color.b()) > 170);
        }
    }

    #[test]
    fn a_signal_outside_the_set_still_gets_its_color() {
        let palette = Palette::default();
        assert_eq!(palette.color(42), palette.color(42));
        assert_ne!(mixed_hash(1), mixed_hash(2));
    }
}
