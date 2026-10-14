//! Throughput history (ADR 0187): a ring of one sample per second, and its
//! braille painting. Once for both frontends (ADR 0077): the terminal paints
//! [`braille`], the window draws [`Spark::series`] as SVG.
//!
//! Sampled with the painter's clock, never on progress alone: a stalled copy
//! publishes nothing, and a graph fed only by progress would freeze its last
//! speed on screen.

use std::collections::VecDeque;

/// Seconds of history kept.
pub const SAMPLES: usize = 32;
/// Terminal cells of a painted sparkline: each braille cell holds two
/// columns, each column the max of two seconds.
pub const CELLS: usize = 8;

/// One series of bytes per second, the last [`SAMPLES`] seconds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spark {
    ring: VecDeque<f64>,
    last_sec: Option<i64>,
}

impl Spark {
    /// Records `bps` in the second `now_ms` falls in; `None` (no rate yet)
    /// counts as nothing moving.
    ///
    /// The same second again replaces its sample: the last word wins.
    /// Skipped seconds take the NEW value — a gap is ticks that did not run,
    /// not a stop. A clock that jumps — back, or forward by more than the
    /// whole history, as a wall clock does when it is set — starts the
    /// series over: ignoring it froze the graph until time caught up.
    pub fn record(&mut self, now_ms: i64, bps: Option<f64>) {
        let sec = now_ms.div_euclid(1000);
        let v = bps.filter(|b| b.is_finite() && *b > 0.0).unwrap_or(0.0);
        let window = i64::try_from(SAMPLES).unwrap_or(i64::MAX);
        if self
            .last_sec
            .is_some_and(|last| sec < last || sec - last > window)
        {
            *self = Self::default();
        }
        match self.last_sec {
            Some(last) if sec == last => {
                if let Some(x) = self.ring.back_mut() {
                    *x = v;
                }
            }
            Some(last) => {
                let gap = usize::try_from(sec - last).map_or(SAMPLES, |g| g.min(SAMPLES));
                self.ring.extend(std::iter::repeat_n(v, gap));
            }
            None => self.ring.push_back(v),
        }
        self.last_sec = Some(sec);
        while self.ring.len() > SAMPLES {
            self.ring.pop_front();
        }
    }

    /// No sample moved anything: there is nothing worth drawing.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.ring.iter().all(|&x| x <= 0.0)
    }

    /// The series, oldest first, 0–100 against its own maximum, always
    /// [`SAMPLES`] long (padded with 0 on the left). A sample that moved at
    /// all is at least 1, so a trickle next to a burst still shows.
    #[must_use]
    pub fn series(&self) -> Vec<u8> {
        let max = self.ring.iter().copied().fold(0.0_f64, f64::max);
        let mut out = vec![0; SAMPLES - self.ring.len()];
        out.extend(self.ring.iter().map(|&x| {
            if x <= 0.0 || max <= 0.0 {
                return 0;
            }
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a positive ratio times 100, rounded and clamped: \
                          it fits a u8"
            )]
            let pct = (x / max * 100.0).round().clamp(1.0, 100.0) as u8;
            pct
        }));
        out
    }
}

/// A [`Spark::series`] as [`CELLS`] braille cells: column `i` is the max of
/// samples `2i` and `2i+1`, as 0–4 dots from the bottom (a sample that moved
/// is at least one dot). A shorter series is padded on the left.
///
/// ```
/// use norte_frontend::spark::braille;
/// let mut s = vec![0_u8; 16];
/// for v in [25, 50, 75, 100] {
///     s.extend([v; 4]);
/// }
/// assert_eq!(braille(&s), "\u{2800}\u{2800}\u{2800}\u{2800}⣀⣤⣶⣿");
/// ```
#[must_use]
pub fn braille(series: &[u8]) -> String {
    // Dots from the bottom up, left column and right column.
    const LEFT: [u32; 4] = [0x40, 0x04, 0x02, 0x01];
    const RIGHT: [u32; 4] = [0x80, 0x20, 0x10, 0x08];
    let tail = &series[series.len().saturating_sub(SAMPLES)..];
    let mut padded = vec![0_u8; SAMPLES - tail.len()];
    padded.extend_from_slice(tail);
    let dots = |v: u8| (usize::from(v.min(100)) * 4).div_ceil(100);
    padded
        .chunks(4)
        .map(|c| {
            let (l, r) = (dots(c[0].max(c[1])), dots(c[2].max(c[3])));
            let bits = LEFT[..l].iter().sum::<u32>() + RIGHT[..r].iter().sum::<u32>();
            char::from_u32(0x2800 + bits).unwrap_or(' ')
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn the_same_second_replaces_its_sample() {
        let mut s = Spark::default();
        s.record(1_000, Some(10.0));
        s.record(1_900, Some(40.0));
        assert_eq!(s.ring, [40.0]);
    }

    #[test]
    fn a_gap_is_filled_with_the_new_value() {
        let mut s = Spark::default();
        s.record(0, Some(10.0));
        s.record(3_000, Some(20.0));
        assert_eq!(s.ring, [10.0, 20.0, 20.0, 20.0]);
    }

    #[test]
    fn a_clock_that_jumps_starts_over() {
        let mut s = Spark::default();
        s.record(5_000, Some(10.0));
        s.record(4_000, Some(99.0));
        assert_eq!(s.ring, [99.0], "back: the graph keeps moving");
        s.record(5_000, Some(1.0));
        s.record(500_000, Some(7.0));
        assert_eq!(s.ring, [7.0], "far forward: no flat half minute");
    }

    #[test]
    fn the_ring_keeps_only_the_last_samples() {
        let mut s = Spark::default();
        for i in 0..100 {
            s.record(i * 1_000, Some(1.0));
        }
        assert_eq!(s.ring.len(), SAMPLES);
        s.record(131_000, Some(2.0));
        assert_eq!(
            s.ring.len(),
            SAMPLES,
            "a gap within the window does not grow it"
        );
    }

    #[test]
    fn the_series_is_against_its_own_maximum_and_a_trickle_shows() {
        let mut s = Spark::default();
        s.record(0, Some(1.0));
        s.record(1_000, Some(1_000_000.0));
        s.record(2_000, None);
        let series = s.series();
        assert_eq!(series.len(), SAMPLES);
        assert_eq!(&series[SAMPLES - 3..], [1, 100, 0]);
        assert!(series[..SAMPLES - 3].iter().all(|&v| v == 0));
    }

    #[test]
    fn quiet_until_something_moves() {
        let mut s = Spark::default();
        assert!(s.is_quiet());
        s.record(0, None);
        assert!(s.is_quiet());
        s.record(1_000, Some(5.0));
        assert!(!s.is_quiet());
    }

    #[test]
    fn braille_of_nothing_and_of_everything() {
        assert_eq!(braille(&[0; SAMPLES]), "\u{2800}".repeat(CELLS));
        assert_eq!(braille(&[100; SAMPLES]), "⣿".repeat(CELLS));
        assert_eq!(braille(&[]), "\u{2800}".repeat(CELLS));
    }

    #[test]
    fn a_sample_that_moved_is_at_least_one_dot() {
        let mut s = vec![0_u8; SAMPLES];
        s[SAMPLES - 1] = 1;
        assert!(braille(&s).ends_with('\u{2880}'), "{}", braille(&s));
    }

    proptest! {
        #[test]
        fn braille_is_always_cells_wide(series in proptest::collection::vec(any::<u8>(), 0..80)) {
            prop_assert_eq!(braille(&series).chars().count(), CELLS);
        }
    }
}
