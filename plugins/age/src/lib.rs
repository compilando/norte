//! `org.norte.age`: how long ago each entry changed, in a column.
//!
//! Implements the `norte-columns` world: the host hands over the names of
//! the visible page and a location token, and gets one cell per name. The
//! bucketing and the short figure are pure functions with their own tests;
//! the WIT glue below only exists when compiled AS a component, so the
//! host-side tests build the same crate without it.

/// The one column this plugin contributes.
pub const COLUMN_ID: &str = "age";

/// Seconds in a day, the unit of `thresholds`.
const DAY: i64 = 86_400;

/// What the cell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Glyph, a space, figure.
    Both,
    /// The glyph alone.
    Glyph,
    /// The figure alone.
    Text,
}

/// The three settings, already parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params {
    /// Bucket edges in days, ascending, no duplicates, never empty.
    pub thresholds: Vec<u64>,
    /// One glyph per bucket, freshest first; never empty.
    pub glyphs: Vec<char>,
    pub format: Format,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            thresholds: vec![1, 7, 30],
            glyphs: vec!['●', '◐', '○', '·'],
            format: Format::Both,
        }
    }
}

impl Params {
    /// From the three `[config]` values as the host hands them over
    /// (strings, `None` when absent). A threshold list with nothing usable
    /// in it, or an empty glyph string, falls back to the default: the cell
    /// has to say something.
    pub fn parse(thresholds: Option<&str>, glyphs: Option<&str>, format: Option<&str>) -> Self {
        let d = Self::default();
        let mut edges: Vec<u64> = thresholds
            .unwrap_or("")
            .split(',')
            .filter_map(|s| s.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .collect();
        edges.sort_unstable();
        edges.dedup();
        let glyphs: Vec<char> = glyphs
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_whitespace() && !c.is_control())
            .collect();
        Self {
            thresholds: if edges.is_empty() { d.thresholds } else { edges },
            glyphs: if glyphs.is_empty() { d.glyphs } else { glyphs },
            format: match format {
                Some("glyph") => Format::Glyph,
                Some("text") => Format::Text,
                _ => Format::Both,
            },
        }
    }
}

/// Which bucket an age falls in: the index of the first edge it does not
/// exceed, or one past the last edge for anything older.
pub fn bucket(age_secs: i64, thresholds: &[u64]) -> usize {
    let age = age_secs.max(0);
    thresholds
        .iter()
        .position(|days| age <= i64::try_from(*days).unwrap_or(i64::MAX).saturating_mul(DAY))
        .unwrap_or(thresholds.len())
}

/// The short figure: the largest unit that gives a whole number ≥ 1, with
/// `now` under a minute. A modification time in the future reads as now.
pub fn figure(age_secs: i64) -> String {
    let s = age_secs.max(0);
    let (n, unit) = if s < 60 {
        return "now".to_owned();
    } else if s < 3_600 {
        (s / 60, "m")
    } else if s < DAY {
        (s / 3_600, "h")
    } else if s < 7 * DAY {
        (s / DAY, "d")
    } else if s < 30 * DAY {
        (s / (7 * DAY), "w")
    } else if s < 365 * DAY {
        (s / (30 * DAY), "mo")
    } else {
        (s / (365 * DAY), "y")
    };
    format!("{n}{unit}")
}

/// One cell.
pub fn cell(age_secs: i64, p: &Params) -> String {
    let b = bucket(age_secs, &p.thresholds);
    // Fewer glyphs than buckets: the last one repeats.
    let glyph = p.glyphs.get(b).or(p.glyphs.last()).copied().unwrap_or('·');
    match p.format {
        Format::Glyph => glyph.to_string(),
        Format::Text => figure(age_secs),
        Format::Both => format!("{glyph} {}", figure(age_secs)),
    }
}

/// The cells of a page from each entry's modification time (`None` where
/// the host could not stat), against `now`.
pub fn cells(mtimes: &[Option<i64>], now: i64, p: &Params) -> Vec<Option<String>> {
    mtimes
        .iter()
        .map(|m| m.map(|t| cell(now.saturating_sub(t), p)))
        .collect()
}

#[cfg(target_arch = "wasm32")]
mod guest {
    wit_bindgen::generate!({
        world: "norte-columns",
        path: "wit",
        generate_all,
    });

    use exports::norte::plugin::columns::{Guest as ColumnsGuest, LocationRef};
    use norte::host::host_config;
    use norte::location::location;

    use crate::{cells, Params, COLUMN_ID};

    struct Age;

    impl ColumnsGuest for Age {
        fn column_values(
            id: String,
            location: Option<LocationRef>,
            entries: Vec<Vec<u8>>,
        ) -> Vec<Option<String>> {
            if id != COLUMN_ID {
                return entries.iter().map(|_| None).collect();
            }
            // Without a location there is nothing to measure, and empty
            // cells are the right answer: the panel keeps painting.
            let Some(loc) = location else {
                return entries.iter().map(|_| None).collect();
            };
            let params = Params::parse(
                host_config::get("thresholds").as_deref(),
                host_config::get("glyphs").as_deref(),
                host_config::get("format").as_deref(),
            );
            // The wall clock comes from WASI, which the host links for every
            // guest. Seconds are enough: no bucket edge is finer than a day.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
            let mtimes: Vec<Option<i64>> = entries
                .iter()
                .map(|name| {
                    let mut rel = loc.prefix.clone();
                    if !rel.is_empty() && rel.last() != Some(&b'/') {
                        rel.push(b'/');
                    }
                    rel.extend_from_slice(name);
                    location::stat(&loc.token, &rel).ok().map(|m| m.mtime_sec)
                })
                .collect();
            cells(&mtimes, now, &params)
        }
    }

    export!(Age);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_follow_the_edges_and_older_is_one_past_the_last() {
        let t = [1u64, 7, 30];
        assert_eq!(bucket(0, &t), 0, "just now is today");
        assert_eq!(bucket(DAY, &t), 0, "the edge belongs to the fresher bucket");
        assert_eq!(bucket(DAY + 1, &t), 1);
        assert_eq!(bucket(7 * DAY, &t), 1);
        assert_eq!(bucket(8 * DAY, &t), 2);
        assert_eq!(bucket(31 * DAY, &t), 3, "older than the last edge");
        assert_eq!(bucket(-500, &t), 0, "a future mtime is now");
    }

    #[test]
    fn the_figure_uses_the_largest_whole_unit() {
        assert_eq!(figure(30), "now");
        assert_eq!(figure(90), "1m");
        assert_eq!(figure(3 * 3_600 + 5), "3h");
        assert_eq!(figure(2 * DAY), "2d");
        assert_eq!(figure(20 * DAY), "2w");
        assert_eq!(figure(100 * DAY), "3mo");
        assert_eq!(figure(800 * DAY), "2y");
        assert_eq!(figure(-1), "now");
    }

    #[test]
    fn a_cell_pairs_the_bucket_glyph_with_the_figure() {
        let p = Params::default();
        assert_eq!(cell(3_600, &p), "● 1h");
        assert_eq!(cell(3 * DAY, &p), "◐ 3d");
        assert_eq!(cell(100 * DAY, &p), "· 3mo");
        let solo = Params {
            format: Format::Glyph,
            ..Params::default()
        };
        assert_eq!(cell(3 * DAY, &solo), "◐");
        let texto = Params {
            format: Format::Text,
            ..Params::default()
        };
        assert_eq!(cell(3 * DAY, &texto), "3d");
    }

    #[test]
    fn fewer_glyphs_than_buckets_repeat_the_last() {
        let p = Params::parse(Some("1,7,30"), Some("!."), Some("glyph"));
        assert_eq!(cell(0, &p), "!");
        assert_eq!(cell(3 * DAY, &p), ".");
        assert_eq!(cell(100 * DAY, &p), ".");
    }

    #[test]
    fn the_settings_parse_sort_and_fall_back() {
        let p = Params::parse(Some(" 30, 1,x,7,7,0"), Some(" ab "), Some("text"));
        assert_eq!(p.thresholds, vec![1, 7, 30], "sorted, deduped, junk dropped");
        assert_eq!(p.glyphs, vec!['a', 'b']);
        assert_eq!(p.format, Format::Text);
        let d = Params::parse(Some("x,y"), Some("  "), None);
        assert_eq!(d, Params::default(), "nothing usable means the default");
    }

    #[test]
    fn a_page_is_measured_against_now_and_unstattable_rows_stay_empty() {
        let p = Params {
            format: Format::Text,
            ..Params::default()
        };
        let now = 1_000_000;
        assert_eq!(
            cells(&[Some(now - 2 * DAY), None, Some(now + 50)], now, &p),
            vec![Some("2d".to_owned()), None, Some("now".to_owned())]
        );
    }
}
