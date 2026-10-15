//! What each production of the grammar parses to, and where an error points.

use norte_orders::*;

fn q(line: &str) -> Order {
    parse(line).unwrap_or_else(|e| panic!("{line:?}: {e}"))
}

fn sel(terms: Vec<Term>) -> Stage {
    Stage::Sel(Query { terms })
}

fn yes(pred: Pred) -> Term {
    Term {
        negated: false,
        pred,
    }
}

#[test]
fn the_flagship_line() {
    assert_eq!(
        q("sel ext:pdf age<30d | cp >other"),
        Order {
            stages: vec![
                sel(vec![
                    yes(Pred::Ext(vec![b"pdf".to_vec()])),
                    yes(Pred::Age(Cmp::Lt, 30 * 86_400)),
                ]),
                Stage::Cp(Dest::Other),
            ]
        }
    );
}

#[test]
fn units_negation_bare_glob_and_kinds() {
    let o = q("sel size>=1.5M !name:*sample* *.mkv type:file");
    let Stage::Sel(Query { terms }) = &o.stages[0] else {
        panic!("{o:?}")
    };
    assert_eq!(terms[0].pred, Pred::Size(Cmp::Ge, 1_572_864));
    assert!(terms[1].negated);
    assert_eq!(terms[1].pred, Pred::Name(b"*sample*".to_vec()));
    assert_eq!(terms[2].pred, Pred::Name(b"*.mkv".to_vec()));
    assert_eq!(terms[3].pred, Pred::Kind(KindSel::File));
}

#[test]
fn m_is_minutes_not_months() {
    assert_eq!(
        q("sel age<90m").stages[0],
        sel(vec![yes(Pred::Age(Cmp::Lt, 5400))])
    );
}

#[test]
fn a_pipe_needs_no_spaces() {
    assert_eq!(q("sel *.rs|trash").stages.len(), 2);
}

#[test]
fn a_lone_angle_takes_the_next_word() {
    assert_eq!(q("cp > here").stages, vec![Stage::Cp(Dest::Here)]);
}

#[test]
fn escapes_reach_the_bytes() {
    assert_eq!(
        q(r"cp >/mnt/a\ b\|c\xff").stages[0],
        Stage::Cp(Dest::Path(b"/mnt/a b|c\xff".to_vec()))
    );
    assert_eq!(
        q("mkdir 'two words'").stages[0],
        Stage::Mkdir(b"two words".to_vec())
    );
}

#[test]
fn an_escaped_word_is_never_a_keyword() {
    // A folder literally called `other` is a path, not the other pane.
    assert_eq!(
        q(r"cp >\x6fther").stages[0],
        Stage::Cp(Dest::Path(b"other".to_vec()))
    );
    // A name that starts with `!` is not a negation.
    assert_eq!(
        q(r"sel \x21x").stages[0],
        sel(vec![yes(Pred::Name(b"!x".to_vec()))])
    );
}

#[test]
fn errors_carry_their_offset() {
    assert_eq!(
        parse("sel ext:pdf | frobnicate").unwrap_err(),
        ParseError {
            offset: 14,
            kind: ParseErrorKind::UnknownVerb
        }
    );
    assert_eq!(
        parse("sel size>12Q").unwrap_err(),
        ParseError {
            offset: 4,
            kind: ParseErrorKind::BadNumber
        }
    );
    assert_eq!(parse("cp").unwrap_err().kind, ParseErrorKind::MissingDest);
    assert_eq!(parse("  ").unwrap_err().kind, ParseErrorKind::Empty);
    assert_eq!(
        parse(r"cp >a\x4").unwrap_err().kind,
        ParseErrorKind::BadEscape
    );
    assert_eq!(
        parse("mkdir 'ab").unwrap_err().kind,
        ParseErrorKind::Unterminated
    );
    assert_eq!(parse("mkdir").unwrap_err().kind, ParseErrorKind::MissingArg);
    assert_eq!(parse("sel").unwrap_err().kind, ParseErrorKind::MissingArg);
    assert_eq!(
        parse("trash now").unwrap_err().kind,
        ParseErrorKind::BadTerm
    );
    assert_eq!(parse("sel *|").unwrap_err().kind, ParseErrorKind::Empty);
}

/// A size at the edge of u128 with a fraction must be refused, not overflow
/// (the live preview parses on every key).
#[test]
fn a_huge_fractional_size_is_refused_not_overflowed() {
    let huge = format!("sel size>{}.9T", u128::MAX / 1024);
    assert_eq!(parse(&huge).unwrap_err().kind, ParseErrorKind::BadNumber);
}

#[test]
fn redo_is_refused_until_the_journal_has_one() {
    assert_eq!(parse("redo").unwrap_err().kind, ParseErrorKind::NotYet);
}

#[test]
fn log_and_undo_stand_alone() {
    assert_eq!(q("undo 3").stages, vec![Stage::Undo(Some(3))]);
    assert_eq!(q("log").stages, vec![Stage::Log(None)]);
    assert_eq!(
        parse("sel *.rs | undo").unwrap_err().kind,
        ParseErrorKind::StandsAlone
    );
}

#[test]
fn a_dotted_word_is_a_catalogue_command_without_args() {
    assert_eq!(
        q("pane.swap").stages,
        vec![Stage::Catalogue("pane.swap".into())]
    );
    assert_eq!(
        parse("pane.swap now").unwrap_err().kind,
        ParseErrorKind::BadTerm
    );
}

#[test]
fn a_first_stage_that_is_not_sel_consumes_the_marks() {
    assert_eq!(q("trash").stages, vec![Stage::Trash]);
}

#[test]
fn tokens_classify_the_line() {
    let line = "sel ext:pdf | cp >other";
    let classes: Vec<(&str, TokenClass)> = tokens(line)
        .into_iter()
        .map(|(r, c)| (&line[r], c))
        .collect();
    assert_eq!(
        classes,
        vec![
            ("sel", TokenClass::Verb),
            ("ext:", TokenClass::Key),
            ("pdf", TokenClass::Value),
            ("|", TokenClass::Pipe),
            ("cp", TokenClass::Verb),
            (">other", TokenClass::Dest),
        ]
    );
}

#[test]
fn tokens_mark_the_error_from_its_offset() {
    let line = "sel ext:pdf | frob";
    let last = tokens(line).pop().expect("tokens");
    assert_eq!(last, (14..18, TokenClass::Error));
}

/// A no-break space (or any Unicode space) typed between two words splits
/// them, as an ASCII space does: `>other` followed by NBSP is still the
/// other pane, not a folder called `other\u{a0}`.
#[test]
fn unicode_whitespace_splits_words_outside_quotes() {
    assert_eq!(
        q("cp\u{a0}>other\u{a0}").stages,
        vec![Stage::Cp(Dest::Other)]
    );
    assert_eq!(
        q("sel\u{3000}*.rs\u{2003}|\u{2009}trash").stages,
        vec![sel(vec![yes(Pred::Name(b"*.rs".to_vec()))]), Stage::Trash]
    );
    // Inside quotes, or escaped, it is part of the name.
    assert_eq!(
        q("mkdir 'a\u{a0}b'").stages,
        vec![Stage::Mkdir("a\u{a0}b".as_bytes().to_vec())]
    );
    assert_eq!(
        q(r"mkdir a\xc2\xa0b").stages,
        vec![Stage::Mkdir("a\u{a0}b".as_bytes().to_vec())]
    );
}

/// Characters a terminal draws as nothing: Unicode `Cf` (ZWSP, ZWJ, word
/// joiner, BOM, soft hyphen, bidi marks, a tag) plus the invisible letters
/// and marks the printer escapes (VS16, CGJ, a Hangul filler).
const INVISIBLE: &[char] = &[
    '\u{200B}',
    '\u{200C}',
    '\u{200D}',
    '\u{2060}',
    '\u{FEFF}',
    '\u{AD}',
    '\u{200E}',
    '\u{202E}',
    '\u{E0041}',
    '\u{FE0F}',
    '\u{34F}',
    '\u{3164}',
];

/// A keyword with an invisible character anywhere in it is refused, not
/// read as a name that looks exactly like the keyword: `cp >other` plus
/// ZWSP used to copy into a new `other<ZWSP>`. So is a negated `sel` value
/// with one (`!*.rs<ZWSP>` matches nothing, so it selects everything), and
/// a destination or argument made of nothing else.
#[test]
fn an_invisible_char_glued_to_a_keyword_is_refused() {
    for c in INVISIBLE {
        for line in [
            format!("cp >other{c}"),
            format!("mv >{c}other"),
            format!("cp >ot{c}her"),
            format!("cp > {c}here"),
            format!("cp >h{c}ere"),
            format!("cp {c}>other"),
            format!("sel{c} *.rs"),
            format!("s{c}el *.rs"),
            format!("{c}trash"),
            format!("pane.swap{c}"),
            format!("sel {c}ext:pdf"),
            format!("sel e{c}xt:pdf"),
            format!("sel {c}!*.rs"),
            format!("sel !{c}name:x"),
            format!("sel {c}size>1M"),
            format!("sel type:file{c}"),
            format!("sel !*.rs{c} | trash"),
            format!("sel !ext:p{c}df"),
            format!("cp >{c}"),
            format!("mkdir {c}"),
        ] {
            let e = parse(&line).expect_err(&line);
            assert_eq!(e.kind, ParseErrorKind::InvisibleChar, "{line:?}");
            assert!(
                line.is_char_boundary(e.offset) && line[e.offset..].starts_with(*c),
                "the offset points at the invisible char: {line:?} {e:?}"
            );
        }
    }
}

/// A name that merely CONTAINS an invisible character, without it hiding a
/// keyword, is a real file name and stays valid; so does a positive `sel`
/// pattern for one, and one typed escaped or quoted, which says it is a
/// name on purpose.
#[test]
fn an_invisible_char_inside_a_name_stays_valid() {
    let zw = "\u{200B}";
    assert_eq!(
        q(&format!("cp >ot{zw}hers")).stages,
        vec![Stage::Cp(Dest::Path(format!("ot{zw}hers").into_bytes()))]
    );
    assert_eq!(
        q(&format!("sel ext:p{zw}df")).stages,
        vec![sel(vec![yes(Pred::Ext(vec![
            format!("p{zw}df").into_bytes()
        ]))])]
    );
    assert_eq!(
        q(r"sel !*.rs\xe2\x80\x8b").stages,
        vec![sel(vec![Term {
            negated: true,
            pred: Pred::Name(format!("*.rs{zw}").into_bytes())
        }])]
    );
    assert_eq!(
        q(&format!("mkdir report{zw}")).stages,
        vec![Stage::Mkdir(format!("report{zw}").into_bytes())]
    );
    assert_eq!(
        q(&format!("cp >/tmp/other{zw}")).stages,
        vec![Stage::Cp(Dest::Path(
            format!("/tmp/other{zw}").into_bytes()
        ))]
    );
    assert_eq!(
        q(&format!("sel *.rs{zw}")).stages,
        vec![sel(vec![yes(Pred::Name(format!("*.rs{zw}").into_bytes()))])]
    );
    assert_eq!(
        q(&format!("cp >'other{zw}'")).stages,
        vec![Stage::Cp(Dest::Path(format!("other{zw}").into_bytes()))]
    );
    assert_eq!(
        q(r"cp >other\xe2\x80\x8b").stages,
        vec![Stage::Cp(Dest::Path(format!("other{zw}").into_bytes()))]
    );
    // A ZWJ inside an emoji is not at an edge of a keyword either.
    assert_eq!(
        q("mkdir \u{1F469}\u{200D}\u{1F4BB}").stages,
        vec![Stage::Mkdir(
            "\u{1F469}\u{200D}\u{1F4BB}".as_bytes().to_vec()
        )]
    );
}

/// The colour spans of a line split on a Unicode space stay on char
/// boundaries: the box slices the line with them.
#[test]
fn tokens_after_a_unicode_space_stay_on_char_boundaries() {
    let line = "sel\u{a0}*.rs";
    for (r, _) in tokens(line) {
        assert!(line.get(r.clone()).is_some(), "{r:?}");
    }
}

/// `name=` names one file by its exact name: `*` and `?` are letters there,
/// so a file literally called `*` can be targeted without its glob twin.
#[test]
fn name_equals_is_a_literal_name() {
    assert_eq!(
        q("sel name=*").stages,
        vec![sel(vec![yes(Pred::Literal(b"*".to_vec()))])]
    );
    assert_eq!(
        q(r"sel !name=a\x20b?").stages,
        vec![sel(vec![Term {
            negated: true,
            pred: Pred::Literal(b"a b?".to_vec()),
        }])]
    );
    // Typed escaped, it is a glob again.
    assert_eq!(
        q(r"sel name\x3d*").stages,
        vec![sel(vec![yes(Pred::Name(b"name=*".to_vec()))])]
    );
}
