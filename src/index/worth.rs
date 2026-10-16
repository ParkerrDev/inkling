//! How much a command is worth: its runs, the old ones fading.
//!
//! A command's worth in a scope is the sum, over its runs there, of each
//! run's weight times `e^(-age / fade)`, where the age of a run is how many
//! commands were run after it, anywhere. That sum changes with every command
//! run, so it is kept as an anchor that does not: `ln(worth) + now / fade`.
//! A run adds its own term to the anchor, and two anchors made with the same
//! fade compare as the worths do, at any moment. The share of a scope a
//! command has is then `e^(anchor - mass)`, where the mass is the anchor of
//! the sum over every command in it, and no clock enters at all.

/// The anchor of nothing.
pub const NONE: f64 = f64::NEG_INFINITY;

/// `ln(e^a + e^b)`, without leaving the range of a float.
#[must_use]
pub fn log_add(a: f64, b: f64) -> f64 {
    if a.is_infinite() && a < 0.0 {
        return b;
    }
    if b.is_infinite() && b < 0.0 {
        return a;
    }
    let (high, low) = if a >= b { (a, b) } else { (b, a) };
    high + (low - high).exp().ln_1p()
}

/// The term a run at `seq` with weight `e^ln_weight` adds to an anchor.
#[must_use]
pub fn term(seq: u32, ln_weight: f64, fade: f64) -> f64 {
    ln_weight + f64::from(seq) / fade
}

/// The share `e^(anchor - mass)`, which is 0 for a command with no runs.
#[must_use]
pub fn share(anchor: f64, mass: f64) -> f64 {
    if anchor.is_infinite() || mass.is_infinite() {
        return 0.0;
    }
    (anchor - mass).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn adding_in_the_log_is_adding() {
        assert!(close(log_add(2_f64.ln(), 3_f64.ln()), 5_f64.ln()));
        assert!(close(log_add(NONE, 1.5), 1.5));
        assert!(close(log_add(1.5, NONE), 1.5));
        assert!(log_add(NONE, NONE).is_infinite());
        // Far apart, the smaller is lost and nothing overflows.
        assert!(close(log_add(50_000.0, 1.0), 50_000.0));
    }

    #[test]
    fn a_share_does_not_depend_on_when_it_is_asked() {
        // One command run at 10 and 20, another at 15, fade 10: whatever
        // the time now, the first holds the same share of the two.
        let fade = 10.0;
        let first = log_add(term(10, 0.0, fade), term(20, 0.0, fade));
        let second = term(15, 0.0, fade);
        let mass = log_add(first, second);
        let worth = |seq: f64, now: f64| (-(now - seq) / fade).exp();
        for now in [20.0, 100.0, 1_000.0] {
            let a = worth(10.0, now) + worth(20.0, now);
            let b = worth(15.0, now);
            assert!(close(share(first, mass), a / (a + b)));
        }
        assert!(close(share(first, mass) + share(second, mass), 1.0));
        assert!(close(share(NONE, mass), 0.0));
    }
}
