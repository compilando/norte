//! Hostile mode: a plugin `help.md` is THIRD-PARTY text. It never fails, it
//! always bounds, it always masks. The hostile strings come from the
//! canonical `norte-testkit` corpus (house rule: fixtures are code).
//!
//! Everything asserted here is a claim `Origin::Plugin` makes in its rustdoc
//! — "third-party text, already masked and bounded by the parser". A claim
//! nobody tests is a comment.

use norte_help::{Block, Callout, Limits, Origin, Parsed, Span, Topic, parse_untrusted};

/// The cap on every string a plugin header contributes to the model, in
/// CHARACTERS. Pinned here on purpose: the number is the one the plugin
/// manifest already applies to `description`, and moving it must be a
/// deliberate act that turns this test red.
const HEADER_CHARS: usize = 280;

/// A valid header, so a test can aim at the BODY without the header
/// fallback getting in the way.
const HEAD: &str = "+++\nid = \"x\"\ntitle = \"X\"\n+++\n";

#[test]
fn without_front_matter_it_neither_fails_nor_invents_a_title() {
    let parsed = parse_untrusted(b"just a body", "acme-ftp", None);
    assert_eq!(parsed.topic.id.as_str(), "acme-ftp");
    assert_eq!(
        parsed.topic.title, "acme-ftp",
        "no header means no title: the plugin id is the only honest one"
    );
    assert_eq!(
        parsed.topic.origin,
        Origin::Plugin {
            id: "acme-ftp".to_owned(),
            publisher: None,
            truncated: false,
            lossy: false,
        }
    );
    assert!(!parsed.truncated && !parsed.lossy);
    assert_eq!(
        parsed.topic.blocks,
        vec![Block::Paragraph(vec![Span::Text("just a body".to_owned())])],
        "the whole file is body: a broken header is not a reason to lose it"
    );
}

#[test]
fn invalid_utf8_decodes_lossily_and_raises_the_flag() {
    let mut src = HEAD.as_bytes().to_vec();
    src.extend_from_slice(b"\xff\xfe body");
    let parsed = parse_untrusted(&src, "p", None);
    assert!(parsed.lossy, "the flag feeds the UI badge");
    assert!(!parsed.topic.blocks.is_empty());
    let Origin::Plugin { lossy, .. } = parsed.topic.origin else {
        panic!("a plugin topic");
    };
    assert!(lossy, "the model carries it too, not just the parse result");
}

#[test]
fn a_source_over_the_byte_cap_is_truncated() {
    let mut src = HEAD.as_bytes().to_vec();
    src.extend(std::iter::repeat_n(b'a', Limits::untrusted().max_bytes * 2));
    let parsed = parse_untrusted(&src, "p", None);
    assert!(parsed.truncated);
    let Origin::Plugin { truncated, .. } = parsed.topic.origin else {
        panic!("a plugin topic");
    };
    assert!(truncated);
}

#[test]
fn a_bidi_override_never_reaches_the_model_raw() {
    // U+202E (RIGHT-TO-LEFT OVERRIDE) is the classic of name spoofing; in
    // plugin prose it would do the same to the rest of the screen.
    let src = format!("{HEAD}hello \u{202E}dlrow\n");
    let parsed = parse_untrusted(src.as_bytes(), "p", None);
    let text = paragraph_text(&parsed);
    assert!(
        !text.contains('\u{202E}'),
        "a raw bidi override reached the render: {text:?}"
    );
    assert!(text.contains('\u{FFFD}'), "{text:?}");
}

#[test]
fn a_plugin_command_mark_survives_as_a_reference() {
    // A plugin documenting its OWN command is the whole point of the hostile
    // mode: the mark must survive masking, unresolved.
    let src = format!("{HEAD}run {{{{cmd:acme:sync}}}}\n");
    let parsed = parse_untrusted(src.as_bytes(), "acme", None);
    let has_ref = parsed.topic.blocks.iter().any(|b| match b {
        Block::Paragraph(spans) => spans
            .iter()
            .any(|s| matches!(s, Span::CommandRef(c) if c == "acme:sync")),
        _ => false,
    });
    assert!(
        has_ref,
        "the plugin documents ITS commands: {:?}",
        parsed.topic.blocks
    );
}

#[test]
fn tags_see_also_and_context_from_a_plugin_header_are_dropped() {
    // The asymmetry is deliberate and it is a security boundary, not an
    // oversight: a plugin does not get to file itself under the host's index
    // groups (`tags`), link INTO host topics (`see_also`) or claim a UI
    // context so that F1 opens it instead of the host's own page
    // (`context`). Only the commands it documents survive.
    let src = "+++\n\
id = \"mine\"\n\
title = \"Mine\"\n\
tags = [\"doing\"]\n\
see_also = [\"copying\"]\n\
commands = [\"acme:sync\"]\n\
context = [\"pane\"]\n\
+++\n\
body\n";
    let parsed = parse_untrusted(src.as_bytes(), "acme", None);
    let t = &parsed.topic;
    assert!(t.tags.is_empty(), "tags: {:?}", t.tags);
    assert!(t.see_also.is_empty(), "see_also: {:?}", t.see_also);
    assert!(t.context.is_empty(), "context: {:?}", t.context);
    assert_eq!(
        t.commands,
        vec!["acme:sync".to_owned()],
        "its own commands are the one header list that survives"
    );
    assert_eq!(t.id.as_str(), "mine", "the header id is honoured");
    assert_eq!(t.title, "Mine");
}

// --- Hardening.

#[test]
fn the_canonical_hostile_corpus_as_a_plugin_body_is_neither_a_panic_nor_a_hazard() {
    // Every hostile name of the canonical corpus, used as a plugin `help.md`
    // body under each block prefix. Three claims: it does not panic, no raw
    // hazard reaches ANY string of the model, and no command reference is
    // fabricated — a `CommandRef` is read downstream as a claim that the
    // command exists.
    let names = norte_testkit::corpus::hostile_names();
    assert!(names.len() >= 32, "the canonical corpus must not shrink");
    for name in &names {
        let text = String::from_utf8_lossy(&name.bytes).into_owned();
        for prefix in ["", "# ", "> ", "- ", "|", "```", "**"] {
            let body = format!("{prefix}{text}\n");
            let src = [HEAD.as_bytes(), body.as_bytes()].concat();
            let parsed = parse_untrusted(&src, "acme.plugin", None);
            assert_no_hazard(&parsed.topic, &format!("{} + {prefix:?}", name.id));
            let refs = parsed
                .topic
                .blocks
                .iter()
                .flat_map(collect_spans)
                .filter(|s| matches!(s, Span::CommandRef(_)))
                .count();
            assert!(
                refs <= text.matches("{{cmd:").count(),
                "[{}] {prefix:?}: {refs} references out of {} openers — one was fabricated",
                name.id,
                text.matches("{{cmd:").count()
            );
        }
    }
}

#[test]
fn the_non_utf8_content_fixtures_set_lossy_exactly_when_they_are_not_utf8() {
    // The `\xff\xfe` fixtures (a UTF-16LE BOM read as if it were markdown)
    // and the rest of the canonical content corpus, fed in as a plugin
    // `help.md`. `lossy` is not a heuristic: it is exactly "these bytes were
    // not UTF-8".
    let fixtures = norte_testkit::corpus::content_fixtures();
    assert!(fixtures.len() >= 11, "the canonical corpus must not shrink");
    for f in &fixtures {
        let src = [HEAD.as_bytes(), &f.bytes].concat();
        let parsed = parse_untrusted(&src, "acme.plugin", None);
        assert_eq!(
            parsed.lossy,
            std::str::from_utf8(&f.bytes).is_err(),
            "[{}] the flag must mean exactly what it says",
            f.id
        );
        assert_no_hazard(&parsed.topic, f.id);
    }
}

#[test]
fn a_valid_header_over_a_ten_mebibyte_line_is_bounded() {
    // The shape a hostile plugin reaches for: a header that parses cleanly,
    // so nothing is rejected up front, over a body that is one enormous
    // line. The claim is on the FLAGS and the block count, never on a
    // stopwatch — a wall-clock assertion measures the machine and flakes
    // under coverage instrumentation.
    let mut src = HEAD.as_bytes().to_vec();
    src.extend(std::iter::repeat_n(b'a', 10 * 1024 * 1024));
    let parsed = parse_untrusted(&src, "p", None);
    assert!(parsed.truncated, "10 MiB cannot come back whole");
    assert!(!parsed.lossy, "ASCII is UTF-8");
    assert_eq!(parsed.topic.id.as_str(), "x", "the header survived the cut");
    assert_eq!(
        parsed.topic.blocks.len(),
        1,
        "one line is one paragraph, whatever its length"
    );
    let Some(Block::Paragraph(spans)) = parsed.topic.blocks.first() else {
        panic!("a paragraph: {:?}", parsed.topic.blocks);
    };
    let len: usize = spans.iter().map(|s| payload(s).len()).sum();
    assert!(
        len <= Limits::untrusted().max_line_bytes,
        "the line ceiling bounds what reaches the model: {len}"
    );
}

#[test]
fn a_header_title_at_the_cap_survives_and_one_char_over_is_cut() {
    let at = "a".repeat(HEADER_CHARS);
    let parsed = parse_untrusted(title_src(&at).as_bytes(), "p", None);
    assert_eq!(parsed.topic.title, at, "exactly at the cap is not a cut");
    assert!(!parsed.truncated);

    let over = "a".repeat(HEADER_CHARS + 1);
    let parsed = parse_untrusted(title_src(&over).as_bytes(), "p", None);
    assert_eq!(parsed.topic.title.chars().count(), HEADER_CHARS);
    assert!(parsed.truncated, "a cut title must raise the badge");

    // CHARACTERS, not bytes: a non-ASCII language must not pay the cap early.
    // 280 `ñ` is 560 bytes and must come back whole.
    let multibyte = "ñ".repeat(HEADER_CHARS);
    let parsed = parse_untrusted(title_src(&multibyte).as_bytes(), "p", None);
    assert_eq!(parsed.topic.title, multibyte);
    assert!(!parsed.truncated);

    // And the cut never splits a character in half.
    let multibyte = "日".repeat(HEADER_CHARS + 10);
    let parsed = parse_untrusted(title_src(&multibyte).as_bytes(), "p", None);
    assert_eq!(parsed.topic.title, "日".repeat(HEADER_CHARS));
    assert!(parsed.truncated);
}

#[test]
fn the_publisher_is_masked_and_bounded_like_every_other_third_party_string() {
    // `publisher` comes from the plugin manifest, which caps `description`
    // and the command ids but NOT this field: it arrives unbounded and
    // unmasked, and it lands in the model.
    let publisher = format!("Acme \u{202E}Corp{}", "!".repeat(HEADER_CHARS));
    let parsed = parse_untrusted(b"body", "p", Some(publisher));
    let Origin::Plugin {
        publisher,
        truncated,
        ..
    } = parsed.topic.origin
    else {
        panic!("a plugin topic");
    };
    let publisher = publisher.expect("declared");
    assert!(!publisher.contains('\u{202E}'), "{publisher:?}");
    assert_eq!(publisher.chars().count(), HEADER_CHARS);
    assert!(truncated, "a cut publisher is still a cut");
}

#[test]
fn the_wire_round_trip_leaves_no_hazard_anywhere_in_the_model() {
    // The shape phase H3e will push over the wire: bytes in, `Parsed` out.
    // The sweep is UNIVERSAL — every string of the model, not a spot check —
    // because the next block kind someone adds is exactly the one that will
    // be forgotten.
    let mut src = Vec::new();
    // U+200B (ZERO WIDTH SPACE) and not a C0 control: TOML rejects a raw
    // control inside a basic string, so a header carrying one degrades to NO
    // header (pinned in `a_control_char_in_the_header_degrades_to_no_header`)
    // and would never exercise the header masking this test is about.
    src.extend_from_slice(
        "+++\nid = \"i\u{202E}d\"\ntitle = \"T\u{200B}itle\"\n\
commands = [\"acme:s\u{202E}ync\"]\n+++\n"
            .as_bytes(),
    );
    for name in norte_testkit::corpus::hostile_names() {
        for prefix in ["# ", "> \u{26A0} ", "- ", "|", ""] {
            src.extend_from_slice(prefix.as_bytes());
            src.extend_from_slice(&name.bytes);
            src.push(b'\n');
        }
    }
    src.extend_from_slice(b"```s\xffh\n\xfe code \x1b]0;x\x07\n```\n");
    src.extend_from_slice("| a\u{202E} | b |\n|---|---|\n| c\u{0000} | d |\n".as_bytes());

    let parsed = parse_untrusted(&src, "acme.plugin", Some("Acme \u{202E}Corp".to_owned()));
    assert!(parsed.lossy, "the source carries lone bytes");
    assert_no_hazard(&parsed.topic, "round trip");
    assert!(
        !parsed.topic.blocks.is_empty(),
        "bounding is not the same as discarding"
    );
    // The spoofed header strings reached the model MASKED, so neither can
    // pass for the clean thing it imitates.
    assert_eq!(parsed.topic.id.as_str(), "i\u{FFFD}d");
    assert_eq!(parsed.topic.title, "T\u{FFFD}itle");
    assert_eq!(parsed.topic.commands, vec!["acme:s\u{FFFD}ync".to_owned()]);
}

#[test]
fn a_blank_header_id_or_title_falls_back_to_the_plugin_id() {
    // Both are one keystroke away for a hostile author: `id = ""` leaves an
    // unaddressable topic and an empty title renders a nameless page. Blank
    // is the parser's own notion of it — whitespace or `U+FFFD` — so a field
    // that was nothing but hazards counts as blank once masked.
    for (id, title) in [
        ("", ""),
        ("   ", "\t"),
        ("\u{202E}", "\u{202E}\u{202E}"),
        ("\u{FFFD}", " \u{FFFD} "),
    ] {
        let src = format!("+++\nid = \"{id}\"\ntitle = \"{title}\"\n+++\nbody\n");
        let parsed = parse_untrusted(src.as_bytes(), "acme.plugin", None);
        assert_eq!(parsed.topic.id.as_str(), "acme.plugin", "id {id:?}");
        assert_eq!(parsed.topic.title, "acme.plugin", "title {title:?}");
    }
    // A non-blank field is still honoured, hazards and all — masked, never
    // dropped: losing a real title over one bad character would be worse.
    let src = "+++\nid = \"a\u{202E}b\"\ntitle = \"c\u{202E}d\"\n+++\nbody\n";
    let parsed = parse_untrusted(src.as_bytes(), "acme.plugin", None);
    assert_eq!(parsed.topic.id.as_str(), "a\u{FFFD}b");
    assert_eq!(parsed.topic.title, "c\u{FFFD}d");
}

#[test]
fn a_control_char_in_the_header_degrades_to_no_header() {
    // TOML rejects a raw control character inside a basic string, so this
    // header does not parse. ANY header failure degrades to "the whole text
    // is the body" — losing the document over it would be a denial of
    // service dressed up as strictness — and the fallback id takes over.
    let src = "+++\nid = \"x\"\ntitle = \"T\u{0007}itle\"\n+++\nbody\n";
    let parsed = parse_untrusted(src.as_bytes(), "acme.plugin", None);
    assert_eq!(parsed.topic.id.as_str(), "acme.plugin");
    assert_eq!(parsed.topic.title, "acme.plugin");
    assert_no_hazard(&parsed.topic, "control in header");
    assert!(
        !parsed.topic.blocks.is_empty(),
        "the header text becomes body, masked: {:?}",
        parsed.topic.blocks
    );
}

// --- Helpers.

/// A source whose only interesting part is the header `title`.
fn title_src(title: &str) -> String {
    format!("+++\nid = \"x\"\ntitle = \"{title}\"\n+++\nbody\n")
}

/// Every paragraph's text, concatenated.
fn paragraph_text(parsed: &Parsed) -> String {
    parsed
        .topic
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Paragraph(spans) => Some(spans.iter().map(payload).collect::<String>()),
            _ => None,
        })
        .collect()
}

/// The bytes a span carries: no syntax, just what a renderer would paint (or,
/// for the live marks, the id it would resolve).
fn payload(span: &Span) -> String {
    match span {
        Span::Text(t) | Span::Strong(t) | Span::Emph(t) | Span::Code(t) | Span::CommandRef(t) => {
            t.clone()
        }
        Span::TopicLink(id) => id.as_str().to_owned(),
    }
}

/// Every span a block carries, in order. Exhaustive on purpose: a new
/// [`Block`] variant carrying spans must be added here or the sweep would
/// quietly stop covering it.
fn collect_spans(block: &Block) -> Vec<Span> {
    match block {
        Block::Paragraph(spans) | Block::Callout { spans, .. } => spans.clone(),
        Block::Bullets(items) => items.concat(),
        Block::Heading { .. } | Block::Code { .. } | Block::Table { .. } => Vec::new(),
    }
}

/// Every string the topic carries, labelled by where it lives.
///
/// Exhaustive `match`es throughout: this is the sweep that makes the
/// "already masked" claim of `Origin::Plugin` checkable, and a variant that
/// escapes it escapes the claim.
fn strings(topic: &Topic) -> Vec<(&'static str, String)> {
    let mut out = vec![
        ("id", topic.id.as_str().to_owned()),
        ("title", topic.title.clone()),
    ];
    out.extend(topic.tags.iter().map(|t| ("tag", t.clone())));
    out.extend(topic.see_also.iter().map(|t| ("see_also", t.to_string())));
    out.extend(topic.commands.iter().map(|c| ("command", c.clone())));
    out.extend(topic.context.iter().map(|c| ("context", c.clone())));
    match &topic.origin {
        Origin::BuiltIn => panic!("a plugin topic is never built in"),
        Origin::Plugin { id, publisher, .. } => {
            out.push(("origin.id", id.clone()));
            out.extend(publisher.iter().map(|p| ("origin.publisher", p.clone())));
        }
    }
    for block in &topic.blocks {
        match block {
            Block::Heading { level, text } => {
                assert!((1..=3).contains(level), "level {level} is outside 1..=3");
                out.push(("heading", text.clone()));
            }
            Block::Code { lang, text } => {
                out.extend(lang.iter().map(|l| ("code.lang", l.clone())));
                // The `\n` between lines is the PARSER's structure, not the
                // plugin's bytes: masking it would collapse the block into
                // one unreadable line. Every other line is swept.
                out.extend(text.lines().map(|l| ("code.text", l.to_owned())));
            }
            Block::Table { header, rows } => {
                assert!(
                    rows.iter().all(|r| r.len() == header.len()),
                    "a ragged row would panic a renderer indexing by column"
                );
                out.extend(header.iter().map(|c| ("table.header", c.clone())));
                out.extend(rows.iter().flatten().map(|c| ("table.cell", c.clone())));
            }
            Block::Callout { kind, .. } => {
                assert!(matches!(kind, Callout::Note | Callout::Warn | Callout::Tip));
            }
            Block::Paragraph(_) | Block::Bullets(_) => {}
        }
        out.extend(collect_spans(block).iter().map(|s| ("span", payload(s))));
    }
    out
}

/// No string of the model carries a terminal hazard, and every one of them
/// is valid UTF-8.
///
/// The UTF-8 half is structural — a `String` cannot be anything else — so it
/// is asserted through `from_utf8` only to pin that the model keeps using
/// types that guarantee it. The load-bearing half is the hazard sweep.
fn assert_no_hazard(topic: &Topic, ctx: &str) {
    for (label, s) in strings(topic) {
        assert!(
            std::str::from_utf8(s.as_bytes()).is_ok(),
            "[{ctx}] {label}: not UTF-8"
        );
        if let Some(c) = s.chars().find(|c| norte_encoding::is_terminal_hazard(*c)) {
            panic!("[{ctx}] {label}: raw hazard U+{:04X} in {s:?}", c as u32);
        }
    }
}
