//! The Timeline's time ruler labels: where they go and what they say, in the project's Time
//! Display Style (Project Settings ▸ Time Display Style), like After Effects.
//!
//! Timecode shows compact labels whose suffix names the smallest unit shown: `00:15f` (seconds and
//! frames) zoomed in, `05s` / `01:30s` (whole seconds) further out, `02m` / `1:00m` (whole minutes)
//! for long comps; hours appear only past an hour. Frames shows the frame number; Feet + Frames
//! shows `feet+frames`. Steps are whole display frames, so non-integer rates (29.97) label at the
//! same non-drop timecode the current-time readout shows.

use effectcraft_engine::project::TimeDisplayStyle;

/// What a ruler label shows: the style and the frame offset added to a comp frame to get the
/// displayed frame (the composition's Start Timecode, or Start Numbering Frames At), the same as
/// the current-time readout (`commands::time::display_time`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RulerStyle {
    pub style: TimeDisplayStyle,
    /// Nominal frames per second (30 for 29.97).
    pub base: i64,
    /// Displayed frame = comp frame + `offset`.
    pub offset: i64,
}

impl RulerStyle {
    /// Candidate label steps in displayed frames, smallest first.
    pub fn steps(&self) -> Vec<i64> {
        let mut v: Vec<i64> = match self.style {
            TimeDisplayStyle::Timecode => {
                let b = self.base.max(1);
                let mut v: Vec<i64> = [1, 2, 5, 10, b / 2].into_iter().filter(|d| *d >= 1 && *d < b).collect();
                // Seconds, then minutes, then hours.
                for s in [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 18_000, 36_000, 86_400] {
                    v.push(s * b);
                }
                v
            }
            TimeDisplayStyle::Frames => decades(1),
            TimeDisplayStyle::Feet35 | TimeDisplayStyle::Feet16 => {
                let pf = self.style.frames_per_foot().unwrap_or(16).max(1);
                let mut v: Vec<i64> = [1, 2, 4, 5, 8, 10, 20].into_iter().filter(|d| *d < pf && pf % d == 0).collect();
                v.extend(decades(pf));
                v
            }
        };
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The label at displayed frame `n` for labels every `step` displayed frames.
    pub fn label(&self, n: i64, step: i64) -> String {
        match self.style {
            TimeDisplayStyle::Timecode => compact_timecode(n, self.base, step),
            TimeDisplayStyle::Frames => n.to_string(),
            TimeDisplayStyle::Feet35 | TimeDisplayStyle::Feet16 => {
                // The readout's `0012+08`, without the feet field's leading zeros.
                let s = effectcraft_engine::time::format_feet_frames(n, self.style.frames_per_foot().unwrap_or(16));
                let (sign, rest) = s.strip_prefix('-').map_or(("", s.as_str()), |r| ("-", r));
                let trimmed = rest.trim_start_matches('0');
                let trimmed = if trimmed.starts_with('+') || trimmed.is_empty() { format!("0{trimmed}") } else { trimmed.to_string() };
                format!("{sign}{trimmed}")
            }
        }
    }

    /// The smallest step whose labels fit: `px_per_frame` apart they leave room for the widest
    /// label up to displayed frame `last` (`width` measures a label) plus a gap.
    pub fn pick_step(&self, px_per_frame: f64, last: i64, width: impl Fn(&str) -> f32) -> i64 {
        let steps = self.steps();
        let fallback = steps.last().copied().unwrap_or(1);
        for step in steps {
            let px = step as f64 * px_per_frame;
            // Cheap reject before measuring: no label is narrower than ~28 px.
            if px < 28.0 {
                continue;
            }
            let widest = [last, last.saturating_sub(step), step.saturating_mul(3)]
                .iter()
                .map(|n| width(&self.label(n.saturating_sub(n.rem_euclid(step)), step)))
                .fold(0.0f32, f32::max);
            if px >= f64::from(widest) + 14.0 {
                return step;
            }
        }
        fallback
    }
}

/// `start`, then 1-2-5 multiples of it per decade, up to a billion frames.
fn decades(start: i64) -> Vec<i64> {
    let mut v = Vec::new();
    let mut m = start.max(1);
    while m <= 1_000_000_000 {
        v.extend([m, m * 2, m * 5]);
        m *= 10;
    }
    v
}

/// After Effects-style compact ruler timecode: the suffix names the smallest unit shown (frames
/// `f` when `step` isn't whole seconds, seconds `s`, minutes `m` when it is whole minutes), the
/// leading field is two digits and zero fields above it are left out (`00:15f`, `05s`, `01:30s`,
/// `09:36:00f`, `01:00:30s`).
pub(crate) fn compact_timecode(n: i64, base: i64, step: i64) -> String {
    let b = base.max(1);
    let (neg, h, m, s, f) = effectcraft_engine::time::frames_to_fields(n, effectcraft_engine::time::FrameRate::new(b, 1), false);
    let sign = if neg { "-" } else { "" };
    let fw = if b >= 100 { 3 } else { 2 };
    let label = if step % b != 0 {
        if h > 0 {
            format!("{h:02}:{m:02}:{s:02}:{f:0fw$}f")
        } else if m > 0 {
            format!("{m:02}:{s:02}:{f:0fw$}f")
        } else {
            format!("{s:02}:{f:0fw$}f")
        }
    } else if step % (60 * b) != 0 {
        if n == 0 {
            "0s".into()
        } else if h > 0 {
            format!("{h:02}:{m:02}:{s:02}s")
        } else if m > 0 {
            format!("{m:02}:{s:02}s")
        } else {
            format!("{s:02}s")
        }
    } else if n == 0 {
        "0m".into()
    } else if h > 0 {
        format!("{h:02}:{m:02}m")
    } else {
        format!("{m:02}m")
    };
    format!("{sign}{label}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tc(base: i64) -> RulerStyle {
        RulerStyle { style: TimeDisplayStyle::Timecode, base, offset: 0 }
    }

    /// Roughly the width of a 10 px UI label: 6 px a character.
    fn w(s: &str) -> f32 {
        s.chars().count() as f32 * 6.0
    }

    #[test]
    fn timecode_labels_roll_into_minutes_at_60_seconds() {
        let r = tc(30);
        // Frame steps: seconds:frames, minutes once past 60 s (not after an hour).
        assert_eq!(r.label(15, 15), "00:15f");
        assert_eq!(r.label(30, 15), "01:00f");
        assert_eq!(r.label(59 * 30 + 15, 15), "59:15f");
        assert_eq!(r.label(60 * 30, 15), "01:00:00f");
        assert_eq!(r.label(576 * 30, 15), "09:36:00f");
        // Second steps.
        assert_eq!(r.label(0, 150), "0s");
        assert_eq!(r.label(150, 150), "05s");
        assert_eq!(r.label(90 * 30, 900), "01:30s");
        assert_eq!(r.label(570 * 30, 900), "09:30s");
        // Minute steps.
        assert_eq!(r.label(0, 1800), "0m");
        assert_eq!(r.label(120 * 30, 3600), "02m");
        assert_eq!(r.label(9 * 60 * 30, 1800), "09m");
    }

    #[test]
    fn timecode_labels_roll_into_hours_at_3600_seconds() {
        let r = tc(25);
        assert_eq!(r.label(3600 * 25, 25 * 30), "01:00:00s");
        assert_eq!(r.label(3630 * 25 + 5, 5), "01:00:30:05f");
        assert_eq!(r.label(5400 * 25, 25 * 600), "01:30m");
        assert_eq!(r.label(-25, 25), "-01s");
    }

    #[test]
    fn non_integer_rates_label_nominal_timecode() {
        // 29.97 counts 30 frames a second (non-drop), like the current-time readout.
        let r = tc(effectcraft_engine::time::FrameRate::new(30_000, 1001).timecode_base());
        assert_eq!(r.base, 30);
        assert_eq!(r.label(1800, 30), "01:00s");
        assert_eq!(r.label(17_280, 900), "09:36s");
        assert_eq!(r.label(17_281, 1), "09:36:01f");
        let r = tc(120);
        assert_eq!(r.label(7, 5), "00:007f");
    }

    #[test]
    fn frames_and_feet_labels() {
        let fr = RulerStyle { style: TimeDisplayStyle::Frames, base: 30, offset: 0 };
        assert_eq!(fr.label(0, 100), "0");
        assert_eq!(fr.label(17_280, 100), "17280");
        assert_eq!(fr.steps()[..6], [1, 2, 5, 10, 20, 50]);
        let ft = RulerStyle { style: TimeDisplayStyle::Feet35, base: 24, offset: 0 };
        assert_eq!(ft.label(0, 16), "0+00");
        assert_eq!(ft.label(40, 8), "2+08");
        assert_eq!(ft.label(16 * 1080, 16 * 100), "1080+00");
        assert_eq!(ft.label(-8, 8), "-0+08");
        assert!(ft.steps().contains(&8) && ft.steps().contains(&160));
        let ft16 = RulerStyle { style: TimeDisplayStyle::Feet16, base: 24, offset: 0 };
        assert_eq!(ft16.label(85, 5), "2+05");
        assert!(!ft16.steps().contains(&16), "{:?}", ft16.steps());
    }

    #[test]
    fn steps_leave_room_for_the_labels() {
        let r = tc(30);
        // A 9:36 comp (17 280 frames) across ~1000 px: 30-second labels.
        let ppf = 1000.0 / 17_280.0;
        assert_eq!(r.pick_step(ppf, 17_280, w), 30 * 30);
        // Across 10 px a second: 5-second labels ("09:35s" is 36 px).
        assert_eq!(r.pick_step(10.0 / 30.0, 17_280, w), 150);
        // Zoomed in to 40 px a frame: every frame, "09:36:00f".
        assert_eq!(r.pick_step(80.0, 17_280, w), 1);
        // A 3-hour comp squeezed into 300 px: half hours ("02:30m").
        let last = 3 * 3600 * 30;
        let step = r.pick_step(300.0 / last as f64, last, w);
        assert_eq!(step, 1800 * 30);
        assert_eq!(r.label(5400 * 30, step), "01:30m");
        // Every pick leaves the widest label room.
        for ppf in [0.001, 0.01, 0.05, 0.2, 1.0, 3.0, 20.0] {
            let step = r.pick_step(ppf, 17_280, w);
            assert!(step as f64 * ppf >= f64::from(w(&r.label(17_250, step))) + 14.0 || step == *r.steps().last().unwrap(), "{ppf}: {step}");
        }
        let fr = RulerStyle { style: TimeDisplayStyle::Frames, base: 30, offset: 0 };
        assert_eq!(fr.pick_step(1.0, 17_280, w), 50);
    }
}
