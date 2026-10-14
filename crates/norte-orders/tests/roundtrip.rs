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
