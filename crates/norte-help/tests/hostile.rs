//! Hostile mode: a plugin `help.md` is THIRD-PARTY text. It never fails, it
//! always bounds, it always masks. The hostile strings come from the
//! canonical `norte-testkit` corpus (house rule: fixtures are code).
//!
//! Everything asserted here is a claim `Origin::Plugin` makes in its rustdoc
//! — "third-party text, already masked and bounded by the parser". A claim
//! nobody tests is a comment.
//!
//! The exception the model documents is exercised too: an identity is never
//! masked. The plugin id is host-assigned and kept byte-exact, and a command
//! id is a dispatch key the parser accepts or REFUSES but never rewrites, so
//! the hazard sweep still covers both.

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
fn a_byte_the_detected_encoding_cannot_decode_raises_the_lossy_flag() {
    // A UTF-8 BOM is CERTAINTY, not statistics: the encoding is settled, so
    // the invalid byte that follows really is a decode failure and the badge
    // must say so.
    let mut src = b"\xef\xbb\xbf".to_vec();
    src.extend_from_slice(HEAD.as_bytes());
    src.extend_from_slice(b"body \xff\n");
    let parsed = parse_untrusted(&src, "p", None);
    assert!(parsed.lossy, "the flag feeds the UI badge");
    assert!(!parsed.topic.blocks.is_empty());
    let Origin::Plugin { lossy, .. } = parsed.topic.origin else {
        panic!("a plugin topic");
    };
    assert!(lossy, "the model carries it too, not just the parse result");
}

#[test]
fn a_utf8_bom_does_not_destroy_the_front_matter() {
    // `EF BB BF` is what a Windows editor writes by default. Before the
    // decode went through `norte_encoding`, those three bytes made the `+++`
    // prefix fail: the header was lost, the raw TOML rendered as prose, and
    // BOTH flags stayed false — a total loss with no badge.
    let mut src = b"\xef\xbb\xbf".to_vec();
    src.extend_from_slice("+++\nid = \"x\"\ntitle = \"Real title\"\n+++\nbody\n".as_bytes());
    let parsed = parse_untrusted(&src, "acme.plugin", None);
    assert_eq!(
        parsed.topic.title, "Real title",
        "the BOM is a byte-order mark, not content"
    );
    assert_eq!(
        parsed.topic.blocks,
        vec![Block::Paragraph(vec![Span::Text("body".to_owned())])],
        "the TOML must not leak into the prose"
    );
    assert!(!parsed.truncated && !parsed.lossy);
}

#[test]
fn every_content_fixture_decodes_to_its_declared_text() {
    // The canonical content corpus, each one used as a plugin `help.md`.
    // Asserting the boolean alone is what let mojibake through: a UTF-16LE
    // file read as UTF-8 sets no flag and renders as garbage, so the
    // assertion has to be against the DECODED TEXT.
    let fixtures = norte_testkit::corpus::content_fixtures();
    assert!(fixtures.len() >= 11, "the canonical corpus must not shrink");
    for f in &fixtures {
        let parsed = parse_untrusted(&f.bytes, "acme.plugin", None);
        let text = paragraph_text(&parsed);
        // The expected value is the decoded text with the SAME masking the
        // parser applies — one fixture is a raw control injection, and
        // masking it is the other half of this crate's job. Anything else
        // that differs is a decode bug.
        let expected = norte_encoding::mask_terminal_hazards(f.decoded.trim_end_matches('\n'));
        assert_eq!(text, expected, "[{}] decoded as {:?}", f.id, text);
        assert!(!parsed.lossy, "[{}] every fixture decodes cleanly", f.id);
        assert_no_hazard(&parsed.topic, f.id);
    }
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
    // mode. The id is the DISPATCH KEY the palette already uses,
    // `plugin:{plugin_id}:{command_id}` — the namespace is what tells a
    // plugin's command apart from the host's.
    let src = format!("{HEAD}run {{{{cmd:plugin:acme:sync}}}}\n");
    let parsed = parse_untrusted(src.as_bytes(), "acme", None);
    assert_eq!(
        command_refs(&parsed),
        vec!["plugin:acme:sync".to_owned()],
        "the plugin documents ITS commands: {:?}",
        parsed.topic.blocks
    );
}

#[test]
fn a_plugin_cannot_fabricate_a_reference_to_a_host_command() {
    // The full payload from the audit. Warning chrome the host owns, the
    // host's REAL effective chord resolved at render time, and an executable
    // row — added after approval, since help is outside the approval digest.
    // The mark must degrade to literal text exactly as a malformed one does.
    let src = "+++\nid = \"x\"\ntitle = \"X\"\ncommands = [\"fs.delete\"]\n+++\n\
> \u{26A0} **Action required.** Press {{cmd:fs.delete}} to apply the update.\n";
    let parsed = parse_untrusted(src.as_bytes(), "acme", None);
    assert!(
        command_refs(&parsed).is_empty(),
        "a host command was fabricated: {:?}",
        parsed.topic.blocks
    );
    assert!(
        parsed.topic.commands.is_empty(),
        "the header list is the same door: {:?}",
        parsed.topic.commands
    );
    // Degraded, not deleted: the reader still sees the sentence, and the
    // literal mark in it is the evidence that something claimed to be a
    // command and was refused.
    let text = block_text(&parsed);
    assert!(text.contains("{{cmd:fs.delete}}"), "{text:?}");

    // Every near miss is refused too: a sibling plugin, a prefix that only
    // looks like ours, and the bare `{plugin}:{cmd}` form that is NOT the
    // dispatch key.
    for id in [
        "plugin:other:sync",
        "plugin:acmex:sync",
        "plugin:acme",
        "plugin:acme:",
        "acme:sync",
        "fs.copy",
        "plugin::sync",
    ] {
        let src = format!("{HEAD}run {{{{cmd:{id}}}}}\n");
        let parsed = parse_untrusted(src.as_bytes(), "acme", None);
        assert!(
            command_refs(&parsed).is_empty(),
            "{id:?} passed the namespace check"
        );
    }
}

#[test]
fn a_plugin_cannot_claim_a_built_in_topic_id() {
    // `id = "copying"` would shadow the host topic once the corpus and the
    // registry share one id space, and every built-in `[[copying]]` link
    // would jump into plugin prose. The topic id is HOST-ASSIGNED: whatever
    // the header says, the id is the one the caller passed in.
    let src = "+++\nid = \"copying\"\ntitle = \"Copying\"\n+++\nbody\n";
    let parsed = parse_untrusted(src.as_bytes(), "acme.plugin", None);
    assert_eq!(
        parsed.topic.id.as_str(),
        "acme.plugin",
        "the header id is IGNORED, not honoured"
    );
    assert_eq!(
        parsed.topic.title, "Copying",
        "the title is the plugin's to choose: it names a page, it does not address one"
    );
}

#[test]
fn a_topic_link_in_plugin_prose_degrades_to_literal_text() {
    // `see_also` is dropped so a plugin cannot link into host topics; a
    // `[[…]]` in the body is the same jump through the other door. The body
    // must agree with the header.
    let src = format!("{HEAD}see [[copying]] for more\n");
    let parsed = parse_untrusted(src.as_bytes(), "acme", None);
    let links: Vec<Span> = parsed
        .topic
        .blocks
        .iter()
        .flat_map(collect_spans)
        .filter(|s| matches!(s, Span::TopicLink(_)))
        .collect();
    assert!(links.is_empty(), "a link into the host corpus: {links:?}");
    assert!(block_text(&parsed).contains("[[copying]]"));
}

#[test]
fn a_live_mark_cannot_straddle_a_line_break() {
    // The paragraph joiner used to weld a mark out of two lines: neither
    // source line holds a complete `{{cmd:…}}`, yet the joined text did. It
    // is the mirror of the code-block bug — there, mask per line then join;
    // here, parse per line then join — and it also defeats any line-by-line
    // host-side validator.
    let src = format!("{HEAD}{{{{cmd:\nplugin:acme:sync}}}}\n");
    let parsed = parse_untrusted(src.as_bytes(), "acme", None);
    assert!(
        command_refs(&parsed).is_empty(),
        "a mark was welded across a line break: {:?}",
        parsed.topic.blocks
    );
    let text = block_text(&parsed);
    assert!(text.contains("{{cmd: plugin:acme:sync}}"), "{text:?}");
    // The same for a topic link, and in trusted-shaped prose the reflow is
    // unchanged: two lines still join into ONE text span with one space.
    let src = format!("{HEAD}one\ntwo\n");
    let parsed = parse_untrusted(src.as_bytes(), "acme", None);
    assert_eq!(
        parsed.topic.blocks,
        vec![Block::Paragraph(vec![Span::Text("one two".to_owned())])]
    );
}

#[test]
fn blank_command_ids_in_the_header_are_dropped() {
    // The body refuses a blank id (`{{cmd:}}` stays literal text); the
    // header used to accept one and hand the UI an executable row with no
    // name. Same predicate, both doors.
    let src = "+++\nid = \"x\"\ntitle = \"X\"\n\
commands = [\"\", \"   \", \"\u{202E}\", \"plugin:acme:\", \"plugin:acme:  \"]\n+++\nbody\n";
    let parsed = parse_untrusted(src.as_bytes(), "acme", None);
    assert!(
        parsed.topic.commands.is_empty(),
        "{:?}",
        parsed.topic.commands
    );
}

#[test]
fn a_header_cannot_declare_an_unbounded_number_of_commands() {
    // The front matter used to sit entirely outside `Limits`: a 64 KiB
    // header-only file turned into tens of thousands of command entries,
    // every one of them an executable row on the same dispatch path as the
    // palette, and `truncated` stayed false.
    let mut header = "+++\nid = \"x\"\ntitle = \"X\"\ncommands = [".to_owned();
    while header.len() < Limits::untrusted().max_bytes - 16 {
        header.push_str("\"plugin:acme:a\",");
    }
    header.push_str("]\n+++\n");
    let parsed = parse_untrusted(header.as_bytes(), "acme", None);
    assert!(
        parsed.topic.commands.len() <= 16,
        "{} command rows out of one 64 KiB header",
        parsed.topic.commands.len()
    );
    assert!(parsed.truncated, "a dropped command is content lost");
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
commands = [\"plugin:acme:sync\"]\n\
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
        vec!["plugin:acme:sync".to_owned()],
        "its own commands are the one header list that survives"
    );
    assert_eq!(
        t.id.as_str(),
        "acme",
        "the id is host-assigned, never the header's"
    );
    assert_eq!(t.title, "Mine", "the title IS the plugin's to choose");
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
fn a_source_of_pure_continuation_bytes_still_yields_its_content() {
    // 64 KiB + 1 of `0x80`: no lead byte anywhere, so the boundary walk used
    // to march the cut all the way down to ZERO and hand back an empty
    // document — `truncated = true`, `lossy = false`, no blocks at all. The
    // walk is bounded now: a UTF-8 character is at most four bytes, so
    // failing to find a boundary within three means these are not UTF-8
    // bytes and the cut stands where it was.
    let src = vec![0x80_u8; Limits::untrusted().max_bytes + 1];
    let parsed = parse_untrusted(&src, "acme.plugin", None);
    assert!(parsed.truncated, "a byte was dropped: say so");
    assert!(
        !parsed.topic.blocks.is_empty(),
        "the whole document vanished"
    );
    assert_no_hazard(&parsed.topic, "continuation bytes");
}

#[test]
fn a_cut_cannot_hide_a_decode_failure_in_the_tail() {
    // `lossy` used to be computed on the CUT PREFIX, so a file whose invalid
    // bytes all sat past the ceiling reported clean. The prefix here is
    // spotless ASCII and the tail is not, and the badge must still fire.
    let mut src = b"\xef\xbb\xbf".to_vec();
    src.extend_from_slice(HEAD.as_bytes());
    src.extend(std::iter::repeat_n(b'a', Limits::untrusted().max_bytes));
    src.extend_from_slice(b"\xff\xfe");
    let parsed = parse_untrusted(&src, "acme.plugin", None);
    assert!(parsed.truncated);
    assert!(
        parsed.lossy,
        "the tail we refused to read was not clean either"
    );
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
    assert_eq!(
        parsed.topic.title, "X",
        "the header survived the cut (the id is host-assigned, so it proves nothing)"
    );
    assert_eq!(parsed.topic.id.as_str(), "p");
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
fn masking_can_grow_a_line_threefold_and_no_further() {
    // `max_line_bytes` bounds the SOURCE line. Masking runs afterwards and
    // replaces a 1-byte control with a 3-byte `U+FFFD`, so the model line
    // can reach 3x the ceiling — and not one byte more, since 3 bytes is the
    // most a single character can grow to. The old assertion used ASCII
    // only, which is exactly why it never saw this.
    let limits = Limits::untrusted();
    let src = format!("{HEAD}{}\n", "\u{0007}".repeat(limits.max_line_bytes * 2));
    let parsed = parse_untrusted(src.as_bytes(), "p", None);
    let Some(Block::Paragraph(spans)) = parsed.topic.blocks.first() else {
        panic!("a paragraph: {:?}", parsed.topic.blocks);
    };
    let len: usize = spans.iter().map(|s| payload(s).len()).sum();
    assert_eq!(
        len,
        limits.max_line_bytes * 3,
        "one U+FFFD per source byte, three bytes each"
    );
    assert!(parsed.truncated, "the source line WAS cut");
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
fn the_publisher_is_masked_and_bounded_without_badging_the_document() {
    // `publisher` comes from the plugin manifest, which caps `description`
    // and the command ids but NOT this field: it arrives unbounded and
    // unmasked, and it lands in the model.
    let publisher = format!("Acme \u{202E}Corp{}", "!".repeat(HEADER_CHARS));
    let parsed = parse_untrusted(b"body", "p", Some(publisher));
    assert!(
        !parsed.truncated,
        "the badge speaks about the plugin's DOCUMENT: 4 bytes of body, \
         byte-complete. Capping a host-supplied metadata field is not the \
         document being cut, and conflating them cries wolf."
    );
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
    assert!(!truncated);
}

#[test]
fn fold_flags_carries_a_badge_across_the_wire_hop() {
    // Phase H3e parses host-side, sends the ALREADY BOUNDED markdown, and
    // the frontend parses it again. Both flags are structurally false on the
    // second parse — the text arrives short and clean — so the badge, the
    // whole user-facing mitigation for hostile help, would silently go
    // clean. `fold_flags` is the seam that carries it.
    let parsed = parse_untrusted(b"body", "acme.plugin", None);
    assert!(!parsed.truncated && !parsed.lossy);

    let folded = parsed.fold_flags(true, true);
    assert!(folded.truncated && folded.lossy);
    let Origin::Plugin {
        truncated, lossy, ..
    } = folded.topic.origin
    else {
        panic!("a plugin topic");
    };
    assert!(
        truncated && lossy,
        "the copy inside the model is what a renderer reads"
    );

    // It only ever ORs: a flag already raised cannot be cleared by a caller
    // that received a clean one.
    let mut src = HEAD.as_bytes().to_vec();
    src.extend(std::iter::repeat_n(b'a', Limits::untrusted().max_bytes * 2));
    let parsed = parse_untrusted(&src, "p", None).fold_flags(false, false);
    assert!(parsed.truncated);
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
    // A fence whose `lang` carries a REAL hazard. It used to carry a lone
    // `\xff`, which lossy-decodes to `U+FFFD` — not a hazard — so the `lang`
    // masking was never actually exercised by this test.
    src.extend_from_slice("```s\u{202E}h\n code \u{1B}]0;x\u{07}\n```\n".as_bytes());
    src.extend_from_slice(b"\xfe\n");
    src.extend_from_slice("| a\u{202E} | b |\n|---|---|\n| c\u{0000} | d |\n".as_bytes());

    let parsed = parse_untrusted(&src, "acme.plugin", Some("Acme \u{202E}Corp".to_owned()));
    assert!(parsed.lossy, "the source carries lone bytes");
    assert_no_hazard(&parsed.topic, "round trip");
    assert!(
        !parsed.topic.blocks.is_empty(),
        "bounding is not the same as discarding"
    );
    assert!(
        parsed
            .topic
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Code { lang: Some(l), .. } if l.contains('\u{FFFD}'))),
        "the fence label is third-party text too: {:?}",
        parsed.topic.blocks
    );
    // The id is HOST-ASSIGNED and kept verbatim; the spoofed title reached
    // the model masked, so it cannot pass for the clean thing it imitates;
    // and the un-namespaced command was refused outright.
    assert_eq!(parsed.topic.id.as_str(), "acme.plugin");
    assert_eq!(parsed.topic.title, "T\u{FFFD}itle");
    assert!(parsed.topic.commands.is_empty());
}

#[test]
fn a_hostile_fallback_id_is_kept_verbatim_as_the_identity() {
    // `fallback_id` is supplied by the HOST registry, not by the plugin, and
    // both `Topic.id` and `Origin::Plugin.id` are LOOKUP KEYS. Masking them
    // destroyed identity: `acme\u{202E}ftp`, `acme\u{200B}ftp` and
    // `acme\u{07}ftp` all collapsed onto the same masked string, so a
    // registry lookup would match the WRONG plugin. Verbatim, always — the
    // caller is required to pass an id it has validated.
    let ids = ["acme\u{202E}ftp", "acme\u{200B}ftp", "acme\u{07}ftp"];
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        let parsed = parse_untrusted(b"body", id, None);
        assert_eq!(parsed.topic.id.as_str(), id, "the key was mutated");
        let Origin::Plugin { id: origin_id, .. } = &parsed.topic.origin else {
            panic!("a plugin topic");
        };
        assert_eq!(origin_id, id, "both copies of the key, byte for byte");
        assert!(seen.insert(origin_id.clone()), "two ids collapsed into one");
        // A key is never a cut, whatever its length.
        let long = "a".repeat(HEADER_CHARS * 4);
        let parsed = parse_untrusted(b"body", &long, None);
        assert_eq!(parsed.topic.id.as_str(), long);
        assert!(!parsed.truncated);
    }
    assert_eq!(seen.len(), ids.len());
}

#[test]
fn a_blank_header_title_falls_back_to_the_plugin_id() {
    // An empty title renders a nameless page, and it is one keystroke away
    // for a hostile author. Blank is the parser's own notion of it:
    // whitespace, `U+FFFD`, or a character that paints NOTHING — U+3164
    // HANGUL FILLER and friends are not in the hazard set (that set is a
    // cross-crate contract this crate does not widen), so blankness is where
    // they are handled.
    for title in [
        "",
        "   ",
        "\\t",
        "\u{202E}\u{202E}",
        " \u{FFFD} ",
        "\u{3164}\u{3164}\u{3164}",
        "\u{2064}\u{115F}\u{180E}\u{2800}",
    ] {
        let src = format!("+++\nid = \"x\"\ntitle = \"{title}\"\n+++\nbody\n");
        let parsed = parse_untrusted(src.as_bytes(), "acme.plugin", None);
        assert_eq!(parsed.topic.title, "acme.plugin", "title {title:?}");
    }
    // A non-blank title is still honoured, hazards and all — masked, never
    // dropped: losing a real title over one bad character would be worse.
    let src = "+++\nid = \"x\"\ntitle = \"c\u{202E}d\"\n+++\nbody\n";
    let parsed = parse_untrusted(src.as_bytes(), "acme.plugin", None);
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

/// The text of EVERY block, concatenated: what a reader would end up seeing.
fn block_text(parsed: &Parsed) -> String {
    parsed
        .topic
        .blocks
        .iter()
        .flat_map(|b| collect_spans(b).iter().map(payload).collect::<Vec<_>>())
        .collect()
}

/// Every command reference the body produced, in order.
fn command_refs(parsed: &Parsed) -> Vec<String> {
    parsed
        .topic
        .blocks
        .iter()
        .flat_map(collect_spans)
        .filter_map(|s| match s {
            Span::CommandRef(c) => Some(c),
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
    // A command id is a DISPATCH KEY, never masked — masking a key destroys
    // the identity it exists to carry. It is swept all the same, because the
    // parser REFUSES a key it could not paint safely instead of rewriting
    // it, which is what keeps this assertion true.
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
                // one unreadable line. Every other byte is swept — split on
                // `\n` and NOT `lines()`, which also strips a trailing `\r`
                // and would let a raw CR walk straight through the sweep.
                out.extend(text.split('\n').map(|l| ("code.text", l.to_owned())));
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
