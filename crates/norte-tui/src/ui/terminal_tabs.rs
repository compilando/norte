//! The terminal panel's tab strip, in its top border: which instances fit,
//! where each one starts (for the mouse), and where the list was cut.
//! Pure: the painting is `panels.rs`'s.

use norte_frontend::terminals::InstanceId;

/// Between two tabs.
pub(crate) const SEP: &str = " │ ";
/// Where the strip was cut, on that side.
pub(crate) const CUT: &str = "… ";

/// What fits of the strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Strip {
    /// The tabs that fit, in list order.
    pub items: Vec<StripItem>,
    /// Some were left out on the left.
    pub cut_left: bool,
    /// Some were left out on the right.
    pub cut_right: bool,
}

/// One tab: its text, and where it is (relative to the strip's start).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StripItem {
    pub id: InstanceId,
    pub label: String,
    pub active: bool,
    pub x: u16,
    pub width: u16,
}

/// Display COLUMNS, as ratatui paints them: a CJK character is two.
fn chars(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Lays `labels` out in `width` columns, growing outwards from `active` —
/// the one tab that must always be seen.
pub(crate) fn layout_strip(
    labels: &[(InstanceId, String)],
    active: InstanceId,
    width: u16,
) -> Strip {
    let width = usize::from(width);
    let sep = chars(SEP);
    let cut = chars(CUT);
    let Some(at) = labels.iter().position(|(id, _)| *id == active) else {
        return Strip {
            items: Vec::new(),
            cut_left: false,
            cut_right: false,
        };
    };
    let cost = |lo: usize, hi: usize| -> usize {
        let body: usize =
            labels[lo..=hi].iter().map(|(_, l)| chars(l)).sum::<usize>() + sep * (hi - lo);
        body + if lo > 0 { cut } else { 0 } + if hi + 1 < labels.len() { cut } else { 0 }
    };
    let (mut lo, mut hi) = (at, at);
    loop {
        let grown_right = hi + 1 < labels.len() && cost(lo, hi + 1) <= width;
        if grown_right {
            hi += 1;
        }
        let grown_left = lo > 0 && cost(lo - 1, hi) <= width;
        if grown_left {
            lo -= 1;
        }
        if !grown_right && !grown_left {
            break;
        }
    }
    let (cut_left, cut_right) = (lo > 0, hi + 1 < labels.len());
    let room =
        width.saturating_sub(if cut_left { cut } else { 0 } + if cut_right { cut } else { 0 });
    let mut x = if cut_left { cut } else { 0 };
    let items = labels[lo..=hi]
        .iter()
        .map(|(id, label)| {
            // Only the active one can be alone and too wide: shortened.
            let label = if chars(label) > room {
                // Column by column, leaving one for the `…`.
                let mut s = String::new();
                let mut used = 0;
                for c in label.chars() {
                    let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                    if used + w > room.saturating_sub(1) {
                        break;
                    }
                    used += w;
                    s.push(c);
                }
                s.push('…');
                s
            } else {
                label.clone()
            };
            let w = chars(&label);
            let item = StripItem {
                id: *id,
                label,
                active: *id == active,
                x: u16::try_from(x).unwrap_or(u16::MAX),
                width: u16::try_from(w).unwrap_or(u16::MAX),
            };
            x += w + sep;
            item
        })
        .collect();
    Strip {
        items,
        cut_left,
        cut_right,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_frontend::terminals::InstanceId;

    fn labels(n: u32) -> Vec<(InstanceId, String)> {
        (1..=n)
            .map(|i| (InstanceId(i), format!("{i} sh")))
            .collect()
    }

    #[test]
    fn everything_fits() {
        let s = layout_strip(&labels(3), InstanceId(2), 40);
        assert_eq!(s.items.len(), 3);
        assert!(!s.cut_left && !s.cut_right);
        assert!(s.items.windows(2).all(|w| w[0].x + w[0].width < w[1].x));
        assert!(s.items[1].active);
    }

    /// Too many: the strip keeps the active one and cuts around it.
    #[test]
    fn cut_around_the_active() {
        let s = layout_strip(&labels(10), InstanceId(7), 20);
        assert!(s.items.iter().any(|i| i.id == InstanceId(7)));
        assert!(s.cut_left && s.cut_right);
        let last = s.items.last().expect("one");
        assert!(last.x + last.width <= 20);
    }

    /// One label wider than the strip: still there, shortened with `…`.
    #[test]
    fn the_active_is_kept_even_if_alone_too_wide() {
        let wide = vec![(InstanceId(1), "x".repeat(50))];
        let s = layout_strip(&wide, InstanceId(1), 10);
        assert_eq!(s.items.len(), 1);
        assert!(s.items[0].label.ends_with('…'));
        assert!(s.items[0].width <= 10);
    }

    /// A wide character takes TWO columns, as ratatui paints it: counted as
    /// one, every later tab drifts and a click picks the wrong shell.
    #[test]
    fn a_wide_title_counts_its_columns() {
        let l = vec![
            (InstanceId(1), "1 文档".to_owned()),
            (InstanceId(2), "2 sh".to_owned()),
        ];
        let s = layout_strip(&l, InstanceId(1), 40);
        assert_eq!(s.items[0].width, 6);
        assert_eq!(s.items[1].x, 6 + 3);
    }

    /// Widths are counted in characters, not bytes.
    #[test]
    fn widths_count_chars_not_bytes() {
        let l = vec![(InstanceId(1), "ñandú".to_owned())];
        let s = layout_strip(&l, InstanceId(1), 40);
        assert_eq!(s.items[0].width, 5);
    }
}
