//! The disk map, laid out into rectangles (phase 4, T3).
//!
//! A list of already-measured children (`fs.dir_usage`) goes in and a
//! [`StyledFrame`] comes out: styled lines and clickable zones, which is what
//! both frontends already know how to paint since phase 3. The layout lives
//! here and not in each one because a treemap computed twice is two
//! different treemaps the moment someone touches a rounding — the lesson of
//! ADR 0077.
//!
//! # The frame is OURS
//! In a plugin panel, the label AND the command are chosen by a third party,
//! and that is why `zone_allowed` exists (ADR 0116). Here, this function
//! chooses them: every rectangle names `nav.enter` over a child of the
//! directory being shown, so its zones do not go through that filter and a
//! plugin cannot fabricate a `disk-map` frame.
//!
//! # The `arg` is the name in WIRE form, never what gets painted
//! What is painted goes through [`crate::display_name`], which masks: a name
//! with control bytes shows up as `�` and THAT form identifies no file. The
//! `arg` carries [`Segment::to_wire`](norte_proto::Segment::to_wire), which
//! is reversible, and whoever receives it resolves
//! `parent.join(Segment::parse_wire(arg))`. A non-UTF8 name, an NFD one, or
//! one called `!` arrive whole or not at all.

use norte_proto::EntryKind;
use norte_proto::methods::DirUsageChild;
use norte_theme::Role;

use crate::ansi::StyledSpan;
use crate::frame::{Hit, MAX_HITS, StyledFrame};

/// The command a rectangle runs: enter that child.
const COMMAND: &str = "nav.enter";

/// Mark for a child whose size is a LOWER BOUND (`partial`).
///
/// It goes in the label and not in the color: the color says what CLASS the
/// file is, and an incomplete rectangle can be of any class. The painter
/// does not have to choose between the two things.
const PARTIAL_MARK: char = '≈';

/// What class a child is, so the map paints it as what it is.
///
/// There was no taxonomy to reuse: a plugin's decorator receives a theme
/// ROLE, not a class (ADR 0105), so this is the first one and it lives here,
/// where both frontends use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildClass {
    /// A directory.
    Directory,
    /// Source, headers, scripts.
    Code,
    /// A compressed container.
    Archive,
    /// A still image.
    Image,
    /// Audio or video — what usually takes up the big rectangle.
    Media,
    /// Readable text: documents, notes, data.
    Document,
    /// Everything else, including what we do not know how to read.
    Other,
}

impl ChildClass {
    /// The theme role it is painted with.
    ///
    /// Roles and not raw colors (ADR 0037): the theme rules, and a map
    /// hard-coded to `#ff8800` looks equally bad in both themes the reader
    /// chose. None of these roles means "file of this class" —that family
    /// does not exist— so they are borrowed for CONTRAST, which is what a
    /// treemap needs: neighbouring rectangles that stand apart.
    #[must_use]
    pub fn role(self) -> Role {
        match self {
            Self::Directory => Role::Info,
            Self::Code => Role::Match,
            Self::Archive => Role::Warning,
            Self::Image => Role::Badge,
            Self::Media => Role::Selection,
            Self::Document => Role::Regular,
            Self::Other => Role::Muted,
        }
    }
}

/// A child's class, by its type and its extension.
///
/// The extension is read from the name's BYTES and compared in lowercase
/// ASCII: the name is not decoded to classify it, because a name that is not
/// UTF-8 has an extension all the same (rule 1).
#[must_use]
pub fn class_of(child: &DirUsageChild) -> ChildClass {
    if child.kind == EntryKind::Dir {
        return ChildClass::Directory;
    }
    let bytes = child.name.as_bytes();
    let Some(dot) = bytes.iter().rposition(|b| *b == b'.') else {
        return ChildClass::Other;
    };
    let ext: Vec<u8> = bytes[dot + 1..].to_ascii_lowercase();
    match ext.as_slice() {
        b"rs" | b"c" | b"h" | b"cpp" | b"hpp" | b"py" | b"js" | b"ts" | b"go" | b"java" | b"rb"
        | b"sh" | b"toml" | b"json" | b"yaml" | b"yml" => ChildClass::Code,
        b"zip" | b"gz" | b"bz2" | b"xz" | b"zst" | b"tar" | b"rar" | b"7z" => ChildClass::Archive,
        b"png" | b"jpg" | b"jpeg" | b"gif" | b"webp" | b"bmp" | b"svg" | b"ico" => {
            ChildClass::Image
        }
        b"mp3" | b"flac" | b"ogg" | b"wav" | b"mp4" | b"mkv" | b"avi" | b"mov" | b"webm" => {
            ChildClass::Media
        }
        b"txt" | b"md" | b"pdf" | b"doc" | b"docx" | b"odt" | b"csv" | b"html" => {
            ChildClass::Document
        }
        _ => ChildClass::Other,
    }
}

/// One rectangle of the layout, in frame CELLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Column of the top-left corner.
    pub x: u16,
    /// Row of the top-left corner.
    pub y: u16,
    /// Width in cells. Zero = not painted.
    pub w: u16,
    /// Height in cells. Zero = not painted.
    pub h: u16,
}

impl Rect {
    /// How many cells it occupies.
    #[must_use]
    pub fn cells(self) -> u32 {
        u32::from(self.w) * u32::from(self.h)
    }
}

/// The layout: one rectangle per child, in the same order as `weights`.
///
/// A child that does not reach a whole cell comes out with `w` or `h` at
/// zero — **it is not painted, but it does not disappear**: its size is
/// already counted in whatever total is shown next to it, and dropping it
/// from the list would make the rectangles lie about what the directory is
/// made of.
///
/// # EXACT layout, by construction
/// Widths are distributed with a remainder accumulator instead of rounding
/// each one on its own: the last one of each strip takes whatever is left.
/// That way the rectangles neither overlap nor leave gaps, and it does not
/// need to be checked afterwards — what the tests check is that this
/// property holds.
///
/// ```
/// use norte_frontend::treemap::{Rect, distribute};
/// let r = distribute(&[3, 1], Rect { x: 0, y: 0, w: 4, h: 1 });
/// assert_eq!(r.len(), 2);
/// // They cover the whole strip, without overlapping.
/// assert_eq!(r[0].w + r[1].w, 4);
/// ```
#[must_use]
pub fn distribute(weights: &[u64], area: Rect) -> Vec<Rect> {
    let mut out = vec![
        Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0
        };
        weights.len()
    ];
    if weights.is_empty() || area.w == 0 || area.h == 0 {
        return out;
    }
    // Descending size order: that is what makes a treemap come out legible,
    // and the tie-break by POSITION keeps the result stable between two
    // views of the same thing.
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by(|a, b| weights[*b].cmp(&weights[*a]).then(a.cmp(b)));

    let mut remaining: u64 = weights.iter().copied().fold(0, u64::saturating_add);
    let mut free = area;
    let mut i = 0;
    while i < order.len() && free.w > 0 && free.h > 0 && remaining > 0 {
        // The strip is laid along the SHORT side, which is what keeps the
        // rectangles square instead of turning them into thin strips.
        let horizontal = free.w <= free.h;
        let length = if horizontal { free.w } else { free.h };
        let total_thickness = if horizontal { free.h } else { free.w };

        // How many children fit in this strip: it grows while the worst
        // aspect ratio improves (the classic squarified algorithm).
        let mut end = i;
        let mut sum: u64 = 0;
        let mut best = f64::INFINITY;
        while end < order.len() {
            let new_sum = sum.saturating_add(weights[order[end]]);
            if new_sum == 0 {
                end += 1;
                continue;
            }
            let worst = worst_aspect(
                &order[i..=end],
                weights,
                new_sum,
                remaining,
                length,
                total_thickness,
            );
            if worst > best {
                break;
            }
            best = worst;
            sum = new_sum;
            end += 1;
        }
        if end == i {
            // Nothing measurable is left: the rest are zeros and go
            // unpainted.
            break;
        }

        // The strip's thickness, at least one cell if it carries anything.
        let thickness =
            cells_of(fraction(sum, remaining), total_thickness).clamp(1, total_thickness);

        // And inside it, the length is distributed with a remainder
        // accumulator.
        let mut used: u16 = 0;
        for (n, idx) in order[i..end].iter().enumerate() {
            let last = n == end - i - 1;
            let chunk = if last {
                length - used
            } else {
                cells_of(fraction(weights[*idx], sum), length).min(length - used)
            };
            out[*idx] = if horizontal {
                Rect {
                    x: free.x + used,
                    y: free.y,
                    w: chunk,
                    h: thickness,
                }
            } else {
                Rect {
                    x: free.x,
                    y: free.y + used,
                    w: thickness,
                    h: chunk,
                }
            };
            used += chunk;
        }

        // What is left free, for the next strip.
        if horizontal {
            free.y += thickness;
            free.h -= thickness;
        } else {
            free.x += thickness;
            free.w -= thickness;
        }
        remaining = remaining.saturating_sub(sum);
        i = end;
    }
    out
}

/// What fraction of the total `part` is, to distribute area.
///
/// A treemap distributes PROPORTIONS, and that calls for floating point.
/// `u64` to `f64`'s precision loss starts above 2^53 bytes: a nine-petabyte
/// directory would lose one byte of precision computing how many cells it
/// gets. It is a DRAWING — the sizes that are shown come from the report,
/// which stays an integer.
#[expect(
    clippy::cast_precision_loss,
    reason = "area distribution: the error starts at 2^53 bytes and only affects how many cells get painted, not the size that is stated"
)]
fn fraction(part: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    part as f64 / total as f64
}

/// How many cells of `total` a proportion gets.
///
/// The result is bounded by construction: `prop` comes from [`fraction`] and
/// lives in `[0, 1]`, so the product falls in `[0, total]` and the `clamp`
/// keeps it there even if a rounding overshoots by one. It neither truncates
/// what matters nor can come out negative.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "product of a proportion in [0,1] by a number of cells, and bounded besides: the result fits in u16 and is not negative"
)]
fn cells_of(prop: f64, total: u16) -> u16 {
    let n = (prop * f64::from(total)).round();
    n.clamp(0.0, f64::from(total)) as u16
}

/// The worst aspect ratio (long side / short side) of a candidate strip.
fn worst_aspect(
    strip: &[usize],
    weights: &[u64],
    sum: u64,
    remaining: u64,
    length: u16,
    total_thickness: u16,
) -> f64 {
    let thickness = (fraction(sum, remaining) * f64::from(total_thickness)).max(1.0);
    let mut worst: f64 = 1.0;
    for idx in strip {
        let p = weights[*idx];
        if p == 0 {
            continue;
        }
        let chunk = fraction(p, sum) * f64::from(length);
        if chunk <= 0.0 {
            continue;
        }
        let a = (thickness / chunk).max(chunk / thickness);
        worst = worst.max(a);
    }
    worst
}

/// The whole map: painted, clickable rectangles.
///
/// `cols`/`rows` are the slot's cells. A zero-sized slot paints nothing,
/// which is different from painting an empty frame.
///
/// # Zones are BUDGETED
/// A multi-row rectangle is described with one [`Hit`] per row (the frame's
/// contract), so a tall map eats through the zone cap fast — and
/// [`StyledFrame::clamped`] drops the excess SILENTLY. They are handed out
/// from largest to smallest: if not all of them fit, the ones left without a
/// zone are the small rectangles', never the big one somebody is trying to
/// click.
#[must_use]
pub fn squarify(children: &[DirUsageChild], cols: u16, rows: u16) -> StyledFrame {
    if cols == 0 || rows == 0 || children.is_empty() {
        return StyledFrame::default();
    }
    let weights: Vec<u64> = children.iter().map(|c| c.bytes).collect();
    let rects = distribute(
        &weights,
        Rect {
            x: 0,
            y: 0,
            w: cols,
            h: rows,
        },
    );

    // Owner grid: who occupies each cell. This is what turns rectangles into
    // lines without two of them stepping on each other.
    let width = usize::from(cols);
    let height = usize::from(rows);
    let mut owner: Vec<Option<usize>> = vec![None; width * height];
    for (i, r) in rects.iter().enumerate() {
        for y in r.y..r.y.saturating_add(r.h) {
            for x in r.x..r.x.saturating_add(r.w) {
                let (xi, yi) = (usize::from(x), usize::from(y));
                if xi < width && yi < height {
                    owner[yi * width + xi] = Some(i);
                }
            }
        }
    }

    let labels: Vec<String> = children.iter().map(label_of).collect();
    let lines = paint(&owner, children, &labels, &rects, width, height);
    let hits = hit_zones(children, &rects);
    StyledFrame::clamped(lines, hits)
}

/// A child's label: its masked name and what it takes up.
fn label_of(child: &DirUsageChild) -> String {
    let (name, _masked) = crate::display_name(child.name.as_bytes());
    let size = crate::human_bytes_short(child.bytes);
    if child.partial {
        format!("{name} {PARTIAL_MARK}{size}")
    } else {
        format!("{name} {size}")
    }
}

/// The frame's lines, one per row of cells.
fn paint(
    owner: &[Option<usize>],
    children: &[DirUsageChild],
    labels: &[String],
    rects: &[Rect],
    width: usize,
    height: usize,
) -> Vec<Vec<StyledSpan>> {
    let mut lines = Vec::with_capacity(height);
    for y in 0..height {
        let mut row: Vec<StyledSpan> = Vec::new();
        let mut x = 0;
        while x < width {
            let current = owner[y * width + x];
            let mut end = x;
            while end < width && owner[y * width + end] == current {
                end += 1;
            }
            let cell_count = end - x;
            let text = match current {
                // The label is painted on the rectangle's FIRST row and only
                // if it fits whole: half a label names a file that is not
                // there.
                Some(i)
                    if usize::from(rects[i].y) == y && labels[i].chars().count() <= cell_count =>
                {
                    let mut t = labels[i].clone();
                    t.push_str(&" ".repeat(cell_count - labels[i].chars().count()));
                    t
                }
                // No label fits, or no owner: blank cells. It is the SAME
                // result on purpose — the color already says whose rectangle
                // it is, and a different fill would be noise.
                _ => " ".repeat(cell_count),
            };
            row.push(StyledSpan {
                text,
                role: current.map(|i| class_of(&children[i]).role()),
                fg: None,
                bg: None,
            });
            x = end;
        }
        lines.push(row);
    }
    lines
}

/// The clickable zones, from largest to smallest and up to the cap.
fn hit_zones(children: &[DirUsageChild], rects: &[Rect]) -> Vec<Hit> {
    let mut order: Vec<usize> = (0..children.len()).collect();
    order.sort_by(|a, b| {
        children[*b].bytes.cmp(&children[*a].bytes).then_with(|| {
            children[*a]
                .name
                .as_bytes()
                .cmp(children[*b].name.as_bytes())
        })
    });
    let mut hits = Vec::new();
    for i in order {
        let r = rects[i];
        if r.w == 0 || r.h == 0 {
            continue;
        }
        for y in r.y..r.y.saturating_add(r.h) {
            if hits.len() >= MAX_HITS {
                return hits;
            }
            hits.push(Hit {
                row: y,
                col: r.x,
                width: r.w,
                command: COMMAND.to_owned(),
                // WIRE form: it is the one that can be turned back into the
                // exact name. What is PAINTED is masked and is no use here.
                arg: Some(children[i].name.to_wire()),
            });
        }
    }
    hits
}

/// How many maps are remembered at once.
///
/// Each one reaches up to [`DIR_USAGE_MAX_CHILDREN`] children, so this is not
/// a convenience count: with no cap, walking a large tree keeps saving every
/// directory visited for the whole session.
///
/// [`DIR_USAGE_MAX_CHILDREN`]: norte_proto::methods::DIR_USAGE_MAX_CHILDREN
pub const CACHE_MAX: usize = 8;

/// What has already been measured, so returning to a directory paints
/// instantly.
///
/// Measuring a tree costs seconds or minutes; going back to the parent and
/// down again is the most ordinary thing in the world. This keeps the LAST
/// report of each directory, bounded by [`CACHE_MAX`].
///
/// # The ping does not say WHAT changed, so everything is forgotten
/// Directory watching delivers a `()` per burst —"something changed in some
/// watched directory"— and nothing more. With that, ONE specific entry
/// cannot be invalidated: picking one would be making up which, and leaving
/// the rest would paint old sizes as if they were current. That is why
/// [`Self::invalidate`] drops everything, and [`Self::forget`] exists
/// separately for whoever DOES know which directory was touched.
///
/// # Lives in the consumer, not in a singleton
/// Same as `Tree`: the slot that shows the map has its own. A map belongs to
/// whoever is looking at it, and two slots showing different directories
/// share nothing. The RULE —when it is forgotten— is what both frontends
/// share; the wiring is set up by each one from its own refresh path,
/// because today only the terminal has native watching.
#[derive(Debug, Default)]
pub struct Cache {
    /// From most recent to oldest. A `Vec` and not a map: there are eight.
    entries: Vec<(
        norte_proto::VPath,
        norte_proto::methods::FsDirUsageReportResult,
    )>,
}

impl Cache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The last map measured for `dir`, if it is remembered.
    #[must_use]
    pub fn get(
        &self,
        dir: &norte_proto::VPath,
    ) -> Option<&norte_proto::methods::FsDirUsageReportResult> {
        self.entries.iter().find(|(p, _)| p == dir).map(|(_, r)| r)
    }

    /// Saves —or replaces— `dir`'s map, evicting the oldest one if needed.
    ///
    /// **An incomplete report is not saved.** A map cancelled mid-measurement
    /// is a correct fragment to show NOW, with its warning in front; saving
    /// it would turn it into the answer painted tomorrow with no warning at
    /// all.
    pub fn put(
        &mut self,
        dir: norte_proto::VPath,
        report: norte_proto::methods::FsDirUsageReportResult,
    ) {
        if !report.listed {
            return;
        }
        self.entries.retain(|(p, _)| *p != dir);
        self.entries.insert(0, (dir, report));
        self.entries.truncate(CACHE_MAX);
    }

    /// Forgets EVERYTHING: this is the answer to a warning that does not say
    /// what changed.
    pub fn invalidate(&mut self) {
        self.entries.clear();
    }

    /// Forgets one specific directory, for whoever DOES know which one was
    /// touched (a copy, a delete, a rename done from here).
    pub fn forget(&mut self, dir: &norte_proto::VPath) {
        self.entries.retain(|(p, _)| p != dir);
    }

    /// How many maps are remembered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Is none remembered?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::Segment;
    use norte_proto::VPath;
    use norte_proto::methods::FsDirUsageReportResult;

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("valid wire")
    }

    /// A FINISHED report of `bytes` bytes.
    fn report(bytes: u64) -> FsDirUsageReportResult {
        FsDirUsageReportResult {
            total_bytes: bytes,
            listed: true,
            ..FsDirUsageReportResult::default()
        }
    }

    /// What was measured is remembered: returning to a directory paints
    /// instantly instead of walking a tree that took minutes all over again.
    #[test]
    fn what_was_measured_is_remembered_per_directory() {
        let mut cache = Cache::new();
        cache.put(vp("mem:///a"), report(10));
        cache.put(vp("mem:///b"), report(20));
        assert_eq!(cache.get(&vp("mem:///a")).map(|r| r.total_bytes), Some(10));
        assert_eq!(cache.get(&vp("mem:///b")).map(|r| r.total_bytes), Some(20));
        assert!(cache.get(&vp("mem:///c")).is_none());
    }

    /// A map that never got listed is NOT saved.
    ///
    /// As a fragment shown now, with its warning in front, it is correct.
    /// Saved, it turns into the answer painted tomorrow with no warning at
    /// all: a directory that looks like it has one child because the
    /// measurement was cut off at the first one.
    #[test]
    fn a_half_done_map_is_not_saved() {
        let mut cache = Cache::new();
        let fragment = FsDirUsageReportResult {
            total_bytes: 5,
            listed: false,
            ..FsDirUsageReportResult::default()
        };
        cache.put(vp("mem:///a"), fragment);
        assert!(cache.is_empty(), "a fragment is not a savable answer");
    }

    /// The watch's warning does not say WHAT changed, so everything is
    /// forgotten.
    ///
    /// Invalidating only one entry would be making up which; leaving the
    /// rest would paint old sizes as if they were current.
    #[test]
    fn an_unnamed_warning_forgets_everything() {
        let mut cache = Cache::new();
        cache.put(vp("mem:///a"), report(10));
        cache.put(vp("mem:///b"), report(20));
        cache.invalidate();
        assert!(cache.is_empty());
    }

    /// Whoever DOES know what changed forgets only that.
    #[test]
    fn whoever_knows_what_changed_forgets_only_that() {
        let mut cache = Cache::new();
        cache.put(vp("mem:///a"), report(10));
        cache.put(vp("mem:///b"), report(20));
        cache.forget(&vp("mem:///a"));
        assert!(cache.get(&vp("mem:///a")).is_none());
        assert_eq!(cache.get(&vp("mem:///b")).map(|r| r.total_bytes), Some(20));
    }

    /// The cap evicts the oldest: walking a large tree cannot keep saving
    /// every directory of the session, with up to 4096 children each.
    #[test]
    fn the_cap_evicts_the_oldest() {
        let mut cache = Cache::new();
        for i in 0..CACHE_MAX + 3 {
            cache.put(vp(&format!("mem:///d{i}")), report(i as u64));
        }
        assert_eq!(cache.len(), CACHE_MAX);
        assert!(
            cache.get(&vp("mem:///d0")).is_none(),
            "the first one in is no longer there"
        );
        assert!(
            cache
                .get(&vp(&format!("mem:///d{}", CACHE_MAX + 2)))
                .is_some(),
            "the last one is"
        );
    }

    /// Measuring the same directory again REPLACES, it does not duplicate.
    #[test]
    fn measuring_again_replaces() {
        let mut cache = Cache::new();
        cache.put(vp("mem:///a"), report(10));
        cache.put(vp("mem:///a"), report(99));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(&vp("mem:///a")).map(|r| r.total_bytes), Some(99));
    }

    fn child(name: &str, bytes: u64, kind: EntryKind) -> DirUsageChild {
        DirUsageChild {
            name: Segment::new(name.as_bytes().to_vec()).expect("segment"),
            kind,
            bytes,
            entries: 1,
            partial: false,
        }
    }

    /// The layout COVERS the area and does not overlap: every cell has an
    /// owner and only one. This is the property everything else hangs from
    /// — a map with gaps lies about the free space, and one with overlaps
    /// makes a click open the wrong file.
    #[test]
    fn the_rectangles_cover_the_area_and_do_not_overlap() {
        let weights = [40_u64, 30, 20, 10];
        let area = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 10,
        };
        let rects = distribute(&weights, area);
        let mut cells = [0_u8; 20 * 10];
        for r in &rects {
            for y in r.y..r.y + r.h {
                for x in r.x..r.x + r.w {
                    cells[usize::from(y) * 20 + usize::from(x)] += 1;
                }
            }
        }
        assert!(
            cells.iter().all(|c| *c == 1),
            "each cell, exactly one owner"
        );
    }

    /// A tiny child that does not reach a whole cell is NOT painted, and it
    /// does not disappear from the list either: it still has its place in
    /// the layout, with zero area.
    #[test]
    fn a_tiny_child_is_not_painted_but_still_counts() {
        let weights = [1_000_000_u64, 1];
        let rects = distribute(
            &weights,
            Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 2,
            },
        );
        assert_eq!(rects.len(), 2, "the layout loses no children");
        assert!(rects[0].cells() > 0, "the big one is painted");
    }

    /// A zone's `arg` is the name's WIRE form, not what is painted.
    ///
    /// What is painted goes through the masking and a name with control
    /// bytes shows up as `�`: navigating with that would open another file,
    /// or none.
    #[test]
    fn the_zone_carries_the_name_in_wire_form() {
        let name = Segment::new(vec![0xFF, b'.', b'r', b's']).expect("segment");
        let child = DirUsageChild {
            name: name.clone(),
            kind: EntryKind::File,
            bytes: 100,
            entries: 1,
            partial: false,
        };
        let frame = squarify(&[child], 20, 3);
        let hit = frame.hits.first().expect("a zone");
        assert_eq!(hit.command, "nav.enter");
        assert_eq!(hit.arg.as_deref(), Some(name.to_wire().as_str()));
        assert_eq!(
            Segment::parse_wire(hit.arg.as_deref().expect("arg")).expect("round trip"),
            name,
            "the arg comes back as the exact bytes"
        );
    }

    /// What is painted is MASKED, even though the `arg` keeps the bytes.
    #[test]
    fn what_is_painted_is_masked() {
        let child = DirUsageChild {
            name: Segment::new(vec![0xFF, b'.', b'r', b's']).expect("segment"),
            kind: EntryKind::File,
            bytes: 100,
            entries: 1,
            partial: false,
        };
        let frame = squarify(&[child], 20, 3);
        let painted: String = frame
            .lines
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.clone()))
            .collect();
        assert!(painted.contains('\u{FFFD}'), "the raw byte is not painted");
        assert!(!painted.as_bytes().contains(&0xFF));
    }

    /// An incomplete rectangle is marked, and the mark goes in the LABEL:
    /// the color says what class the file is, and both things have to fit.
    #[test]
    fn a_partial_child_is_marked_without_losing_its_class() {
        let mut child = child("photos", 1000, EntryKind::Dir);
        child.partial = true;
        let frame = squarify(&[child], 30, 3);
        let painted: String = frame
            .lines
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.clone()))
            .collect();
        assert!(
            painted.contains(PARTIAL_MARK),
            "it says it is a lower bound"
        );
        assert_eq!(
            frame.lines[0][0].role,
            Some(Role::Info),
            "and it still paints as the directory it is"
        );
    }

    /// Zones do not exceed the cap, and the ones that survive are the BIG
    /// rectangles': `clamped` drops the excess silently, so losing a zone
    /// has to fall on the one nobody is going to click.
    #[test]
    fn zones_are_budgeted_from_largest_to_smallest() {
        let children: Vec<DirUsageChild> = (0..60_u64)
            .map(|i| child(&format!("f{i}"), (60 - i) * 1000, EntryKind::File))
            .collect();
        let frame = squarify(&children, 40, 30);
        assert!(frame.hits.len() <= MAX_HITS, "it does not exceed the cap");
        let biggest = frame.hits.iter().any(|h| h.arg.as_deref() == Some("f0"));
        assert!(biggest, "the biggest one keeps its zone");
    }

    /// The class comes from the type and the extension, read in BYTES.
    #[test]
    fn the_class_is_read_from_the_names_bytes() {
        assert_eq!(
            class_of(&child("x", 1, EntryKind::Dir)),
            ChildClass::Directory
        );
        assert_eq!(
            class_of(&child("main.RS", 1, EntryKind::File)),
            ChildClass::Code
        );
        assert_eq!(
            class_of(&child("a.tar", 1, EntryKind::File)),
            ChildClass::Archive
        );
        assert_eq!(
            class_of(&child("no_extension", 1, EntryKind::File)),
            ChildClass::Other
        );
    }

    /// A zero-sized slot paints nothing — which is not the same as painting
    /// an empty frame.
    #[test]
    fn a_zero_sized_slot_paints_nothing() {
        let frame = squarify(&[child("a", 10, EntryKind::File)], 0, 5);
        assert!(frame.lines.is_empty());
        assert!(frame.hits.is_empty());
    }
}
