//! `org.norte.size-bar`: the size of each file as a small bar.
//!
//! Implements the `norte-columns` world: the host hands over the names of
//! the visible page and a location token, and gets one cell per name. The
//! bar itself is a pure function over sizes with its own tests; the WIT glue
//! below only exists when compiled AS a component, so the host-side tests
//! build the same crate without it.

/// The one column this plugin contributes.
pub const COLUMN_ID: &str = "bar";

/// What fills the bar on the `absolute` scale: one gibibyte.
pub const ABSOLUTE_CEILING: u64 = 1 << 30;

/// The bar's length, in cells, when the setting is missing or unreadable.
pub const DEFAULT_WIDTH: u8 = 5;

/// How a size maps to a length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    /// Proportional: half the ceiling is half the bar.
    Linear,
    /// Logarithmic: bytes to gigabytes spread over the bar.
    Log,
}

/// What fills the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelativeTo {
    /// The biggest file on the visible page.
    Page,
    /// [`ABSOLUTE_CEILING`].
    Absolute,
}

/// The three settings, already parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub scale: Scale,
    pub width: u8,
    pub relative_to: RelativeTo,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            scale: Scale::Log,
            width: DEFAULT_WIDTH,
            relative_to: RelativeTo::Page,
        }
    }
}

impl Params {
    /// From the three `[config]` values as the host hands them over
    /// (strings, `None` when absent). Anything unreadable is the default:
    /// the manifest validates them first, so this is belt and braces.
    pub fn parse(scale: Option<&str>, width: Option<&str>, relative_to: Option<&str>) -> Self {
        let d = Self::default();
        Self {
            scale: match scale {
                Some("linear") => Scale::Linear,
                Some("log") => Scale::Log,
                _ => d.scale,
            },
            width: width
                .and_then(|w| w.trim().parse::<u8>().ok())
                .map_or(d.width, |w| w.clamp(3, 8)),
            relative_to: match relative_to {
                Some("absolute") => RelativeTo::Absolute,
                Some("page") => RelativeTo::Page,
                _ => d.relative_to,
            },
        }
    }
}

/// How much of the bar `size` fills, in `0.0..=1.0`.
fn ratio(size: u64, ceiling: u64, scale: Scale) -> f64 {
    if size == 0 || ceiling == 0 {
        return 0.0;
    }
    let r = match scale {
        Scale::Linear => size as f64 / ceiling as f64,
        // `ln_1p` so that a ceiling of 1 does not divide by zero.
        Scale::Log => (size as f64).ln_1p() / (ceiling as f64).ln_1p(),
    };
    r.clamp(0.0, 1.0)
}

/// One bar. A file with any bytes at all shows at least one block: a bar
/// that reads as empty says «zero», and zero is a different thing.
pub fn bar(size: u64, ceiling: u64, p: &Params) -> String {
    let width = usize::from(p.width);
    let mut filled = (ratio(size, ceiling, p.scale) * width as f64).round() as usize;
    if size > 0 && filled == 0 {
        filled = 1;
    }
    let filled = filled.min(width);
    let mut s = String::with_capacity(width * 3);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('░');
    }
    s
}

/// The cells of a page: `None` where there is no size (a directory, an
/// entry the host could not stat). The ceiling is decided ONCE per page.
pub fn cells(sizes: &[Option<u64>], p: &Params) -> Vec<Option<String>> {
    let ceiling = match p.relative_to {
        RelativeTo::Page => sizes.iter().flatten().copied().max().unwrap_or(0),
        RelativeTo::Absolute => ABSOLUTE_CEILING,
    };
    sizes.iter().map(|s| s.map(|n| bar(n, ceiling, p))).collect()
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

    struct SizeBar;

    impl ColumnsGuest for SizeBar {
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
                host_config::get("scale").as_deref(),
                host_config::get("width").as_deref(),
                host_config::get("relative-to").as_deref(),
            );
            let sizes: Vec<Option<u64>> = entries
                .iter()
                .map(|name| {
                    let mut rel = loc.prefix.clone();
                    if !rel.is_empty() && rel.last() != Some(&b'/') {
                        rel.push(b'/');
                    }
                    rel.extend_from_slice(name);
                    let meta = location::stat(&loc.token, &rel).ok()?;
                    // A directory's size is not what is on disk: no bar.
                    match meta.kind {
                        location::EntryKind::File => Some(meta.size),
                        _ => None,
                    }
                })
                .collect();
            cells(&sizes, &params)
        }
    }

    export!(SizeBar);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear(width: u8) -> Params {
        Params {
            scale: Scale::Linear,
            width,
            relative_to: RelativeTo::Page,
        }
    }

    #[test]
    fn a_linear_bar_is_proportional_and_the_ceiling_fills_it() {
        let p = linear(4);
        assert_eq!(bar(100, 100, &p), "████");
        assert_eq!(bar(50, 100, &p), "██░░");
        assert_eq!(bar(0, 100, &p), "░░░░", "zero is empty");
        assert_eq!(bar(1, 100, &p), "█░░░", "but a byte shows");
        assert_eq!(bar(500, 100, &p), "████", "over the ceiling is full");
    }

    #[test]
    fn a_log_bar_grows_with_every_order_of_magnitude() {
        let p = Params::default();
        let ceiling = ABSOLUTE_CEILING;
        let sizes = [1u64, 1 << 10, 1 << 20, 1 << 30];
        let filled: Vec<usize> = sizes
            .iter()
            .map(|s| bar(*s, ceiling, &p).chars().filter(|c| *c == '█').count())
            .collect();
        assert!(
            filled.windows(2).all(|w| w[0] < w[1]),
            "strictly more blocks per magnitude: {filled:?}"
        );
        assert_eq!(filled[3], 5, "the ceiling fills it");
    }

    #[test]
    fn the_page_ceiling_is_the_biggest_file_and_directories_get_nothing() {
        let p = linear(2);
        let page = cells(&[Some(10), None, Some(5), Some(0)], &p);
        assert_eq!(
            page,
            vec![
                Some("██".to_owned()),
                None,
                Some("█░".to_owned()),
                Some("░░".to_owned())
            ]
        );
        assert_eq!(cells(&[None, None], &p), vec![None, None], "a page of folders");
    }

    #[test]
    fn the_settings_parse_and_clamp() {
        let p = Params::parse(Some("linear"), Some("12"), Some("absolute"));
        assert_eq!(p.scale, Scale::Linear);
        assert_eq!(p.width, 8, "clamped to the manifest's max");
        assert_eq!(p.relative_to, RelativeTo::Absolute);
        let d = Params::parse(None, Some("junk"), Some("nope"));
        assert_eq!(d, Params::default());
        assert_eq!(Params::parse(None, Some("1"), None).width, 3, "and to its min");
    }

    #[test]
    fn a_bar_is_exactly_width_cells() {
        for w in 3..=8u8 {
            let p = linear(w);
            for size in [0u64, 1, 37, 100] {
                assert_eq!(bar(size, 100, &p).chars().count(), usize::from(w));
            }
        }
    }
}
