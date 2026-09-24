//! The RIGHT half of the status bar: informational items the reader picks
//! (`[ui] status_items`) and that are clickable (ADR 0132).
//!
//! The left half does not go through here and is not configurable: it is
//! where messages, waits and WARNINGS go (incomplete listing,
//! reinterpreted names, pruned marks), and a warning a configuration could
//! remove would stop being a warning.
//!
//! What each item says, at what priority it gives way and what command it
//! runs is decided HERE, once, for the TUI and for the window (ADR 0077).
//! The frontends gather the facts ([`StatusInput`]) and paint.

use norte_config::{StatusItem, StatusItems};
use norte_i18n::{Lang, t_in, ta_in};

use crate::sort::{SortColumn, SortDir, SortSpec};

/// The facts of the moment, of the pane with the keyboard and of the
/// program.
///
/// Not `Copy` since the sort can name an attribute (ADR 0144).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusInput {
    /// `(cursor + 1, total)`, or `None` with an active filter: with the
    /// filter the real position is not the one shown, and a `3/120` would
    /// mislead.
    pub position: Option<(usize, usize)>,
    /// Marked entries.
    pub marked: usize,
    /// How much the marked ones that declare a size weigh.
    pub marked_bytes: u64,
    /// How many of the marked ones are directories.
    pub marked_dirs: usize,
    /// The listing's sort.
    pub sort: SortSpec,
    /// The pane's name reinterpretation; `None` = the bytes as they are,
    /// read as UTF-8.
    pub encoding: Option<norte_encoding::NameEncoding>,
    /// The light progress bar (ADR 0146): what the `tasks` item says now,
    /// or nothing — before the threshold, or with no work.
    pub strip: Option<crate::task_strip::StripView>,
    /// Notices that expired unread.
    pub notices: u32,
}

impl StatusInput {
    /// The facts of `pane` plus the program's. ONE rule for both frontends:
    /// with the FILTER active there is no position (review MINOR-2 T4: the
    /// selection is not the real cursor, and the footer already gives the
    /// honest `n/m`).
    #[must_use]
    pub fn from_pane(
        pane: &crate::PaneState,
        strip: Option<crate::task_strip::StripView>,
        notices: u32,
    ) -> Self {
        let total = pane.entries().len();
        let pos = if total == 0 { 0 } else { pane.cursor() + 1 };
        Self {
            position: pane.quick_visible().is_none().then_some((pos, total)),
            marked: pane.marks_len(),
            marked_bytes: pane.marked_bytes(),
            marked_dirs: pane.marked_dirs(),
            sort: pane.sort(),
            encoding: pane.name_encoding(),
            strip,
            notices,
        }
    }
}

/// An item, already worded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusItemView {
    /// The stable id (`position`, `marks`…, or `plugin:<plugin>/<column>`
    /// for a plugin's): `norte.toml`'s, and the one that comes back with a
    /// click.
    pub id: String,
    /// The text, in the requested language.
    pub text: String,
    /// What it is and, if clicked, what it does.
    pub tooltip: String,
    /// The command a click runs, from the catalogue; `None` = not
    /// clickable.
    pub command: Option<&'static str>,
    /// Higher = gives way later when they do not all fit.
    pub priority: u8,
    /// Whether the progress bar goes after the text (ADR 0146): only the
    /// `tasks` item, with work under way.
    pub bar: bool,
    /// The progress that bar paints, and what phase the burst is in.
    pub progress: Option<ItemProgress>,
    /// Shorter forms of the same item, from longest to shortest: [`fit`]
    /// tries them before dropping any item.
    pub shorter: Vec<crate::task_strip::Form>,
}

/// The `tasks` item's progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemProgress {
    /// Out of the total; `None` = unknown (a pulsing bar, not a 0%).
    pub percent: Option<u8>,
    /// Running, done or failed.
    pub phase: crate::task_strip::StripPhase,
}

impl StatusItemView {
    /// The cells it takes up, with its bar if it carries one.
    #[must_use]
    pub fn cells(&self) -> usize {
        crate::display::cells(&self.text)
            + if self.bar {
                crate::task_strip::BAR_CELLS + 3
            } else {
                0
            }
    }
}

/// `list`'s items, in their order, without the ones that now have nothing
/// to say (with no marks there is no "0 marked"; an empty item takes up no
/// room or separator).
#[must_use]
pub fn items(input: &StatusInput, list: StatusItems, lang: Lang) -> Vec<StatusItemView> {
    list.iter().filter_map(|i| item(input, i, lang)).collect()
}

fn item(input: &StatusInput, which: StatusItem, lang: Lang) -> Option<StatusItemView> {
    let mut form: Option<(bool, Option<ItemProgress>, Vec<crate::task_strip::Form>)> = None;
    let (text, command, priority) = match which {
        StatusItem::Position => {
            let (pos, total) = input.position?;
            (format!("{pos}/{total}"), None, 60)
        }
        StatusItem::Marks => {
            let s = crate::notes::marked(input.marked, input.marked_bytes, input.marked_dirs, lang);
            if s.is_empty() {
                return None;
            }
            (s, None, 70)
        }
        StatusItem::Sort => (sort_text(&input.sort, lang), Some("pane.sort-menu"), 30),
        StatusItem::Encoding => (
            input
                .encoding
                .map_or_else(|| "UTF-8".to_owned(), |e| e.label().to_uppercase()),
            Some("pane.names-encoding"),
            40,
        ),
        StatusItem::Tasks => {
            let v = input.strip.as_ref()?;
            let mut forms = crate::task_strip::forms(v, lang).into_iter();
            let first = forms.next()?;
            form = Some((
                first.bar,
                Some(ItemProgress {
                    percent: v.percent,
                    phase: v.phase,
                }),
                forms.collect(),
            ));
            (first.text, Some("layout.processes"), 80)
        }
        StatusItem::Notices => {
            if input.notices == 0 {
                return None;
            }
            (format!("!{}", input.notices), Some("layout.log"), 90)
        }
    };
    let id = which.as_str();
    let tooltip = ta_in(
        lang,
        &format!("status-item-{id}-tip"),
        &[("n", &tip_count(input, which))],
    );
    let (bar, progress, shorter) = form.unwrap_or_default();
    Some(StatusItemView {
        id: id.to_owned(),
        text,
        tooltip,
        command,
        priority,
        bar,
        progress,
        shorter,
    })
}

/// The most a plugin item's text takes up, in cells: the value is a third
/// party's, and a long one cannot eat the bar.
pub const PLUGIN_ITEM_MAX_CELLS: usize = 32;

/// The items the PLUGINS contribute (ADR 0137): the value of each
/// `(plugin, column)` pair for the entry under the cursor, in their order.
///
/// They go to the LEFT of the right half — where VS Code puts the branch —
/// and are the first to give way: the program's own stuff outranks a third
/// party's. Not clickable: a column plugin paints, it does not drive the
/// file manager.
///
/// The text comes from [`crate::PaneState::plugin_cell`], which serves it
/// re-masked, and is also clipped to [`PLUGIN_ITEM_MAX_CELLS`]. With no
/// value — plugin not consented, column not declared, entry with no data —
/// the item does not appear.
#[must_use]
pub fn plugin_items(
    pane: &crate::PaneState,
    pairs: &[(String, String)],
    lang: Lang,
) -> Vec<StatusItemView> {
    let Some(entry) = pane.selected() else {
        return Vec::new();
    };
    pairs
        .iter()
        .filter_map(|(plugin, column)| {
            let id = crate::columns::plugin_display_id(plugin, column);
            let value = pane.plugin_cell(&id, &entry.path)?;
            let text = clip(&value, PLUGIN_ITEM_MAX_CELLS);
            let (plugin_visible, _) = crate::display_name(plugin.as_bytes());
            let (column_visible, _) = crate::display_name(column.as_bytes());
            let tooltip = ta_in(
                lang,
                "status-item-plugin-tip",
                &[("plugin", &plugin_visible), ("column", &column_visible)],
            );
            Some(StatusItemView {
                id,
                text,
                tooltip,
                command: None,
                priority: 10,
                bar: false,
                progress: None,
                shorter: Vec::new(),
            })
        })
        .collect()
}

/// `s` in at most `max` cells, with `…` at the end if it did not fit. By
/// CELLS and not characters: an ideograph takes up two.
fn clip(s: &str, max: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if s.width() <= max {
        return s.to_owned();
    }
    let mut out = String::new();
    let mut width = 0;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        // One cell for the ellipsis.
        if width + w + 1 > max {
            break;
        }
        width += w;
        out.push(c);
    }
    // No zero width at the end: a cut ZWJ or combining mark would stick to
    // the ellipsis (the same trap as `ellipsis_at_bytes`).
    while out
        .chars()
        .next_back()
        .is_some_and(|c| c.width() == Some(0))
    {
        out.pop();
    }
    out.push('…');
    out
}

/// The figure an item's tooltip needs, if any.
fn tip_count(input: &StatusInput, which: StatusItem) -> String {
    match which {
        StatusItem::Tasks => input.strip.as_ref().map_or(0, |v| v.count).to_string(),
        StatusItem::Notices => input.notices.to_string(),
        _ => String::new(),
    }
}

/// `Name ↑`: the column and the direction.
fn sort_text(s: &SortSpec, lang: Lang) -> String {
    let column = match &s.column {
        SortColumn::Name => t_in(lang, "status-item-sort-name"),
        SortColumn::Size => t_in(lang, "status-item-sort-size"),
        SortColumn::Mtime => t_in(lang, "status-item-sort-mtime"),
        SortColumn::Extension => t_in(lang, "status-item-sort-ext"),
        // An attribute is named by its id (`posix.mode`): its readable
        // label lives in the provider's catalogue, which this bar does not
        // have. And the id is emitted by a THIRD PARTY, so it is masked
        // before painting it, like anything else a provider says about
        // itself.
        SortColumn::Attr(id) => crate::display_name(id.as_bytes()).0,
    };
    let arrow = match s.dir {
        SortDir::Asc => '↑',
        SortDir::Desc => '↓',
    };
    format!("{column} {arrow}")
}

/// Which items fit in `width` cells with `sep` cells between two in a row,
/// already in the form they are painted.
///
/// It SHORTENS first: an item with shorter forms (the tasks one, ADR 0146)
/// drops a form before any other is removed, because a short form keeps
/// the data and removing an item loses it. Then the LOWEST-priority ones
/// are dropped until they fit, and the ones that remain keep the
/// configured order.
///
/// Half a word is not an item: one that does not fit whole is not painted.
///
/// ```
/// use norte_frontend::statusbar::{StatusItemView, fit};
/// let v = |id: &str, text: &str, priority| StatusItemView {
///     id: id.into(), text: text.into(), tooltip: String::new(), command: None, priority,
///     bar: false, progress: None, shorter: Vec::new(),
/// };
/// let items = [v("a", "aaaa", 10), v("b", "bb", 90), v("c", "cc", 50)];
/// let ids = |w| fit(&items, w, 2).into_iter().map(|i| i.id).collect::<Vec<_>>();
/// assert_eq!(ids(100), ["a", "b", "c"]);
/// // 2 + 2 + 2 = 6 fit; with "aaaa" it would be 12.
/// assert_eq!(ids(6), ["b", "c"]);
/// assert!(ids(1).is_empty());
/// ```
#[must_use]
pub fn fit(items: &[StatusItemView], width: usize, sep: usize) -> Vec<StatusItemView> {
    let mut remaining: Vec<StatusItemView> = items.to_vec();
    let width_of = |q: &[StatusItemView]| {
        q.iter().map(StatusItemView::cells).sum::<usize>() + sep * q.len().saturating_sub(1)
    };
    loop {
        if width_of(&remaining) <= width {
            return remaining;
        }
        if let Some(v) = remaining.iter_mut().find(|v| !v.shorter.is_empty()) {
            let f = v.shorter.remove(0);
            v.text = f.text;
            v.bar = f.bar;
            continue;
        }
        // The lowest priority; at equal priority, the rightmost.
        let Some(pos) = remaining
            .iter()
            .enumerate()
            .min_by_key(|&(p, v)| (v.priority, std::cmp::Reverse(p)))
            .map(|(p, _)| p)
        else {
            return remaining;
        };
        remaining.remove(pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> StatusInput {
        StatusInput {
            position: Some((3, 120)),
            marked: 0,
            marked_bytes: 0,
            marked_dirs: 0,
            sort: SortSpec::default(),
            encoding: None,
            strip: None,
            notices: 0,
        }
    }

    /// The order is the list's; what has nothing to say does not appear.
    #[test]
    fn follows_the_configured_order_and_stays_silent_on_the_empty() {
        let list = StatusItems::parse(&["encoding", "tasks", "position", "marks"]).unwrap();
        let v = items(&input(), list, Lang::Es);
        let ids: Vec<_> = v.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["encoding", "position"], "no tasks or marks");
        assert_eq!(v[0].text, "UTF-8");
        assert_eq!(v[1].text, "3/120");
    }

    /// With a filter, the position is not shown: it is not the real one.
    #[test]
    fn no_position_with_a_filter() {
        let mut i = input();
        i.position = None;
        let v = items(&i, StatusItems::DEFAULT, Lang::Es);
        assert!(v.iter().all(|x| x.id != "position"));
    }

    /// Every clickable item runs a command that EXISTS in the catalogue: a
    /// click the TUI accepts and the window rejects is the same decision
    /// with two answers.
    #[test]
    fn the_commands_exist() {
        let mut i = input();
        i.strip = Some(crate::task_strip::StripView {
            phase: crate::task_strip::StripPhase::Running,
            count: 2,
            percent: Some(40),
            kind: None,
            name: None,
            rate: String::new(),
            eta: "1m".to_owned(),
        });
        i.notices = 1;
        i.marked = 1;
        for v in items(&i, StatusItems::DEFAULT, Lang::Es) {
            if let Some(c) = v.command {
                assert!(
                    crate::keymap::catalogue::lookup(c).is_some(),
                    "{c} is not in the catalogue"
                );
            }
            assert!(
                !v.tooltip.starts_with("status-item-"),
                "untranslated: {}",
                v.tooltip
            );
        }
    }

    #[test]
    fn the_sort_and_the_encoding_are_read() {
        let mut i = input();
        i.sort.column = SortColumn::Size;
        i.sort.dir = SortDir::Desc;
        i.encoding = Some(norte_encoding::NameEncoding::Cp437);
        let v = items(&i, StatusItems::DEFAULT, Lang::Es);
        let of = |id| v.iter().find(|x| x.id == id).map(|x| x.text.clone());
        assert_eq!(of("sort").as_deref(), Some("Tamaño ↓"));
        assert_eq!(of("encoding").as_deref(), Some("CP437"));
    }

    /// ADR 0146: the tasks item SHORTENS before anything else falls off,
    /// and only falls off once not even its shortest form fits.
    #[test]
    fn tasks_shorten_before_anything_is_dropped() {
        let mut i = input();
        i.strip = Some(crate::task_strip::StripView {
            phase: crate::task_strip::StripPhase::Running,
            count: 1,
            percent: Some(62),
            kind: Some(norte_proto::TaskKind::Copy),
            name: Some("big-photo.jpg".to_owned()),
            rate: "48 MiB/s".to_owned(),
            eta: String::new(),
        });
        let list = StatusItems::parse(&["tasks", "position"]).unwrap();
        let v = items(&i, list, Lang::Es);
        let whole = fit(&v, 200, 2);
        assert!(whole[0].text.contains("big-photo.jpg") && whole[0].bar);
        // Exactly what the shortest form with a bar takes, plus the
        // position.
        let short = v[0].shorter.iter().rfind(|f| f.bar).expect("short form");
        let w = crate::task_strip::form_cells(short) + 2 + 5;
        let fitted = fit(&v, w, 2);
        let ids: Vec<_> = fitted.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["tasks", "position"], "nothing falls off");
        assert_eq!(fitted[0].text, short.text);
        assert!(fitted.iter().map(StatusItemView::cells).sum::<usize>() + 2 <= w);
    }

    /// With no bar (before the threshold, or with no work) the item does
    /// not appear.
    #[test]
    fn no_bar_means_no_tasks_item() {
        let list = StatusItems::parse(&["tasks"]).unwrap();
        assert!(items(&input(), list, Lang::Es).is_empty());
    }
}
