//! Laying a help topic out for the GUI (H3f): [`norte_help::Topic`] →
//! semantic lines.
//!
//! Pure and GPUI-free on purpose. `main.rs` turns each [`HelpLine`] into
//! elements, so everything decided here — order, roles, the plugin badge, which
//! line each action landed on — is unit-testable without a window, which is the
//! same split `columns_view` and `context_menu` already use.
//!
//! Unlike the TUI's renderer this one does NOT wrap and does not budget cells:
//! GPUI measures and wraps text itself. What survives from that module is the
//! part that is not about a terminal — the ORDER of the page, and the fact that
//! the action map is built from positions in the emitted list.
//!
//! # This module masks NOTHING
//!
//! A claim about its INPUTS, and it only holds for a corpus that has been
//! through the gate:
//!
//! - a BUILT-IN topic is trusted text: its ids are cross-checked by the
//!   documentation gate and its prose is swept for terminal hazards by
//!   `norte-help`'s own corpus test;
//! - a PLUGIN topic was already masked and bounded by
//!   [`norte_help::parse_untrusted`], which refuses what it could not paint
//!   rather than rewriting it;
//! - a CHORD was masked by the resolver ([`crate::help_view::GuiChords`], which
//!   goes through `norte_frontend::keymap::paint_chord`).
//!
//! A caller feeding this an ungated corpus — a third-party topic set loaded at
//! runtime, say — must mask before calling in.

use norte_help::{Block, Callout, ChordResolver, CommandText, Lang, Span, Topic};
use norte_theme::Role;

/// One painted fragment: text plus the theme role that colours it.
#[derive(Debug, Clone)]
pub struct HelpSpan {
    /// Text to paint, already safe (see the module doc).
    pub text: String,
    /// Theme role. `main.rs` maps it to a colour through the same
    /// `ChromeColors`/`theme_map` path every other view uses.
    pub role: Role,
}

impl HelpSpan {
    /// A fragment in `role`.
    fn new(text: impl Into<String>, role: Role) -> Self {
        Self {
            text: text.into(),
            role,
        }
    }
}

/// One line of the body.
#[derive(Debug, Clone, Default)]
pub struct HelpLine {
    /// Fragments, in paint order. Empty = a blank separator line.
    pub spans: Vec<HelpSpan>,
    /// Leading indent, in steps of one space pair (bullets, code, table rows).
    pub indent: u8,
    /// Position of this line in `HelpState::actions()` when it IS an action
    /// row: `Some(i)` means "Enter on the `i`-th action does what this row
    /// says". The frontend uses it both to highlight the focused row and to
    /// scroll it into view.
    pub action: Option<usize>,
    /// The row's command cannot run right now: paint it dimmed. The reason is
    /// already IN `spans` — dimming alone leaves the reader guessing whether
    /// the row is inapplicable or the app is broken.
    pub dim: bool,
    /// The provenance line of a plugin page.
    pub badge: bool,
    /// Monospace line (a code fence).
    pub mono: bool,
}

impl HelpLine {
    /// A line of one fragment.
    fn one(text: impl Into<String>, role: Role) -> Self {
        Self {
            spans: vec![HelpSpan::new(text, role)],
            ..Self::default()
        }
    }

    /// A blank separator.
    fn blank() -> Self {
        Self::default()
    }

    /// A plain line of generated text in a MONOSPACED face: the shape the
    /// synthetic keyboard page arrives in.
    ///
    /// That page is built from the effective keymap rather than the corpus, so
    /// it never goes through [`render_topic`] — and its chord column is aligned
    /// with SPACES, which line up only under a fixed pitch.
    #[must_use]
    pub fn mono_text(text: impl Into<String>) -> Self {
        Self {
            mono: true,
            ..Self::one(text, Role::Regular)
        }
    }
}

/// What a row with no chord shows in the chord column.
const NO_CHORD: &str = "—";

/// Between a row's label and the reason it cannot run.
const SEP: &str = " — ";

/// Lays `topic` out.
///
/// `lang` resolves link titles and the badge's Fluent keys; `r` resolves
/// `{{cmd:…}}` marks, row labels and availability — it must be the FROZEN
/// resolver the overlay was opened with, so that every row of one page is
/// judged against one context.
///
/// The order is the TUI's, deliberately: title, plugin badge, blocks, the
/// runnable rows, then `see_also`. Two frontends showing the same page in two
/// orders is one page a reader cannot carry between them.
#[must_use]
pub fn render_topic(topic: &Topic, lang: Lang, r: &(impl ChordResolver + ?Sized)) -> Vec<HelpLine> {
    let mut lines = vec![HelpLine::one(topic.title.clone(), Role::Title)];

    if let Some(badge) = plugin_badge(topic, lang) {
        let mut line = HelpLine::one(badge, Role::Info);
        line.badge = true;
        lines.push(line);
    }

    for (i, block) in topic.blocks.iter().enumerate() {
        lines.push(HelpLine::blank());
        // A heading opens a SECTION, and the blank line that separates two
        // paragraphs says nothing about that; a second one does. Never before
        // the first block, which already sits under the title.
        if i > 0 && matches!(block, Block::Heading { .. }) {
            lines.push(HelpLine::blank());
        }
        lines.extend(render_block(block, lang, r));
    }

    let rows = norte_help::rows_of(topic, r);
    if !rows.is_empty() {
        lines.push(HelpLine::blank());
        for (i, row) in rows.iter().enumerate() {
            lines.push(row_line(row, i, lang));
        }
    }

    if !topic.see_also.is_empty() {
        lines.push(HelpLine::blank());
        for (i, id) in topic.see_also.iter().enumerate() {
            // The TITLE of the page the link opens, not its id: the sidebar row
            // for that same page says exactly this, and a reader who follows the
            // link must land somewhere they recognise as the place the row
            // named. An id the corpus cannot resolve keeps the id — a dangling
            // link is a corpus defect (`norte_help::check_corpus` catches it)
            // and a blank row would hide it from whoever is reading the page.
            let label = norte_help::topic(lang, id.as_str())
                .map_or_else(|| id.to_string(), |t| t.title.clone());
            let mut line = HelpLine::one(label, Role::Info);
            line.indent = 1;
            // The action indices continue the SAME sequence the command rows
            // started, because `HelpState::actions()` is one flat list in that
            // order: commands first, then `see_also`.
            line.action = Some(rows.len() + i);
            lines.push(line);
        }
    }

    lines
}

/// One runnable row: the chord, the label — and, when it cannot run, WHY.
///
/// Always ONE line: an action that spilled onto two would break the invariant
/// [`HelpLine::action`] rests on. The GUI does not budget cells (GPUI truncates
/// or wraps the element), so unlike the TUI nothing here is cut; what it keeps
/// from that renderer is which text the row LEADS with.
fn row_line(row: &norte_help::ResolvedRow, index: usize, lang: Lang) -> HelpLine {
    let available = row.row.avail.is_available();
    // An unavailable row is prose, not a key: it must not wear the key style
    // while it cannot be pressed.
    let key_role = if available { Role::Mark } else { Role::Info };
    let label_role = if available { Role::Regular } else { Role::Info };
    let mut spans = vec![
        HelpSpan::new(
            row.chord.clone().unwrap_or_else(|| NO_CHORD.to_owned()),
            key_role,
        ),
        HelpSpan::new(row.label.clone(), label_role),
    ];
    if let Some(reason) = row.row.avail.reason() {
        // The same Fluent ids the context menu paints, so a veto is explained
        // in one wording across the two surfaces of this frontend.
        let text = norte_i18n::t_in(lang, norte_frontend::availability::reason_key(reason));
        spans.push(HelpSpan::new(format!("{SEP}{text}"), Role::Info));
    }
    HelpLine {
        spans,
        indent: 1,
        action: Some(index),
        dim: !available,
        badge: false,
        mono: false,
    }
}

/// Paints one block of the closed vocabulary of [`Block`].
#[must_use]
fn render_block(block: &Block, lang: Lang, r: &(impl ChordResolver + ?Sized)) -> Vec<HelpLine> {
    match block {
        // Levels carry no `#` and no indent: the corpus nests three deep at
        // most and the heading style is the signal that survives a narrow body.
        Block::Heading { text, .. } => vec![HelpLine::one(text.clone(), Role::Title)],
        Block::Paragraph(spans) => vec![HelpLine {
            spans: frags(spans, lang, r),
            ..HelpLine::default()
        }],
        Block::Bullets(items) => items
            .iter()
            .map(|item| {
                let mut spans = vec![HelpSpan::new("• ", Role::Info)];
                spans.extend(frags(item, lang, r));
                HelpLine {
                    spans,
                    indent: 1,
                    ..HelpLine::default()
                }
            })
            .collect(),
        // A code line is NOT wrapped or joined: a wrapped code line is a lie
        // about what to type. One line in, one line out; `main.rs` paints it
        // monospaced and lets it scroll.
        Block::Code { text, .. } => text
            .lines()
            .map(|l| HelpLine {
                spans: vec![HelpSpan::new(l.to_owned(), Role::Mark)],
                indent: 1,
                mono: true,
                ..HelpLine::default()
            })
            .collect(),
        Block::Table { header, rows } => table(header, rows),
        Block::Callout { kind, spans } => {
            let (glyph, role) = match kind {
                Callout::Note => ("ℹ ", Role::Info),
                Callout::Warn => ("⚠ ", Role::Warning),
                Callout::Tip => ("💡 ", Role::Info),
            };
            let mut out = vec![HelpSpan::new(glyph, role)];
            out.extend(frags(spans, lang, r));
            vec![HelpLine {
                spans: out,
                ..HelpLine::default()
            }]
        }
    }
}

/// Header, then the rows.
///
/// Cells are read by ZIPPING each row against the header rather than indexing
/// by position. The parser normalises rows to `header.len()` (see
/// [`Block::Table`]) and this relies on it — but relying on a contract must not
/// mean panicking when it changes, and these rows come from a `split` over text
/// a plugin wrote.
fn table(header: &[String], rows: &[Vec<String>]) -> Vec<HelpLine> {
    if header.is_empty() {
        return Vec::new();
    }
    let mut out = vec![HelpLine {
        spans: header
            .iter()
            .map(|h| HelpSpan::new(h.clone(), Role::Title))
            .collect(),
        indent: 1,
        ..HelpLine::default()
    }];
    for row in rows {
        out.push(HelpLine {
            spans: row
                .iter()
                .zip(header)
                .map(|(cell, _)| HelpSpan::new(cell.clone(), Role::Regular))
                .collect(),
            indent: 1,
            ..HelpLine::default()
        });
    }
    out
}

/// The styled fragments of a run of spans.
///
/// The `{{cmd:…}}` arm is the one that matters: it matches on [`CommandText`]
/// rather than flattening to text, because a chord is painted as a key and a
/// name as prose — collapsing them makes every mark look like something the
/// reader can press, including the ones that are not.
fn frags(spans: &[Span], lang: Lang, r: &(impl ChordResolver + ?Sized)) -> Vec<HelpSpan> {
    spans
        .iter()
        .map(|span| match span {
            Span::Text(t) => HelpSpan::new(t.clone(), Role::Regular),
            Span::Strong(t) => HelpSpan::new(t.clone(), Role::Title),
            Span::Emph(t) => HelpSpan::new(t.clone(), Role::Info),
            Span::Code(t) => HelpSpan::new(t.clone(), Role::Mark),
            // The page's TITLE, exactly as the `see_also` rows and the sidebar
            // paint it: one thing spelled one way. An unresolvable id keeps the
            // id, for the reason the `see_also` arm gives.
            Span::TopicLink(id) => HelpSpan::new(
                norte_help::topic(lang, id.as_str())
                    .map_or_else(|| id.to_string(), |t| t.title.clone()),
                Role::Info,
            ),
            Span::CommandRef(c) => match norte_help::render_command(c, r) {
                CommandText::Chord(k) => HelpSpan::new(k, Role::Mark),
                CommandText::Name(n) => HelpSpan::new(n, Role::Info),
            },
        })
        .collect()
}

/// The provenance line of a PLUGIN page, `None` only for something the host
/// wrote.
///
/// # It is never `None` for a plugin topic, and that is the whole point
///
/// In the TUI it used to be: the line was assembled from three OPTIONAL facts —
/// publisher, cut short, decoded lossily — so a plugin declaring
/// `publisher = ""` and shipping a clean `help.md` got no line at all, and its
/// page then had the exact shape of a built-in one. That is not cosmetic: the
/// reader meets a plugin page from the extension manager, at the moment they
/// are deciding whether to approve it. So the first segment is UNCONDITIONAL
/// and constant — a line that is always there is one a reader can learn to
/// trust, and one that shows up only sometimes teaches the opposite of the
/// truth when it is absent.
///
/// The plugin's ID is deliberately not painted: an id is a LOOKUP KEY and has
/// never been through a mask, and a module whose header promises it masks
/// nothing must not paint the one string nobody masked. The reader already
/// knows which extension they opened — they arrived on its row, under its name.
///
/// `is_blank_id` and not `str::trim`: `"\u{3164}"` (HANGUL FILLER) is not
/// whitespace, so a trim-based check calls it a publisher and paints
/// `published by ` with nothing after it.
fn plugin_badge(topic: &Topic, lang: Lang) -> Option<String> {
    let norte_help::Origin::Plugin {
        publisher,
        truncated,
        lossy,
        ..
    } = &topic.origin
    else {
        return None;
    };
    let mut parts: Vec<String> = vec![norte_i18n::t_in(lang, "help-plugin-origin")];
    if let Some(p) = publisher.as_deref().filter(|p| !norte_help::is_blank_id(p)) {
        parts.push(norte_i18n::ta_in(lang, "help-plugin-by", &[("who", p)]));
    }
    if *truncated {
        parts.push(norte_i18n::t_in(lang, "help-plugin-truncated"));
    }
    if *lossy {
        parts.push(norte_i18n::t_in(lang, "help-plugin-lossy"));
    }
    Some(parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_help::{Availability, Reason};

    struct Fixed;
    impl ChordResolver for Fixed {
        fn chord(&self, c: &str) -> Option<String> {
            (c == "pane.copy").then(|| "F5".to_owned())
        }
        fn label(&self, c: &str) -> String {
            c.to_owned()
        }
        fn availability(&self, c: &str) -> Availability {
            if c == "pane.delete" {
                Availability::Unavailable {
                    reason: Reason::ReadOnlyBackend,
                }
            } else {
                Availability::Available
            }
        }
    }

    fn text_of(lines: &[HelpLine]) -> String {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn una_pagina_del_corpus_se_convierte_en_lineas_con_rol() {
        let topic = norte_help::topic(Lang::En, "copying").expect("corpus topic");
        let lines = render_topic(topic, Lang::En, &Fixed);
        assert!(!lines.is_empty());
        assert_eq!(lines[0].spans[0].role, Role::Title, "the title leads");
        assert!(
            text_of(&lines).contains("F5"),
            "a {{cmd:}} mark became the reader's chord"
        );
    }

    /// The action indices are indices INTO `HelpState::actions()`, so the pin
    /// has to compare against that list and not against a count re-derived from
    /// the same assumption the renderer encodes. A reordering inside
    /// `rebuild_actions` would otherwise mis-map every Enter while this test
    /// stayed green.
    #[test]
    fn el_mapa_de_acciones_indexa_las_acciones_del_modelo() {
        use norte_frontend::help::{Action, HelpState};

        let topic = norte_help::topic(Lang::En, "copying").expect("corpus topic");
        let mut state = HelpState::new(Lang::En, "Keyboard".to_owned());
        state.open(&topic.id);
        let actions = state.actions().to_vec();
        assert!(!actions.is_empty());

        let lines = render_topic(topic, Lang::En, &Fixed);
        let mapped: Vec<usize> = lines.iter().filter_map(|l| l.action).collect();
        assert_eq!(
            mapped,
            (0..actions.len()).collect::<Vec<_>>(),
            "one line per action of the model, in the model's order"
        );

        // And each mapped line NAMES the action it points at, which is what
        // makes Enter land where the highlight is.
        for (line, action) in lines.iter().filter(|l| l.action.is_some()).zip(&actions) {
            let text: String = line.spans.iter().map(|s| s.text.as_str()).collect();
            let expected = match action {
                Action::Run(cmd) => Fixed.label(cmd),
                Action::Open(id) => norte_help::topic(Lang::En, id.as_str())
                    .map_or_else(|| id.to_string(), |t| t.title.clone()),
            };
            assert!(
                text.contains(&expected),
                "row {text:?} does not name {action:?}"
            );
        }
    }

    #[test]
    fn una_fila_no_disponible_lleva_su_razon_y_va_atenuada() {
        let topic = norte_help::topic(Lang::En, "copying").expect("corpus topic");
        assert!(
            topic.commands.iter().any(|c| c == "pane.delete"),
            "this test needs the row the fixture dims"
        );
        let lines = render_topic(topic, Lang::En, &Fixed);
        let dimmed: Vec<&HelpLine> = lines.iter().filter(|l| l.dim).collect();
        assert!(!dimmed.is_empty(), "the resolver dims pane.delete");
        let reason = norte_i18n::t_in(
            Lang::En,
            norte_frontend::availability::reason_key(Reason::ReadOnlyBackend),
        );
        for l in dimmed {
            let text: String = l.spans.iter().map(|s| s.text.as_str()).collect();
            assert!(text.contains(&reason), "a dimmed row says WHY: {text:?}");
        }
    }

    #[test]
    fn toda_pagina_de_plugin_lleva_insignia_aunque_no_declare_nada() {
        // No publisher, nothing truncated, nothing lossy: the case that used to
        // render byte-for-byte like a page norte wrote.
        let parsed = norte_help::parse_untrusted(b"body", "acme.ftp", None);
        let lines = render_topic(&parsed.topic, Lang::En, &Fixed);
        assert!(
            lines.iter().any(|l| l.badge),
            "the line that marks third-party prose is unconditional"
        );
    }

    #[test]
    fn la_insignia_reporta_corte_y_decodificacion_perdida() {
        let parsed = norte_help::parse_untrusted(b"body", "acme.ftp", Some("ACME".to_owned()))
            .fold_flags(true, true);
        let lines = render_topic(&parsed.topic, Lang::En, &Fixed);
        let badge = lines
            .iter()
            .find(|l| l.badge)
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .expect("plugin page");
        assert!(badge.contains(&norte_i18n::t_in(Lang::En, "help-plugin-truncated")));
        assert!(badge.contains(&norte_i18n::t_in(Lang::En, "help-plugin-lossy")));
        assert!(badge.contains("ACME"));
    }

    #[test]
    fn el_texto_hostil_de_un_plugin_llega_ya_enmascarado() {
        let parsed = norte_help::parse_untrusted(
            "a \u{202e}reversed\u{202e} paragraph\n\n`co\u{7}de`\n".as_bytes(),
            "acme.ftp",
            None,
        );
        let lines = render_topic(&parsed.topic, Lang::En, &Fixed);
        for l in &lines {
            for s in &l.spans {
                assert!(
                    !s.text.contains('\u{202e}') && !s.text.contains('\u{7}'),
                    "hazard painted: {:?}",
                    s.text
                );
            }
        }
    }

    #[test]
    fn una_linea_de_codigo_no_se_parte_ni_se_une() {
        let block = Block::Code {
            lang: None,
            text: "one\ntwo".to_owned(),
        };
        let lines = render_block(&block, Lang::En, &Fixed);
        assert_eq!(lines.len(), 2, "one line in, one line out");
        assert!(lines.iter().all(|l| l.mono));
    }

    #[test]
    fn una_tabla_dentada_no_entra_en_panico() {
        // The parser normalises rows; this asserts the renderer does not TRUST
        // it — the rows come from a split over text a plugin wrote.
        let block = Block::Table {
            header: vec!["a".into(), "b".into()],
            rows: vec![vec!["1".into()], vec!["1".into(), "2".into(), "3".into()]],
        };
        let lines = render_block(&block, Lang::En, &Fixed);
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[2].spans.len(),
            2,
            "extra cells are dropped, not painted"
        );
    }
}
