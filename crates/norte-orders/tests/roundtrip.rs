//! `parse(print(order)) == order` over hostile names, and a printed order
//! never carries a byte a terminal would act on.

use norte_orders::*;
use proptest::prelude::*;

fn name() -> impl Strategy<Value = Vec<u8>> {
    norte_testkit::strategies::arb_hostile_filename()
}

fn path() -> impl Strategy<Value = Vec<u8>> {
    name().prop_filter("a path is not empty", |n| !n.is_empty())
}

fn dest() -> impl Strategy<Value = Dest> {
    prop_oneof![
        Just(Dest::Other),
        Just(Dest::Here),
        path().prop_map(Dest::Path),
        Just(Dest::Path(b"other".to_vec())),
    ]
}

fn arb_order() -> impl Strategy<Value = Order> {
    let cmp = prop_oneof![
        Just(Cmp::Lt),
        Just(Cmp::Le),
        Just(Cmp::Gt),
        Just(Cmp::Ge),
        Just(Cmp::Eq)
    ];
    let pred = prop_oneof![
        name().prop_map(Pred::Name),
        name().prop_map(Pred::Literal),
        prop::collection::vec(name(), 1..3).prop_map(Pred::Ext),
        (cmp.clone(), 0u64..1 << 50).prop_map(|(c, n)| Pred::Size(c, n)),
        (cmp, 0u64..1 << 34).prop_map(|(c, n)| Pred::Age(c, n)),
        prop_oneof![Just(KindSel::File), Just(KindSel::Dir), Just(KindSel::Link)]
            .prop_map(Pred::Kind),
    ];
    let term = (any::<bool>(), pred).prop_map(|(negated, pred)| Term { negated, pred });
    let sel = prop::collection::vec(term, 1..4).prop_map(|terms| Stage::Sel(Query { terms }));
    let act = prop_oneof![
        dest().prop_map(Stage::Cp),
        dest().prop_map(Stage::Mv),
        Just(Stage::Trash),
        path().prop_map(Stage::Mkdir),
        path().prop_map(Stage::Cd),
    ];
    (sel, act).prop_map(|(s, a)| Order { stages: vec![s, a] })
}

proptest! {
    #[test]
    fn print_then_parse_is_identity(o in arb_order()) {
        let line = print(&o);
        prop_assert_eq!(parse(&line), Ok(o), "line: {:?}", line);
    }

    #[test]
    fn printed_lines_carry_no_terminal_hazard(o in arb_order()) {
        let line = print(&o);
        prop_assert!(
            line.chars().all(|c| !norte_encoding::is_terminal_hazard(c)),
            "line: {:?}", line
        );
    }
}

proptest! {
    /// A name written with `escape_name` is read back as exactly its bytes,
    /// and carries nothing a terminal would act on: what the assistant is
    /// shown is what it must write.
    #[test]
    fn an_escaped_name_reads_back_as_its_bytes(n in path()) {
        let e = escape_name(&n);
        prop_assert_eq!(
            parse(&format!("mkdir {e}")).map(|o| o.stages),
            Ok(vec![Stage::Mkdir(n.clone())])
        );
        prop_assert!(e.chars().all(|c| !norte_encoding::is_terminal_hazard(c)));
    }
}

#[test]
fn standalone_stages_round_trip() {
    for o in [
        Order {
            stages: vec![Stage::Log(None)],
        },
        Order {
            stages: vec![Stage::Undo(Some(7))],
        },
        Order {
            stages: vec![Stage::Catalogue("pane.swap".into())],
        },
        Order {
            stages: vec![Stage::Trash],
        },
    ] {
        assert_eq!(parse(&print(&o)), Ok(o));
    }
}

#[test]
fn print_keeps_plain_names_readable() {
    let o = parse("sel ext:pdf age<30d | cp >/mnt/backup").expect("parse");
    assert_eq!(print(&o), "sel ext:pdf age<30d | cp >/mnt/backup");
}

/// A combining mark that follows a letter is part of how the name reads
/// (Hindi, decomposed Latin): it stays raw. Alone, or after something
/// escaped, it would draw on whatever precedes it, so it is escaped.
#[test]
fn a_combining_mark_after_a_letter_stays_raw() {
    assert_eq!(escape_name("cafe\u{301}".as_bytes()), "cafe\u{301}");
    assert_eq!(escape_name("हिन्दी".as_bytes()), "हिन्दी");
    assert_eq!(escape_name("\u{301}x".as_bytes()), r"\xcc\x81x");
    assert_eq!(escape_name("a \u{301}".as_bytes()), r"a\x20\xcc\x81");
    // Vietnamese stacks two.
    assert_eq!(escape_name("e\u{323}\u{302}".as_bytes()), "e\u{323}\u{302}");
}

/// A stack of marks past what any script needs (Zalgo) draws over the lines
/// around it: from the fifth on, they are escaped.
#[test]
fn a_tall_stack_of_marks_is_escaped_past_four() {
    let zalgo = format!("a{}", "\u{300}".repeat(16));
    let e = escape_name(zalgo.as_bytes());
    assert!(e.starts_with(&format!("a{}", "\u{300}".repeat(4))), "{e:?}");
    assert_eq!(e.matches('\u{300}').count(), 4, "{e:?}");
    assert_eq!(
        parse(&format!("mkdir {e}")).map(|o| o.stages),
        Ok(vec![Stage::Mkdir(zalgo.into_bytes())])
    );
}

/// Bidi controls and every other terminal hazard are escaped, letter or
/// mark or neither, wherever they fall; a right-to-left LETTER is a letter.
#[test]
fn every_terminal_hazard_is_escaped_even_after_a_letter() {
    for cp in 0..=0x10_FFFFu32 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        if !norte_encoding::is_terminal_hazard(c) {
            continue;
        }
        let e = escape_name(format!("a{c}").as_bytes());
        assert!(!e.contains(c), "U+{cp:04X} printed raw: {e:?}");
    }
    assert_eq!(escape_name("שלום".as_bytes()), "שלום");
    assert_eq!(escape_name("a\u{202e}b".as_bytes()), r"a\xe2\x80\xaeb");
}

/// The corpus's twin of `other` (a trailing ZWSP) cannot be typed raw after
/// `>` — it would read as the keyword — but the printer's escape names it.
#[test]
fn the_keyword_twin_of_the_corpus_is_refused_raw_and_named_escaped() {
    let twin = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "keyword_twin_zwsp")
        .expect("corpus fixture")
        .bytes;
    let raw = String::from_utf8(twin.clone()).expect("utf-8");
    assert_eq!(
        parse(&format!("cp >{raw}")).map_err(|e| e.kind),
        Err(ParseErrorKind::InvisibleChar)
    );
    let o = Order {
        stages: vec![Stage::Cp(Dest::Path(twin))],
    };
    assert_eq!(parse(&print(&o)), Ok(o));
}

/// What the lexer splits on, the printer escapes: a name with a no-break
/// space prints so it reads back whole.
#[test]
fn unicode_spaces_in_a_name_round_trip() {
    for n in ["a\u{a0}b", "x\u{3000}", "\u{2028}y", "a\u{85}b"] {
        let o = Order {
            stages: vec![Stage::Mkdir(n.as_bytes().to_vec())],
        };
        assert_eq!(parse(&print(&o)), Ok(o.clone()), "{n:?}");
    }
}

fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        name(),
        Just(Vec::new()),
        Just(b"other".to_vec()),
        Just(b"here".to_vec()),
        prop::collection::vec(any::<u8>(), 0..6),
    ]
}

/// ANY tree, including the ones the parser never produces: an empty order,
/// an empty `sel`, `ext:` with no list, an empty name, a catalogue "id" with
/// spaces and escapes in it, `log` inside a pipe.
fn arb_any_order() -> impl Strategy<Value = Order> {
    let cmp = prop_oneof![
        Just(Cmp::Lt),
        Just(Cmp::Le),
        Just(Cmp::Gt),
        Just(Cmp::Ge),
        Just(Cmp::Eq)
    ];
    let pred = prop_oneof![
        arb_bytes().prop_map(Pred::Name),
        arb_bytes().prop_map(Pred::Literal),
        prop::collection::vec(arb_bytes(), 0..3).prop_map(Pred::Ext),
        (cmp.clone(), any::<u64>()).prop_map(|(c, n)| Pred::Size(c, n)),
        (cmp, any::<u64>()).prop_map(|(c, n)| Pred::Age(c, n)),
        prop_oneof![Just(KindSel::File), Just(KindSel::Dir), Just(KindSel::Link)]
            .prop_map(Pred::Kind),
    ];
    let term = (any::<bool>(), pred).prop_map(|(negated, pred)| Term { negated, pred });
    let dest = prop_oneof![
        Just(Dest::Other),
        Just(Dest::Here),
        arb_bytes().prop_map(Dest::Path),
    ];
    let catalogue = prop_oneof![
        Just("pane.swap".to_owned()),
        Just(String::new()),
        Just("sel".to_owned()),
        any::<String>(),
    ];
    let stage = prop_oneof![
        prop::collection::vec(term, 0..3).prop_map(|terms| Stage::Sel(Query { terms })),
        dest.clone().prop_map(Stage::Cp),
        dest.prop_map(Stage::Mv),
        Just(Stage::Trash),
        arb_bytes().prop_map(Stage::Mkdir),
        arb_bytes().prop_map(Stage::Cd),
        any::<Option<u32>>().prop_map(Stage::Log),
        any::<Option<u32>>().prop_map(Stage::Undo),
        catalogue.prop_map(Stage::Catalogue),
    ];
    prop::collection::vec(stage, 0..4).prop_map(|stages| Order { stages })
}

/// Whether the parser can produce this tree — the trees `print` promises to
/// give back.
fn producible(o: &Order) -> bool {
    let id_ok = |s: &str| {
        s.contains('.')
            && !s.starts_with('.')
            && !s.ends_with('.')
            && s.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            })
    };
    let stage_ok = |s: &Stage| match s {
        Stage::Sel(q) => {
            !q.terms.is_empty()
                && q.terms
                    .iter()
                    .all(|t| !matches!(&t.pred, Pred::Ext(l) if l.is_empty()))
        }
        Stage::Cp(Dest::Path(p)) | Stage::Mv(Dest::Path(p)) => !p.is_empty(),
        Stage::Mkdir(n) | Stage::Cd(n) => !n.is_empty(),
        Stage::Catalogue(id) => id_ok(id),
        _ => true,
    };
    let alone = o.stages.len() == 1
        || !o
            .stages
            .iter()
            .any(|s| matches!(s, Stage::Log(_) | Stage::Undo(_)));
    !o.stages.is_empty() && alone && o.stages.iter().all(stage_ok)
}

proptest! {
    /// `print` is total: no tree makes it panic or write a terminal hazard,
    /// and every tree the parser can produce comes back from its line.
    #[test]
    fn print_is_total_and_round_trips_what_parse_produces(o in arb_any_order()) {
        let line = print(&o);
        prop_assert!(
            line.chars().all(|c| !norte_encoding::is_terminal_hazard(c)),
            "line: {:?}", line
        );
        if producible(&o) {
            prop_assert_eq!(parse(&line), Ok(o), "line: {:?}", line);
        }
    }
}

#[test]
fn print_of_trees_the_parser_never_makes_is_defined() {
    assert_eq!(print(&Order { stages: vec![] }), "");
    assert_eq!(
        print(&Order {
            stages: vec![Stage::Sel(Query {
                terms: vec![Term {
                    negated: false,
                    pred: Pred::Ext(vec![])
                }]
            })]
        }),
        "sel ext:"
    );
    assert_eq!(
        print(&Order {
            stages: vec![Stage::Mkdir(Vec::new())]
        }),
        "mkdir ''"
    );
    assert_eq!(
        print(&Order {
            stages: vec![Stage::Catalogue("x y\u{1b}".into())]
        }),
        r"x\x20y\x1b"
    );
}
