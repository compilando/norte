//! The layouts picker: which rows there are and what each one shows.
//!
//! Lives here and not in a frontend because of rule 7 —logic does not go in
//! the TUI— and because the GUI will need the same picker with a different
//! painter.

use std::ffi::{OsStr, OsString};

use crate::layout::{KindRegistry, Node, Rect, SlotId, presets, resolve};

/// A user layout, already read from disk.
///
/// It arrives already read and not by name because the picker PAINTS every
/// row's preview: reading it as the cursor passes over would be I/O in the
/// event loop, and not reading it left user rows with their right half blank
/// while the documentation promised otherwise (#244 M3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserLayout {
    /// The file's name without its extension, with its raw bytes (#246).
    pub name: OsString,
    /// Its tree, or why it could not be read.
    pub tree: Result<Node, String>,
}

/// One row of the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The name it loads under.
    ///
    /// [`OsString`] and not `String`: it is a FILE name and ends up as
    /// `layouts/<name>.toml`, so passing it through text could change which
    /// one gets opened (#246).
    pub name: OsString,
    /// Built-in, or from `layouts/<name>.toml` in the config directory.
    pub factory: bool,
    /// Whether the name matches a KEYMAP preset.
    ///
    /// The picker WARNS about it, because choosing this layout does not
    /// change a single key: they are two different settings that share a
    /// name, and without the line the coincidence is a trap instead of a
    /// convenience.
    pub shares_keymap_name: bool,
    /// The tree that would be applied, for the preview. `None` when the file
    /// does not parse: then [`Self::problem`] carries the reason.
    pub tree: Option<Node>,
    /// Why this row has no preview, when it does not.
    pub problem: Option<String>,
}

/// The layouts picker.
#[derive(Debug)]
pub struct LayoutPicker {
    rows: Vec<Row>,
    cursor: usize,
}

impl LayoutPicker {
    /// Opens the picker with the five built-in ones and the user layouts it
    /// is given, already read.
    ///
    /// The files are read by whoever has the disk in front of them: this
    /// crate does not touch directories (rule 2 — this gets called from an
    /// async loop).
    ///
    /// The match against a built-in one is BYTE FOR BYTE, same as the
    /// loader's: on a system that does not distinguish case, `Orthodox.toml`
    /// is a different file from the `orthodox` layout and the two rows are
    /// two different choices — what must never happen is that the one that
    /// says "built-in" loads the other one, and that is
    /// [`crate::layout::config::load`]'s job (#245).
    #[must_use]
    pub fn open(user: Vec<UserLayout>) -> Self {
        let factory_row = |name: &str| Row {
            name: OsString::from(name),
            factory: true,
            shares_keymap_name: crate::keymap::presets::NAMES.contains(&name),
            tree: presets::tree(name).ok(),
            problem: None,
        };
        let mut rows: Vec<Row> = presets::NAMES.iter().map(|n| factory_row(n)).collect();
        // A user file named EXACTLY like a built-in one is not duplicated:
        // the user's one wins, which is what every other configuration layer
        // does.
        for u in user {
            let shares = u
                .name
                .to_str()
                .is_some_and(|n| crate::keymap::presets::NAMES.contains(&n));
            let (tree, problem) = match u.tree {
                Ok(t) => (Some(t), None),
                Err(e) => (None, Some(e)),
            };
            if let Some(r) = rows.iter_mut().find(|r| r.name == u.name) {
                r.factory = false;
                r.tree = tree;
                r.problem = problem;
            } else {
                rows.push(Row {
                    name: u.name,
                    factory: false,
                    shares_keymap_name: shares,
                    tree,
                    problem,
                });
            }
        }
        Self { rows, cursor: 0 }
    }

    /// The rows, in order.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Where the cursor is, clamped to the rows there are.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor.min(self.rows.len().saturating_sub(1))
    }

    /// Moves up.
    pub const fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves down.
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// The name of the highlighted row.
    #[must_use]
    pub fn chosen(&self) -> Option<&OsStr> {
        self.rows.get(self.cursor()).map(|r| r.name.as_os_str())
    }

    /// The whole highlighted row, to paint its preview.
    #[must_use]
    pub fn current(&self) -> Option<&Row> {
        self.rows.get(self.cursor())
    }
}

/// The NOMINAL area a preview is laid out over: a normal terminal. The
/// thumbnail is SCALED from here instead of laying out directly into its own
/// box, because the layout honours each kind's minimums and in twenty columns
/// half the panels would collapse — the thumbnail would then show a screen
/// nobody is ever going to see.
const NOMINAL: (u16, u16) = (80, 24);

/// A tree's preview: boxes drawn from its LAYOUT, one string per row of cells
/// of a `w`×`h` area.
///
/// From the layout and not from a drawing saved next to the file: a saved
/// drawing starts lying the moment anyone touches the sizes, and whoever
/// looks at it has no way to know which of the two is the real screen.
///
/// ```
/// use norte_frontend::layout::{KindRegistry, presets};
/// use norte_frontend::layout_picker::preview;
///
/// let reg = KindRegistry::builtin();
/// let rows = preview(&presets::tree("simple").expect("built-in"), 20, 8, &reg);
/// assert_eq!(rows.len(), 8);
/// assert!(rows.iter().all(|f| f.chars().count() == 20));
/// ```
#[must_use]
pub fn preview(tree: &Node, w: u16, h: u16, decls: &KindRegistry) -> Vec<String> {
    // The task strip is `Auto` and measures zero at rest, so it would
    // disappear in a preview. It is given its minimum: what is shown is the
    // SHAPE of the screen, not the current workload.
    let natural = |id: SlotId| tree.kind_of(id).map_or((0, 1), |k| (0, decls.min_of(k).1));
    let substituted = tree.substitute_auto(&natural);
    let res = resolve(Rect::new(0, 0, NOMINAL.0, NOMINAL.1), &substituted, decls);
    let (wu, hu) = (w as usize, h as usize);
    let mut canvas = vec![vec![' '; wu]; hu];
    // Scales from nominal cells to thumbnail cells, rounding to the nearest
    // edge and guaranteeing that no box disappears: a one-cell box still says
    // that panel is there.
    let scale =
        |v: u16, from: u16, to: u16| (u32::from(v) * u32::from(to) / u32::from(from)) as usize;
    for (id, r) in &res.placements {
        let initial = substituted
            .kind_of(*id)
            .and_then(|k| k.as_str().chars().next())
            .unwrap_or('?');
        let x0 = scale(r.x, NOMINAL.0, w).min(wu.saturating_sub(1));
        let y0 = scale(r.y, NOMINAL.1, h).min(hu.saturating_sub(1));
        let x1 = scale(r.x.saturating_add(r.width), NOMINAL.0, w)
            .max(x0 + 1)
            .min(wu);
        let y1 = scale(r.y.saturating_add(r.height), NOMINAL.1, h)
            .max(y0 + 1)
            .min(hu);
        for (y, row) in canvas.iter_mut().enumerate().take(y1).skip(y0) {
            for (x, cell) in row.iter_mut().enumerate().take(x1).skip(x0) {
                // The frame is only drawn if there is room left for something
                // inside. In a one- or two-cell box, the frame WOULD BE the
                // whole box and the thumbnail would lose the letter that says
                // which panel it is.
                let frame_h = x1 - x0 >= 3 && (x == x0 || x + 1 == x1);
                let frame_v = y1 - y0 >= 3 && (y == y0 || y + 1 == y1);
                *cell = if frame_h || frame_v { '·' } else { initial };
            }
        }
    }
    canvas
        .into_iter()
        .map(|f| f.into_iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mine(name: &str, tree: Result<Node, String>) -> UserLayout {
        UserLayout {
            name: OsString::from(name),
            tree,
        }
    }

    #[expect(clippy::unnecessary_wraps, reason = "the `tree` field is a Result")]
    fn tree_of(n: &str) -> Result<Node, String> {
        Ok(presets::tree(n).expect(n))
    }

    #[test]
    fn the_five_built_ins_come_out_in_order() {
        let p = LayoutPicker::open(Vec::new());
        let names: Vec<&str> = p
            .rows()
            .iter()
            .map(|r| r.name.to_str().expect("a built-in is ASCII"))
            .collect();
        assert_eq!(names, presets::NAMES);
        assert!(p.rows().iter().all(|r| r.factory));
        assert!(
            p.rows().iter().all(|r| r.tree.is_some()),
            "every built-in row carries its tree for the preview"
        );
    }

    /// The user row CARRIES its tree: its right half used to paint blank, and
    /// the documentation said every row draws its screen (#244 M3).
    #[test]
    fn a_user_row_carries_its_preview() {
        let p = LayoutPicker::open(vec![mine("mine", tree_of("krusader"))]);
        let row = p.rows().last().expect("mine");
        assert_eq!(row.name, OsString::from("mine"));
        assert!(row.tree.is_some());
        assert!(row.problem.is_none());
    }

    /// And one that does not parse SAYS SO, instead of showing a blank gap
    /// indistinguishable from an empty layout.
    #[test]
    fn a_row_that_does_not_parse_carries_its_diagnosis() {
        let p = LayoutPicker::open(vec![mine("broken", Err("not TOML".to_owned()))]);
        let row = p.rows().last().expect("broken");
        assert!(row.tree.is_none());
        assert_eq!(row.problem.as_deref(), Some("not TOML"));
    }

    /// A name that is not UTF-8 has its row: the lister used to drop it
    /// silently (#246 m2).
    #[cfg(unix)]
    #[test]
    fn a_non_utf8_name_has_its_row() {
        use std::os::unix::ffi::OsStrExt as _;

        let raw = OsStr::from_bytes(b"m\xffl").to_os_string();
        let p = LayoutPicker::open(vec![UserLayout {
            name: raw.clone(),
            tree: tree_of("simple"),
        }]);
        assert_eq!(p.rows().last().expect("its own").name, raw);
    }

    /// `krusader` is ALSO a keymap preset, and choosing the layout does not
    /// change a single key. The row says so; if this breaks, the warning
    /// disappears and the name coincidence turns into a trap.
    #[test]
    fn the_row_warns_when_the_name_is_also_a_keymap_one() {
        let p = LayoutPicker::open(Vec::new());
        let f = |n: &str| {
            p.rows()
                .iter()
                .find(|r| r.name == OsStr::new(n))
                .expect(n)
                .shares_keymap_name
        };
        assert!(f("krusader"));
        assert!(f("orthodox"));
        assert!(!f("explorer"));
    }

    /// A user file with a built-in name does not appear twice.
    #[test]
    fn a_user_layout_with_a_built_in_name_is_not_duplicated() {
        let p = LayoutPicker::open(vec![
            mine("simple", tree_of("krusader")),
            mine("mine", tree_of("simple")),
        ]);
        assert_eq!(p.rows().len(), presets::NAMES.len() + 1);
        let simple = p
            .rows()
            .iter()
            .find(|r| r.name == OsStr::new("simple"))
            .expect("simple");
        assert!(!simple.factory);
        assert_eq!(
            simple.tree.as_ref(),
            presets::tree("krusader").ok().as_ref(),
            "the user row shows ITS tree, not the built-in one it covers"
        );
        assert_eq!(p.rows().last().expect("mine").name, OsString::from("mine"));
    }

    /// A file that differs only in case does NOT cover the built-in one:
    /// they are two different files and two different choices, and the
    /// loader resolves byte for byte so the "built-in" row never ends up
    /// opening the user's one (#245).
    #[test]
    fn a_name_with_different_case_is_a_different_row() {
        let p = LayoutPicker::open(vec![mine("Orthodox", tree_of("krusader"))]);
        assert_eq!(p.rows().len(), presets::NAMES.len() + 1);
        assert!(
            p.rows()
                .iter()
                .find(|r| r.name == OsStr::new("orthodox"))
                .expect("orthodox")
                .factory,
            "the built-in one is still built-in"
        );
    }

    #[test]
    fn the_cursor_does_not_run_off() {
        let mut p = LayoutPicker::open(Vec::new());
        for _ in 0..20 {
            p.down();
        }
        assert_eq!(p.chosen(), Some(OsStr::new("full")));
        for _ in 0..20 {
            p.up();
        }
        assert_eq!(p.chosen(), Some(OsStr::new("orthodox")));
    }

    /// The preview comes from the layout: `simple` has ONE listing and
    /// `orthodox` two, so they cannot be painted the same.
    #[test]
    fn the_preview_tells_one_layout_apart_from_another() {
        let reg = KindRegistry::builtin();
        let one = preview(&presets::tree("simple").expect("s"), 20, 8, &reg);
        let two = preview(&presets::tree("orthodox").expect("o"), 20, 8, &reg);
        assert_ne!(one, two);
    }

    /// No preview runs outside its box, not even with an absurd area.
    #[test]
    fn the_preview_stays_inside_its_area() {
        let reg = KindRegistry::builtin();
        for name in presets::NAMES {
            let tree = presets::tree(name).expect(name);
            for (w, h) in [(20_u16, 8_u16), (1, 1), (3, 2), (80, 24)] {
                let rows = preview(&tree, w, h, &reg);
                assert_eq!(rows.len(), h as usize, "{name} {w}x{h}");
                assert!(
                    rows.iter().all(|f| f.chars().count() == w as usize),
                    "{name} {w}x{h}"
                );
            }
        }
    }

    /// The task strip shows up in the preview even though it measures zero
    /// at rest: what is shown is the shape of the screen, not the current
    /// workload.
    #[test]
    fn the_task_strip_shows_up_in_the_preview() {
        let reg = KindRegistry::builtin();
        let rows = preview(&presets::tree("orthodox").expect("o"), 20, 10, &reg);
        assert!(
            rows.iter().any(|f| f.contains('t')),
            "the `tasks` strip does not appear: {rows:?}"
        );
    }
}
