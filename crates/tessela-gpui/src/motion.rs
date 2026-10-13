//! Time-driven drawing (docs/adr/0021): what the renderer keeps per node to
//! animate with no guest code running — a `Slider` level that advances by
//! itself (`rate` from `at`) and an eased, moving wave (`wave`).
//!
//! Pure state and arithmetic; the shell asks for frames while [`Sample`]
//! says so, which it only does for windows that are open.

use std::time::Duration;

use tessela_view::styled::advanced;

/// How long a committed level (a seek) wins over a `value` prop that did
/// not change since: the guest's answer is a service round trip away.
const HOLD_MS: f64 = 2000.0;
/// Amplitude easing per second (the reference's `dt * 8`).
const EASE: f32 = 8.0;
/// Longest step of a frame, so a stall does not jump the animation.
const MAX_DT: f32 = 0.05;

/// The props of a `Slider` that depend on time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub value: f32,
    pub rate: f32,
    pub at: Option<f64>,
    /// Target amplitude, px.
    pub wave: f32,
    /// Radians per second.
    pub speed: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// 0..=1, where to draw the level now.
    pub level: f32,
    pub amplitude: f32,
    pub phase: f32,
    /// Something moves every frame: ask for the next one.
    pub animating: bool,
    /// Only the level moves: redraw after this long.
    pub tick: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct Motion {
    /// `value` and `at` last seen, to tell a new report from a re-render.
    seen: (f32, Option<f64>),
    /// When `seen` arrived (ms since the epoch); the anchor without `at`.
    received: f64,
    /// A committed level and when, shown until the next report.
    held: Option<(f32, f64)>,
    amplitude: f32,
    phase: f32,
    /// Last step (ms since the epoch).
    last: f64,
    /// A timed redraw is pending until then.
    tick_due: f64,
}

impl Motion {
    /// A node appearing draws its target at once: no easing in.
    pub fn new(p: &Params, now: f64) -> Self {
        Self {
            seen: (p.value, p.at),
            received: now,
            held: None,
            amplitude: p.wave.max(0.0),
            phase: 0.0,
            last: now,
            tick_due: 0.0,
        }
    }

    /// The user released the pointer at `value`: show it until the next
    /// report (or for [`HOLD_MS`]) instead of the stale prop.
    pub fn commit(&mut self, value: f32, now: f64) {
        self.held = Some((value, now));
    }

    /// Whether to schedule a timed redraw `delay` from `now`: one at a time,
    /// however often the window redraws for other reasons.
    pub fn claim_tick(&mut self, now: f64, delay: Duration) -> bool {
        if now < self.tick_due {
            return false;
        }
        self.tick_due = now + delay.as_secs_f64() * 1000.0;
        true
    }

    /// Advance to `now` (ms since the epoch) and say what to draw.
    pub fn step(&mut self, p: &Params, now: f64) -> Sample {
        if self.seen != (p.value, p.at) {
            self.seen = (p.value, p.at);
            self.received = now;
            self.held = None;
        }
        if self.held.is_some_and(|(_, t)| now - t > HOLD_MS) {
            self.held = None;
        }
        let level = match self.held {
            Some((v, t)) => advanced(v, p.rate, Some(t), now),
            None => advanced(p.value, p.rate, Some(p.at.unwrap_or(self.received)), now),
        };

        let dt = (((now - self.last) / 1000.0) as f32).clamp(0.0, MAX_DT);
        self.last = now;
        let target = p.wave.max(0.0);
        self.amplitude += (target - self.amplitude) * (dt * EASE).min(1.0);
        if (self.amplitude - target).abs() < 0.05 {
            self.amplitude = target;
        }
        // The wave travels while playing; pausing flattens it in place.
        if target > 0.0 {
            self.phase = (self.phase + dt * p.speed) % std::f32::consts::TAU;
        }
        let animating = target > 0.0 || self.amplitude != target;
        let tick = (!animating && p.rate != 0.0 && level > 0.0 && level < 1.0).then(|| {
            // About half a pixel of a wide bar per redraw, between 30 Hz and 1 Hz.
            let secs = 0.5 / (p.rate.abs() * 1000.0);
            Duration::from_secs_f32(secs.clamp(1.0 / 30.0, 1.0))
        });
        Sample {
            level,
            amplitude: self.amplitude,
            phase: self.phase,
            animating,
            tick,
        }
    }
}

/// The wave's height at `x` px along a filled part `cut` px long: a sine
/// that settles onto the line near the start and the thumb.
pub fn wave_y(x: f32, cut: f32, amplitude: f32, wavelength: f32, phase: f32) -> f32 {
    let ease = (x / 12.0).min((cut - x) / 12.0 + 0.25).clamp(0.0, 1.0);
    (x * std::f32::consts::TAU / wavelength.max(1.0) - phase).sin() * amplitude * ease
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(value: f32, rate: f32, at: Option<f64>, wave: f32) -> Params {
        Params {
            value,
            rate,
            at,
            wave,
            speed: 7.0,
        }
    }

    #[test]
    fn the_level_advances_from_the_report() {
        let p = params(0.5, 0.01, Some(1_000.0), 0.0);
        let mut m = Motion::new(&p, 1_000.0);
        let s = m.step(&p, 11_000.0);
        assert!((s.level - 0.6).abs() < 1e-5);
        assert!(!s.animating);
        assert!(s.tick.is_some(), "only the level moves: timed redraws");
        // Without `at`, from when the value arrived.
        let p = params(0.5, 0.01, None, 0.0);
        let mut m = Motion::new(&p, 5_000.0);
        assert!((m.step(&p, 15_000.0).level - 0.6).abs() < 1e-5);
        // Paused: still, no redraws.
        let p = params(0.5, 0.0, Some(1_000.0), 0.0);
        let s = Motion::new(&p, 0.0).step(&p, 9_000.0);
        assert_eq!((s.level, s.animating, s.tick), (0.5, false, None));
    }

    #[test]
    fn the_wave_moves_while_playing_and_flattens_when_paused() {
        let playing = params(0.3, 0.0, None, 3.5);
        let mut m = Motion::new(&playing, 0.0);
        let a = m.step(&playing, 16.0);
        let b = m.step(&playing, 32.0);
        assert_eq!(a.amplitude, 3.5, "no easing in on first sight");
        assert!(b.animating && b.phase > a.phase);

        let paused = Params {
            wave: 0.0,
            ..playing
        };
        let first = m.step(&paused, 48.0);
        assert!(first.amplitude > 0.0 && first.amplitude < 3.5, "eases out");
        assert_eq!(first.phase, b.phase, "the phase stops at once");
        let mut t = 48.0;
        let mut s = first;
        while s.animating && t < 2_000.0 {
            t += 16.0;
            s = m.step(&paused, t);
        }
        assert_eq!(s.amplitude, 0.0);
        assert!(!s.animating, "settled: no more frames");
    }

    #[test]
    fn a_commit_holds_until_the_next_report() {
        let p = params(0.2, 0.0, Some(0.0), 0.0);
        let mut m = Motion::new(&p, 0.0);
        m.commit(0.8, 100.0);
        assert_eq!(m.step(&p, 200.0).level, 0.8, "stale prop: the seek wins");
        let reported = params(0.79, 0.0, Some(250.0), 0.0);
        assert_eq!(m.step(&reported, 300.0).level, 0.79);
        m.commit(0.5, 400.0);
        assert_eq!(
            m.step(&reported, 400.0 + HOLD_MS + 1.0).level,
            0.79,
            "gives up"
        );
    }

    #[test]
    fn the_wave_settles_at_its_ends() {
        assert_eq!(wave_y(0.0, 100.0, 4.0, 26.0, 1.0), 0.0);
        let mid = (0..26).map(|x| wave_y(50.0 + x as f32, 100.0, 4.0, 26.0, 0.0).abs());
        assert!(mid.fold(0.0, f32::max) > 3.9);
        assert!(wave_y(100.0, 100.0, 4.0, 26.0, 0.0).abs() <= 1.0);
    }
}
