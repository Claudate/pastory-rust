//! Motion: the one animation primitive the 晾衣绳 form needs.
//!
//! The 面板 form animates only through `NSAnimationContext` — an open/close
//! fade and a 28pt slide — so nothing in it moves once the tree is drawn. The
//! line form hangs photos from a rope and lets them swing, which needs a frame
//! clock: a 30 Hz `NSTimer` that steps each card's angle and asks the view to
//! repaint. That clock runs *only* while the line form is up (see
//! `line_panel::install_tick`), so the panel form pays nothing for it.
//!
//! Why a hand-rolled spring and not `CASpringAnimation`: every card swings on
//! its own random wind, so each needs its own animation and its own delay.
//! Layer animations would mean one layer per card and a Core Animation
//! dependency for what is a closed-form two-line solution — and a self-test
//! could not then freeze a specific frame. [`Spring`] is that closed form
//! (the SwiftUI `interpolatingSpring(stiffness:damping:)` pair), and
//! [`wind_angle`] is the per-card breeze, both pure functions of `t` so a
//! frame is reproducible from a seed.

use std::f64::consts::PI;

/// A damped spring from `from` to `to`, released at rest, sampled at `t`
/// seconds. `mass` is fixed at 1 (SwiftUI's default), so `stiffness` and
/// `damping` carry the whole shape.
///
/// The solution is the textbook underdamped oscillator
/// `x(t) = to − (to−from)·e^(−ζωt)·[cos(ω_d t) + (ζω/ω_d)·sin(ω_d t)]`;
/// an overdamped or critically-damped pair (`ζ ≥ 1`) degenerates to the
/// critically-damped form, which is why both branches exist.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    pub from: f64,
    pub to: f64,
    pub stiffness: f64,
    pub damping: f64,
}

impl Spring {
    pub fn new(from: f64, to: f64, stiffness: f64, damping: f64) -> Spring {
        Spring { from, to, stiffness, damping }
    }

    fn omega(&self) -> f64 {
        self.stiffness.max(0.0).sqrt()
    }

    /// ζ = c / (2·√(k·m)), m = 1.
    fn zeta(&self) -> f64 {
        self.damping / (2.0 * self.omega().max(1e-9))
    }

    /// The value at `t` seconds after release.
    pub fn value(&self, t: f64) -> f64 {
        if t <= 0.0 {
            return self.from;
        }
        let w = self.omega();
        if w <= 1e-9 {
            return self.to;
        }
        let z = self.zeta();
        let d = self.to - self.from;
        if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            self.to
                - d * (-z * w * t).exp() * ((wd * t).cos() + (z * w / wd) * (wd * t).sin())
        } else {
            // Critically / over damped: 1 − (1 + ωt)e^(−ωt) scaled to the span.
            self.to - d * (1.0 + w * t) * (-w * t).exp()
        }
    }

    /// True while the spring is still visibly moving at `t` (an amplitude
    /// above a thousandth of the span, the threshold the tick uses to decide
    /// whether another frame is worth drawing).
    pub fn moving(&self, t: f64) -> bool {
        (self.value(t) - self.to).abs() > (self.to - self.from).abs() * 1e-3 + 1e-4
    }
}

/// The attach swing: a newly hung photo starts tilted and settles upright.
/// 16° at hang, `interpolatingSpring(stiffness: 46, damping: 2.6)` — the pair
/// Tendedero uses, underdamped so it overshoots once and comes to rest.
pub const ATTACH_DEG: f64 = 16.0;
pub fn attach_spring() -> Spring {
    Spring::new(ATTACH_DEG, 0.0, 46.0, 2.6)
}

/// Tendedero's breeze: each photo leans by its own small angle that drifts on
/// its own period, starting after its own delay, so a row of them never moves
/// in lockstep. `seed` is the card's own (an id hash), so the same card always
/// gets the same breeze — and a self-test can freeze it.
///
/// Ranges are Tendedero's: amplitude 1.6–3.4°, delay 0–0.35 s.
pub fn wind_angle(seed: u64, t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    let f = |salt: u64| -> f64 {
        // SplitMix-ish scramble → [0,1).
        let mut z = seed.wrapping_add(salt.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        z ^= z >> 30;
        z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z ^= z >> 27;
        z = z.wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    };
    let amp = 1.6 + 1.8 * f(1);
    let delay = 0.35 * f(2);
    let period = 2.2 + 1.4 * f(3);
    let phase = 2.0 * PI * f(4);
    if t <= delay {
        return 0.0;
    }
    let local = t - delay;
    let fade = (local / 0.6).min(1.0);
    amp * (2.0 * PI * local / period + phase).sin() * fade
}

/// A card's full tilt at `t`: the settle of the attach swing plus its breeze.
pub fn card_angle(seed: u64, t: f64) -> f64 {
    attach_spring().value(t) + wind_angle(seed, t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_starts_at_from_and_rests_at_to() {
        let s = attach_spring();
        assert_eq!(s.value(0.0), ATTACH_DEG);
        assert!((s.value(4.0) - 0.0).abs() < 0.05, "rests upright: {}", s.value(4.0));
        // Underdamped: it must cross zero at least once (the overshoot).
        let mut crossed = false;
        let mut prev = s.value(0.0);
        let mut t = 0.0;
        while t < 2.0 {
            t += 0.01;
            let v = s.value(t);
            if prev > 0.0 && v <= 0.0 {
                crossed = true;
            }
            prev = v;
        }
        assert!(crossed, "an underdamped attach swing overshoots");
    }

    #[test]
    fn spring_is_monotone_when_overdamped() {
        let s = Spring::new(10.0, 0.0, 20.0, 40.0);
        let mut prev = s.from;
        let mut t = 0.0;
        while t < 3.0 {
            t += 0.02;
            let v = s.value(t);
            assert!(v <= prev + 1e-9, "overdamped must not overshoot");
            prev = v;
        }
        assert!((s.value(6.0)).abs() < 0.1);
    }

    #[test]
    fn wind_is_bounded_and_delayed() {
        for seed in [0u64, 1, 42, 999, u64::MAX] {
            assert_eq!(wind_angle(seed, 0.0), 0.0);
            let mut t = 0.0;
            while t < 20.0 {
                t += 0.05;
                assert!(wind_angle(seed, t).abs() <= 3.4 + 1e-9, "breeze stays small");
            }
        }
    }

    #[test]
    fn same_seed_same_frame() {
        // A frozen frame is the whole point of the seed: two calls agree.
        assert_eq!(card_angle(7, 0.83), card_angle(7, 0.83));
        assert_ne!(wind_angle(1, 1.0), wind_angle(2, 1.0));
    }
}
