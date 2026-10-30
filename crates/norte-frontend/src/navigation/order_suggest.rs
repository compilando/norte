//! What can come next in an order line (#429): the rows the `:` box offers
//! for the word under the cursor. Pure: the pane's names and the known
//! folders arrive as data, taken when the box opened.
//!
//! Every row's text is what gets TYPED, built by the grammar's own printer
//! from bytes the reader did not type, so what the reader sees is exactly
//! what the line will hold, and it cannot carry a byte a terminal would act
//! on.

use std::collections::HashMap;
use std::ops::Range;

use norte_encoding::NameEncoding;
use norte_orders::Span;
use norte_proto::{EntryKind, VPath};

use super::goto::{CAP_PER_SECTION, GotoRow, K_SET, PREFIX_ORDER, SECTION_COMMANDS};

/// How many of the pane's names the box keeps when it opens: the marked
/// first, then the listing in its order. Ranking a 100k-row folder on every
/// key would make typing lag.
pub const NAMES_KEPT: usize = 2000;

/// What an order line is completed against: the focused pane as it was when
/// the box opened.
///
/// ```
/// use norte_frontend::goto::OrderContext;
/// assert!(OrderContext::default().names().is_empty());
/// ```
#[derive(Debug, Clone, Default)]
pub struct OrderContext {
    here: Option<VPath>,
    names: Vec<OrderName>,
    /// Whether a name of the listing is a folder OF `here`. Not in a results
    /// tab, whose hits live in folders below it.
    browsing: bool,
}

/// One row of the pane, as the completion needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderName {
    /// The name's bytes, as listed.
    pub bytes: Vec<u8>,
    /// What it is.
    pub kind: EntryKind,
    /// The `sel` term that selects it and no other row of the listing.
    term: String,
}

impl OrderContext {
    /// A context over `names` (in the order to offer them) in the folder
    /// `here`, read with `enc`. `browsing`: the names are `here`'s own.
    ///
    /// A name's `sel` term is a bare glob only when nothing else in the
    /// listing folds to it — a case twin, an NFC twin, a legacy name the pane
    /// decodes the same — and the whole listing was seen; otherwise `name=`,
    /// which compares bytes.
    #[must_use]
    pub fn new(
        here: Option<VPath>,
        enc: Option<NameEncoding>,
        names: impl IntoIterator<Item = (Vec<u8>, EntryKind)>,
        browsing: bool,
    ) -> Self {
        let mut raw: Vec<(Vec<u8>, EntryKind)> = names.into_iter().collect();
        // Past the cap a twin can hide in what was not kept.
        let complete = raw.len() <= NAMES_KEPT;
        raw.truncate(NAMES_KEPT);
        let mut folds: HashMap<String, usize> = HashMap::new();
        for (b, _) in &raw {
            *folds.entry(crate::nav::fold_with(b, enc)).or_default() += 1;
        }
        let names = raw
            .into_iter()
            .map(|(bytes, kind)| {
                let twin = folds
                    .get(&crate::nav::fold_with(&bytes, enc))
                    .is_some_and(|n| *n > 1);
                let literal = !complete || twin || std::str::from_utf8(&bytes).is_err();
                OrderName {
                    term: norte_orders::name_term(&bytes, literal),
                    bytes,
                    kind,
                }
            })
            .collect();
        Self {
            here,
            names,
            browsing,
        }
    }

    /// The context of `pane`: its folder, and its real rows (never the `..`
    /// row, whose path is the parent) with the marked ones first. Taken
    /// before the live `sel` preview touches the marks.
    #[must_use]
    pub fn of(pane: &crate::PaneState) -> Self {
        let real = pane.real_entries();
        let marked = real.iter().filter(|e| pane.is_marked(e));
        let rest = real.iter().filter(|e| !pane.is_marked(e));
        let names = marked
            .chain(rest)
            .filter_map(|e| Some((e.path.segments().last()?.to_vec(), e.kind)))
            // One past the cap, so `new` knows the listing did not fit.
            .take(NAMES_KEPT + 1);
        Self::new(
            Some(pane.dir().clone()),
            pane.name_encoding(),
            names,
            !pane.is_results(),
        )
    }

    /// The names kept, marked first.
    #[must_use]
    pub fn names(&self) -> &[OrderName] {
        &self.names
    }

    fn dirs(&self) -> impl Iterator<Item = &OrderName> {
        self.names
            .iter()
            .filter(|n| self.browsing && n.kind == EntryKind::Dir)
    }
}

/// Where the word being typed sits.
struct At<'q> {
    /// The query up to the word, `:` included.
    before: &'q str,
    /// The word.
    word: &'q str,
    /// The stage's verb, if the word is not it.
    verb: Option<&'q str>,
    /// How many words of the stage come before this one, the verb included.
    index: usize,
}

/// Splits as the grammar does (an escaped space or `|`, or a quoted run,
/// stays in its word). `None` inside an open quote or escape.
fn at(query: &str) -> Option<At<'_>> {
    let start = PREFIX_ORDER.len();
    let body = query.get(start..)?;
    let spans = norte_orders::spans(body)?;
    let (word, prior): (Range<usize>, &[Span]) = match spans.split_last() {
        Some((Span::Word(r), prior)) if r.end == body.len() => (r.clone(), prior),
        _ => (body.len()..body.len(), &spans),
    };
    let stage_start = prior
        .iter()
        .rposition(|s| matches!(s, Span::Pipe(_)))
        .map_or(0, |i| i + 1);
    let words: Vec<&Range<usize>> = prior[stage_start..]
        .iter()
        .filter_map(|s| match s {
            Span::Word(r) => Some(r),
            Span::Pipe(_) => None,
        })
        .collect();
    Some(At {
        before: &query[..start + word.start],
        word: &body[word],
        verb: words.first().map(|r| &body[(*r).clone()]),
        index: words.len(),
    })
}

/// The suggestions for a line, and whether the first one completes syntax
/// (a verb, a key): only those may be chosen before the reader chooses.
pub(crate) struct Suggestions {
    pub rows: Vec<GotoRow>,
    pub syntax_first: bool,
}

/// A row and whether it completes syntax rather than naming a value.
struct Sugg {
    row: GotoRow,
    syntax: bool,
}

/// The verbs that need something after them: typed whole, the box offers
/// what comes next instead of nothing.
const TAKES_ARGS: &[&str] = &["sel", "cp", "mv", "cd"];

/// The completions for the word under the cursor of `query` (an order line,
/// `:` included). `places` are the folders the box already knows (favorites,
/// history, popular), best first. A word already typed whole is never
/// offered: Enter must keep meaning what the line says.
pub(crate) fn rows(query: &str, ctx: &OrderContext, places: &[VPath]) -> Suggestions {
    let Some(a) = at(query) else {
        return Suggestions {
            rows: Vec::new(),
            syntax_first: false,
        };
    };
    let mut out = match a.verb {
        None if TAKES_ARGS.contains(&a.word) => {
            // `:cp` → what follows `cp `, typed with its space.
            return rows(&format!("{query} "), ctx, places);
        }
        None if norte_orders::VERBS.contains(&a.word) => Vec::new(),
        None => verbs(&a),
        Some("sel") => sel(&a, ctx),
        Some("cp" | "mv") if a.index == 1 => dests(&a, ctx, places),
        Some("cd") if a.index == 1 => folders(&a, "", ctx, places),
        Some(_) => Vec::new(),
    };
    out.retain(|s| s.row.text != a.word);
    out.truncate(CAP_PER_SECTION);
    Suggestions {
        syntax_first: out.first().is_some_and(|s| s.syntax),
        rows: out.into_iter().map(|s| s.row).collect(),
    }
}

fn row(a: &At<'_>, text: String) -> GotoRow {
    GotoRow {
        section: SECTION_COMMANDS.id,
        key: format!("{K_SET}{}{text}", a.before),
        text,
        ..GotoRow::default()
    }
}

fn syntax(row: GotoRow) -> Sugg {
    Sugg { row, syntax: true }
}

fn value(row: GotoRow) -> Sugg {
    Sugg { row, syntax: false }
}

/// What the reader typed, written the way the printer writes the rows, so
/// `two\ w` ranks `two\x20words.txt`. As typed if it is not one word.
fn canon(typed: &str) -> String {
    decode(typed).map_or_else(|| typed.to_owned(), |b| norte_orders::escape_name(&b))
}

/// The bytes one typed word stands for, escapes resolved.
fn decode(typed: &str) -> Option<Vec<u8>> {
    let stages = norte_orders::parse(&format!("mkdir {typed}")).ok()?.stages;
    match stages.as_slice() {
        [norte_orders::Stage::Mkdir(b)] => Some(b.clone()),
        _ => None,
    }
}

/// Ranks by the typed word, or keeps the first [`CAP_PER_SECTION`] in order
/// with nothing typed.
fn ranked(rows: impl Iterator<Item = GotoRow>, needle: &str) -> Vec<GotoRow> {
    if needle.is_empty() {
        rows.take(CAP_PER_SECTION).collect()
    } else {
        super::goto::rank(rows.collect(), needle, false)
    }
}

fn verbs(a: &At<'_>) -> Vec<Sugg> {
    let verb = |v: &str| {
        let mut r = row(a, format!("{v} "));
        v.clone_into(&mut r.text);
        r
    };
    let verbs = norte_orders::VERBS.iter().map(|v| verb(v));
    let mut out = ranked(verbs, a.word);
    if !a.word.is_empty() {
        let ids = crate::keymap::catalogue::CATALOGUE
            .iter()
            .map(|d| verb(d.name));
        out.extend(ranked(ids, a.word));
    }
    out.into_iter().map(syntax).collect()
}

/// A `sel` term: a key, a key's value, or a name of the pane.
fn sel(a: &At<'_>, ctx: &OrderContext) -> Vec<Sugg> {
    let (neg, term) = a
        .word
        .strip_prefix('!')
        .map_or(("", a.word), |rest| ("!", rest));
    // `key` is the typed text the rows keep; it reaches a painted row, so it
    // must be plain ASCII that needs no escape.
    let keyed = |key: &str, values: Vec<String>| -> Vec<Sugg> {
        let typed = term[key.len()..].to_lowercase();
        values
            .into_iter()
            .filter(|v| v.to_lowercase().starts_with(&typed))
            .map(|v| value(row(a, format!("{neg}{key}{v}"))))
            .collect()
    };
    if let Some(list) = term.strip_prefix("ext:") {
        // Only the last of `ext:pdf,PN` is being typed.
        let done = list.rfind(',').map_or("", |i| &list[..=i]);
        if !done.bytes().all(|b| b.is_ascii_alphanumeric() || b == b',') {
            return Vec::new();
        }
        let listed: Vec<String> = done.split(',').map(str::to_ascii_lowercase).collect();
        let fresh = extensions(ctx)
            .into_iter()
            .filter(|e| !listed.contains(e))
            .collect();
        return keyed(&format!("ext:{done}"), fresh);
    }
    if term.starts_with("type:") {
        return keyed("type:", ["file", "dir", "link"].map(str::to_owned).to_vec());
    }
    if let Some(typed) = term
        .strip_prefix("name:")
        .or_else(|| term.strip_prefix("name="))
    {
        // Always `name=`: a name offered as a `name:` glob would be a pattern.
        let typed = typed.to_lowercase();
        let names = ctx
            .names
            .iter()
            .map(|n| norte_orders::escape_name(&n.bytes))
            .filter(|e| e.to_lowercase().starts_with(&typed))
            .take(CAP_PER_SECTION)
            .map(|e| value(row(a, format!("{neg}name={e}"))));
        return names.collect();
    }
    let mut out: Vec<Sugg> = norte_orders::QUERY_KEYS
        .iter()
        .filter(|k| !term.is_empty() && k.starts_with(term))
        .map(|k| syntax(row(a, format!("{neg}{k}"))))
        .collect();
    // A pattern is a pattern: `*.pdf` is not completed into one name. Judged
    // on the bytes, so a typed `\ ` is a space and `\x2a` a star.
    let pattern = |b: &u8| matches!(b, b'*' | b'?' | b'[' | b']' | b'{' | b'}' | b'\\');
    let plain = term.is_empty() || decode(term).is_some_and(|b| !b.iter().any(pattern));
    if plain && !term.contains(['<', '>', '=', ':']) {
        let names = ctx.names.iter().map(|n| row(a, format!("{neg}{}", n.term)));
        let needle = if term.is_empty() {
            String::new()
        } else {
            format!("{neg}{}", canon(term))
        };
        out.extend(ranked(names, &needle).into_iter().map(value));
    }
    out
}

/// The extensions the pane holds, folded, in listing order: only plain
/// ASCII ones, which need no escape inside a comma list.
fn extensions(ctx: &OrderContext) -> Vec<String> {
    let mut seen = Vec::<String>::new();
    for n in ctx.names.iter().filter(|n| n.kind != EntryKind::Dir) {
        let Some(dot) = n.bytes.iter().rposition(|b| *b == b'.') else {
            continue;
        };
        let ext = &n.bytes[dot + 1..];
        if dot == 0 || ext.is_empty() || !ext.iter().all(u8::is_ascii_alphanumeric) {
            continue;
        }
        let ext = String::from_utf8_lossy(ext).to_ascii_lowercase();
        if !seen.contains(&ext) {
            seen.push(ext);
        }
    }
    seen
}

/// After `cp`/`mv`: the two keywords, then folders behind a `>`.
fn dests(a: &At<'_>, ctx: &OrderContext, places: &[VPath]) -> Vec<Sugg> {
    let typed = a.word.strip_prefix('>').unwrap_or(a.word);
    let mut out: Vec<Sugg> = ["other", "here"]
        .iter()
        .filter(|k| k.starts_with(typed))
        .map(|k| value(row(a, format!(">{k}"))))
        .collect();
    out.extend(folders(a, ">", ctx, places));
    out
}

/// Folders to go or send things to: this pane's own, by name, then the
/// known places on the same backend, absolute. A place elsewhere is left
/// out: a typed `/x` resolves on the pane's backend, and a URL would need
/// percent-free bytes the wire form does not keep.
fn folders(a: &At<'_>, lead: &str, ctx: &OrderContext, places: &[VPath]) -> Vec<Sugg> {
    let typed = a.word.strip_prefix(lead).unwrap_or(a.word);
    let mut rows: Vec<GotoRow> = ctx
        .dirs()
        .map(|n| row(a, format!("{lead}{}", norte_orders::escape_dest(&n.bytes))))
        .collect();
    if let Some(here) = &ctx.here {
        for p in places {
            if p == here || p.scheme() != here.scheme() || p.authority() != here.authority() {
                continue;
            }
            let mut abs = Vec::new();
            for seg in p.segments() {
                abs.push(b'/');
                abs.extend_from_slice(seg);
            }
            if abs.is_empty() {
                abs.push(b'/');
            }
            let text = format!("{lead}{}", norte_orders::escape_dest(&abs));
            if !rows.iter().any(|r| r.text == text) {
                rows.push(row(a, text));
            }
        }
    }
    let needle = if typed.is_empty() {
        String::new()
    } else {
        canon(typed)
    };
    ranked(rows.into_iter(), &needle)
        .into_iter()
        .map(value)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{OrderContext, rows};
    use norte_proto::{EntryKind, VPath};

    fn ctx_of(names: &[(&[u8], EntryKind)]) -> OrderContext {
        OrderContext::new(
            VPath::parse("file:///home/u").ok(),
            None,
            names.iter().map(|(n, k)| (n.to_vec(), *k)),
            true,
        )
    }

    fn ctx() -> OrderContext {
        ctx_of(&[
            (b"report.pdf", EntryKind::File),
            (b"Documents", EntryKind::Dir),
            (b"photo.PNG", EntryKind::File),
            (b"two words.txt", EntryKind::File),
            (b"a*", EntryKind::File),
        ])
    }

    fn texts_in(query: &str, ctx: &OrderContext) -> Vec<String> {
        let places = [
            VPath::parse("file:///srv/backup").expect("wire"),
            VPath::parse("sftp://h/far").expect("wire"),
        ];
        rows(query, ctx, &places)
            .rows
            .into_iter()
            .map(|r| r.text)
            .collect()
    }

    fn texts(query: &str) -> Vec<String> {
        texts_in(query, &ctx())
    }

    fn key_of(query: &str, text: &str) -> String {
        rows(query, &ctx(), &[])
            .rows
            .into_iter()
            .find(|r| r.text == text)
            .map(|r| r.key)
            .unwrap_or_default()
    }

    /// #429's own case: `:cp` offered nothing to pick.
    #[test]
    fn a_whole_cp_offers_its_destinations() {
        let t = texts(":cp");
        assert_eq!(&t[..2], [">other", ">here"]);
        assert!(t.contains(&">Documents".to_owned()));
        assert!(t.contains(&">/srv/backup".to_owned()));
        assert!(
            !t.iter().any(|t| t.contains("far")),
            "a place on another backend is not typed as a local path"
        );
        assert_eq!(key_of(":cp", ">other"), "set::cp >other");
        assert!(
            !rows(":cp", &ctx(), &[]).syntax_first,
            "a destination is a value: nothing chooses it for the reader"
        );
    }

    #[test]
    fn a_destination_completes_by_what_follows_the_arrow() {
        let t = texts(":sel *.pdf | mv >ot");
        assert_eq!(t.first().map(String::as_str), Some(">other"));
        assert!(!t.contains(&">here".to_owned()));
        assert_eq!(
            texts(":mv >Doc").first().map(String::as_str),
            Some(">Documents")
        );
        assert!(!texts(":mv >other").contains(&">other".to_owned()));
    }

    #[test]
    fn a_sel_word_offers_keys_and_the_panes_names() {
        let t = texts(":sel rep");
        assert_eq!(t.first().map(String::as_str), Some("report.pdf"));
        // A name with a space is written escaped; one with a glob char, literal.
        assert!(texts(":sel two").contains(&r"two\x20words.txt".to_owned()));
        assert!(texts(":sel a").contains(&"name=a*".to_owned()));
        assert!(texts(":sel !rep").contains(&"!report.pdf".to_owned()));
        assert!(
            rows(":sel ex", &ctx(), &[]).syntax_first,
            "`ext:` is syntax"
        );
    }

    /// A bare glob folds: a name with a twin in the listing, or one the
    /// matcher would read as an escape, is offered literally.
    #[test]
    fn a_name_that_a_glob_would_confuse_is_offered_literally() {
        let c = ctx_of(&[
            (b"Report.pdf", EntryKind::File),
            (b"report.pdf", EntryKind::File),
            (br"a\b", EntryKind::File),
            (b"ab", EntryKind::File),
            (b"caf\xc3\xa9", EntryKind::File),
            (b"cafe\xcc\x81", EntryKind::File),
            (b"\xff", EntryKind::File),
        ]);
        let t = texts_in(":sel rep", &c);
        assert!(t.contains(&"name=Report.pdf".to_owned()), "{t:?}");
        assert!(t.contains(&"name=report.pdf".to_owned()), "{t:?}");
        assert!(texts_in(":sel a", &c).contains(&r"name=a\x5cb".to_owned()));
        let t = texts_in(":sel caf", &c);
        assert!(t.iter().all(|t| t.starts_with("name=")), "NFC twins: {t:?}");
        assert!(texts_in(":sel \\xf", &c).is_empty(), "inside an escape");
        assert!(texts_in(":sel x", &c).contains(&r"name=\xff".to_owned()));
    }

    #[test]
    fn a_pattern_is_not_completed_into_one_name() {
        assert!(texts(":sel *.pd").is_empty());
    }

    #[test]
    fn key_values_come_from_the_pane() {
        assert_eq!(texts(":sel ext:p"), ["ext:pdf", "ext:png"]);
        assert_eq!(texts(":sel ext:pdf,P"), ["ext:pdf,png"]);
        assert_eq!(texts(":sel type:d"), ["type:dir"]);
        assert!(texts(":sel name:rep").contains(&"name=report.pdf".to_owned()));
    }

    /// What the reader typed before a value is repeated in the row only when
    /// it is plain ASCII: a pasted bidi override must not reach a raw row.
    #[test]
    fn typed_text_reaches_a_row_only_when_plain() {
        assert!(texts(":sel ext:\u{202e}pdf,p").is_empty());
        for q in [":sel ext:p", ":sel rep", ":cp >", ":sel type:"] {
            for t in texts(q) {
                assert!(
                    !t.chars().any(norte_encoding::is_terminal_hazard),
                    "{q}: {t:?}"
                );
            }
        }
    }

    /// Words split where the grammar splits them: an escaped space or pipe
    /// stays inside its word, and an open quote offers nothing.
    #[test]
    fn words_split_as_the_grammar_splits_them() {
        let k = |q: &str, t: &str| {
            rows(q, &ctx(), &[])
                .rows
                .into_iter()
                .find(|r| r.text == t)
                .map(|r| r.key)
        };
        assert_eq!(
            k(r":sel two\ w", r"two\x20words.txt"),
            Some(r"set::sel two\x20words.txt".to_owned())
        );
        assert!(texts(r":sel a\|c").iter().all(|t| t != "cp"));
        assert!(texts(":sel 'two w").is_empty());
    }

    #[test]
    fn cd_offers_folders_and_mkdir_nothing() {
        assert!(texts(":cd Do").contains(&"Documents".to_owned()));
        assert!(texts(":mkdir x").is_empty());
        assert!(texts(":trash ").is_empty());
    }

    /// A results tab's hits live below its folder: none is offered as a
    /// folder OF it.
    #[test]
    fn a_results_tab_offers_no_relative_folder() {
        let c = OrderContext::new(
            VPath::parse("file:///root").ok(),
            None,
            [(b"sub".to_vec(), EntryKind::Dir)],
            false,
        );
        assert!(!texts_in(":cp >", &c).contains(&">sub".to_owned()));
        assert!(!texts_in(":cd s", &c).contains(&"sub".to_owned()));
    }

    /// The `..` row's path is the parent: its name is not this folder's.
    #[test]
    fn the_parent_row_is_not_a_folder_of_the_pane() {
        let parent = VPath::parse("file:///home").expect("wire");
        let mut pane = crate::PaneState::new(
            VPath::parse("file:///home/u").expect("wire"),
            vec![norte_proto::Entry {
                path: VPath::parse("file:///home/u/real").expect("wire"),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            }],
        );
        pane.set_parent_row(true);
        assert!(
            pane.entries().iter().any(|e| e.path == parent),
            "the .. row is there"
        );
        let c = OrderContext::of(&pane);
        let names: Vec<&[u8]> = c.names().iter().map(|n| n.bytes.as_slice()).collect();
        assert_eq!(names, [b"real".as_slice()]);
    }
}
