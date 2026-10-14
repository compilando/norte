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
