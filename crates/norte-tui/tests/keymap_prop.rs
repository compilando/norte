//! Property-based tests for the keymap engine (spec §12: "keybinding
//! resolution — no ambiguous sequence"). If `Effective::build` accepts a
//! keymap, EVERY bound sequence resolves deterministically: Pending on
//! every prefix and Run exactly on the last key; and no arbitrary stream
//! can panic or leave a pending state longer than the longest sequence.
//!
//! K2a adds the resolver's second state machine — the numeric count's
//! accumulator — with the same discipline: over ANY stream, the count
//! neither overflows nor sticks to the next key.

use norte_tui::keymap::{Count, Effective, Resolution, Resolver, parse_chord, parse_keymap};
use proptest::prelude::*;

const COMANDOS: &[&str] = &["app.quit", "pane.switch", "cursor.up", "cursor.down"];
const TECLAS: &[&str] = &["a", "b", "g", "q", "G", "f5", "ctrl+c", "enter", "esc"];

fn arb_seq() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(
        proptest::sample::select(TECLAS).prop_map(str::to_owned),
        1..=3,
    )
}

fn arb_bindings() -> impl Strategy<Value = Vec<(Vec<String>, String)>> {
    proptest::collection::vec(
        (
            arb_seq(),
            proptest::sample::select(COMANDOS).prop_map(str::to_owned),
        ),
        0..8,
    )
}

fn to_toml(section: &str, bindings: &[(Vec<String>, String)], key: &str) -> String {
    use std::fmt::Write;
    let mut out = format!("[{section}]\n{key} = [\n");
    for (on, run) in bindings {
        let keys: Vec<String> = on.iter().map(|k| format!("{k:?}")).collect();
        let _ = writeln!(out, "  {{ on = [{}], run = {run:?} }},", keys.join(", "));
    }
    out.push_str("]\n");
    out
}

proptest! {
    #[test]
    fn keymap_aceptado_implica_resolucion_determinista(
        pane in arb_bindings(),
        global in arb_bindings(),
        user_pre in arb_bindings(),
        user_app in arb_bindings(),
        stream in proptest::collection::vec(proptest::sample::select(TECLAS), 0..32),
    ) {
        let src = format!(
            "{}{}",
            to_toml("pane", &pane, "keymap"),
            to_toml("global", &global, "keymap"),
        );
        let user_src = format!(
            "{}{}",
            to_toml("pane", &user_pre, "prepend_keymap"),
            to_toml("global", &user_app, "append_keymap"),
        );
        let file = parse_keymap(&src).expect("valid generated TOML");
        let user = parse_keymap(&user_src).expect("valid user TOML");
        let Ok(eff) = Effective::build(&file, Some(&user), COMANDOS) else {
            // Rejected on load (ambiguous/duplicate): exactly the contract.
            return Ok(());
        };
        let todas = || pane.iter().chain(&global).chain(&user_pre).chain(&user_app);
        let max_len = todas().map(|(on, _)| on.len()).max().unwrap_or(0);

        // 1) Every bound sequence walks Pending…Pending→Run (it does not
        //    matter which layer: an exact sequence that got overridden is
        //    still BOUND).
        for (on, _) in todas() {
            let mut r = Resolver::new(eff.clone());
            for (i, k) in on.iter().enumerate() {
                let res = r.push(parse_chord(k).unwrap());
                if i + 1 < on.len() {
                    prop_assert_eq!(res, Resolution::Pending(i + 1), "prefix of {:?}", on);
                } else {
                    prop_assert!(
                        matches!(res, Resolution::Run { .. }),
                        "end of {:?}: {:?}", on, res
                    );
                }
            }
        }

        // 2) No arbitrary stream panics or overflows the pending state; and
        //    the resolution is a FUNCTION of the stream (two identical passes).
        let mut r1 = Resolver::new(eff.clone());
        let mut r2 = Resolver::new(eff.clone());
        for k in &stream {
            let c = parse_chord(k).unwrap();
            let a = r1.push(c);
            let b = r2.push(c);
            prop_assert_eq!(a, b, "determinism");
            prop_assert!(r1.pending().len() <= max_len.max(1));
        }
    }
}

// --- K2a: the count's accumulator ------------------------------------

/// The accumulator's ceiling (`MAX_COUNT` in `resolve.rs`, private). Written
/// here by hand ON PURPOSE: deriving it from the engine would make the test
/// follow the implementation instead of pinning it, and the number is the
/// promise ("four digits, the fifth gets discarded"), not a detail.
const MAX_COUNT: u32 = 9_999;

/// Commands the generated keymaps bind. `cursor.page-down` is in the
/// CATALOGUE but NOT in [`CONOCIDOS`]: that is how a
/// [`Resolution::Unavailable`] gets manufactured — the fourth terminal
/// resolution, and the only one whose count-clearing path no example pins.
/// The other two cover both sides of the catalogue (`app.quit` does not
/// accept a count, `cursor.down` does).
const COMANDOS_CONTADOR: &[&str] = &["app.quit", "cursor.down", "cursor.page-down"];

/// What this "frontend" really implements.
const CONOCIDOS: &[&str] = &["app.quit", "cursor.down"];

/// BINDABLE keys with counts on. `0` goes in on purpose (it is still
/// bindable: a count never starts with zero); digits `1`-`9` do not,
/// because with `counts = true` binding them is a LOAD error and every case
/// would go down the `else` resolving nothing. `esc` does not either: it
/// only works as a standalone binding and the generated sequences would
/// knock it out on load.
const TECLAS_LIGABLES: &[&str] = &["a", "g", "q", "0", "enter"];

/// STREAM keys: digits (the count), bindable keys, one unbound one (`z`, a
/// miss) and `esc` (the cancellation).
const TECLAS_STREAM: &[&str] = &["a", "g", "q", "z", "enter", "esc"];
const DIGITOS: &[&str] = &["0", "1", "2", "3", "5", "9"];

fn arb_seq_ligable() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(
        proptest::sample::select(TECLAS_LIGABLES).prop_map(str::to_owned),
        1..=3,
    )
}

fn arb_bindings_contador() -> impl Strategy<Value = Vec<(Vec<String>, String)>> {
    proptest::collection::vec(
        (
            arb_seq_ligable(),
            proptest::sample::select(COMANDOS_CONTADOR).prop_map(str::to_owned),
        ),
        0..6,
    )
}

/// A stream keystroke, loaded toward digits: with a uniform split almost no
/// case would get to type five digits in a row, which is exactly where the
/// ceiling lives.
fn arb_pulsacion() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        7 => proptest::sample::select(DIGITOS),
        3 => proptest::sample::select(TECLAS_STREAM),
    ]
}

proptest! {
    /// The count's accumulator is the resolver's SECOND state machine, and
    /// `keymap/mod.rs`'s examples pin it key by key. What they do not pin is
    /// what must hold for ANY stream:
    ///
    /// 1. **The count never survives a terminal resolution.** `Run`,
    ///    `Reset` and `Unavailable` all leave it at `None`, always. A
    ///    number stuck to the next keystroke is the worst failure this
    ///    mechanism can have, and `Unavailable` — the only one of the three
    ///    with no example — is where it slips in most easily: it is an
    ///    `Exact` like `Run`'s, but through a different branch.
    /// 2. **The count that arrives with the command is the one that was
    ///    typed.** `Repeat(n)`/`Ignored(n)` are worth exactly what was in
    ///    flight before the key, and `None` only shows up if there was
    ///    nothing: the resolver neither invents nor rounds.
    /// 3. **`Repeat` belongs to the catalogue, not the resolver.** It only
    ///    emits it for a command whose `CommandDef` says `counts: true`;
    ///    the rest get `Ignored`. It is the rule that decides whether `5`
    ///    over a command fires once or five thousand times.
    /// 4. **The accumulator neither overflows nor goes backward.** With any
    ///    digit string of any length it stays within `1..=MAX_COUNT` and is
    ///    monotonic: a fifth digit gets DISCARDED, it never wraps `u32`
    ///    into a number nobody typed.
    /// 5. **Without the preset's flag there is no count worth anything.**
    ///    The same keymap without `counts = true` does not emit a single
    ///    `Counting` nor a single `Count` other than `None` — `orthodox`
    ///    and `cua` cannot grow counts behind your back. And `0` is still
    ///    bindable with counts on: both keymaps load or fail together.
    #[test]
    fn el_contador_ni_desborda_ni_se_pega_a_la_tecla_siguiente(
        pane in arb_bindings_contador(),
        global in arb_bindings_contador(),
        stream in proptest::collection::vec(arb_pulsacion(), 0..24),
    ) {
        let body = format!(
            "{}{}",
            to_toml("pane", &pane, "keymap"),
            to_toml("global", &global, "keymap"),
        );
        let con = parse_keymap(&format!("counts = true\n\n{body}")).expect("valid generated TOML");
        let sin = parse_keymap(&body).expect("valid generated TOML");
        let (con, sin) = (
            Effective::build(&con, None, CONOCIDOS),
            Effective::build(&sin, None, CONOCIDOS),
        );
        // Property 5, first half: turning counts on does not change WHICH
        // keymaps are legal, because the only key the two dispute — `0` —
        // is exempt from the load rule.
        prop_assert_eq!(
            con.is_ok(),
            sin.is_ok(),
            "the counts flag changed the keymap's legality"
        );
        let (Ok(con), Ok(sin)) = (con, sin) else {
            // Rejected on load (ambiguous/duplicate): the contract above.
            return Ok(());
        };

        let mut r = Resolver::new(con);
        let mut r_sin = Resolver::new(sin);
        for k in &stream {
            let c = parse_chord(k).expect("key from the alphabet");
            let before = r.count();
            let res = r.push(c);
            match &res {
                Resolution::Counting(n) => {
                    // 4: neither overflows nor goes backward.
                    prop_assert!((1..=MAX_COUNT).contains(n), "out of range: {:?}", res);
                    prop_assert_eq!(r.count(), Some(*n), "what is painted is what there is");
                    if let Some(previo) = before {
                        prop_assert!(*n >= previo, "the count went backward: {previo} → {n}");
                        // A digit can only be DISCARDED by the ceiling.
                        // Without this, an accumulator that stopped adding
                        // early would still be monotonic and would pass.
                        prop_assert!(
                            *n != previo || previo > MAX_COUNT / 10,
                            "digit discarded below the ceiling: {previo}"
                        );
                    }
                    prop_assert!(
                        r.pending().is_empty(),
                        "a count never opens with a sequence in flight"
                    );
                }
                Resolution::Run { command, count } => {
                    // 1: the count does NOT survive the command.
                    prop_assert_eq!(r.count(), None, "count alive after {:?}", res);
                    // 3: the catalogue is the authority.
                    let acepta = norte_frontend::keymap::catalogue::lookup(command)
                        .is_some_and(|d| d.counts);
                    match count {
                        // 2: neither invents…
                        Count::None => prop_assert_eq!(before, None, "invented count"),
                        // …nor rounds.
                        Count::Repeat(n) => {
                            prop_assert_eq!(before, Some(*n), "altered count");
                            prop_assert!(acepta, "{command} does not accept a count and got Repeat");
                        }
                        Count::Ignored(n) => {
                            prop_assert_eq!(before, Some(*n), "altered count");
                            prop_assert!(!acepta, "{command} accepts a count and got Ignored");
                        }
                    }
                }
                // 1: the other two terminals clear it the same way.
                Resolution::Reset | Resolution::Unavailable { .. } => {
                    prop_assert_eq!(r.count(), None, "count alive after {:?}", res);
                }
                // A half-typed sequence does NOT touch the count: in `12gg`
                // they coexist.
                Resolution::Pending(_) => prop_assert_eq!(r.count(), before, "the count moved"),
            }

            // 5, second half: the same stream without the preset's flag.
            let res_sin = r_sin.push(c);
            prop_assert!(
                !matches!(res_sin, Resolution::Counting(_)),
                "without the flag there is no Counting: {:?}", res_sin
            );
            prop_assert!(
                !matches!(res_sin, Resolution::Run { count, .. } if count != Count::None),
                "without the flag every Run arrives with Count::None: {:?}", res_sin
            );
            prop_assert_eq!(r_sin.count(), None, "without the flag there is no count to accumulate");
        }
    }
}
