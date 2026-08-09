//! Property-based del keymap engine (spec §12: "resolución de keybindings
//! — ninguna secuencia ambigua"). Si `Effective::build` acepta un keymap,
//! TODA secuencia ligada se resuelve determinista: Pending en cada prefijo
//! y Run exactamente en la última tecla; y ningún stream arbitrario puede
//! panicar ni dejar pending por encima de la secuencia más larga.

use norte_tui::keymap::{Effective, Resolution, Resolver, parse_chord, parse_keymap};
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
        let teclas: Vec<String> = on.iter().map(|k| format!("{k:?}")).collect();
        let _ = writeln!(out, "  {{ on = [{}], run = {run:?} }},", teclas.join(", "));
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
        let file = parse_keymap(&src).expect("TOML generado válido");
        let user = parse_keymap(&user_src).expect("TOML de usuario válido");
        let Ok(eff) = Effective::build(&file, Some(&user), COMANDOS) else {
            // Rechazado al cargar (ambiguo/duplicado): exactamente el contrato.
            return Ok(());
        };
        let todas = || pane.iter().chain(&global).chain(&user_pre).chain(&user_app);
        let max_len = todas().map(|(on, _)| on.len()).max().unwrap_or(0);

        // 1) Toda secuencia ligada camina Pending…Pending→Run (da igual la
        //    capa: una secuencia exacta pisada sigue estando LIGADA).
        for (on, _) in todas() {
            let mut r = Resolver::new(eff.clone());
            for (i, k) in on.iter().enumerate() {
                let res = r.push(parse_chord(k).unwrap());
                if i + 1 < on.len() {
                    prop_assert_eq!(res, Resolution::Pending(i + 1), "prefijo de {:?}", on);
                } else {
                    prop_assert!(
                        matches!(res, Resolution::Run { .. }),
                        "fin de {:?}: {:?}", on, res
                    );
                }
            }
        }

        // 2) Ningún stream arbitrario panica ni desborda el pending; y la
        //    resolución es una FUNCIÓN del stream (dos pasadas idénticas).
        let mut r1 = Resolver::new(eff.clone());
        let mut r2 = Resolver::new(eff.clone());
        for k in &stream {
            let c = parse_chord(k).unwrap();
            let a = r1.push(c);
            let b = r2.push(c);
            prop_assert_eq!(a, b, "determinismo");
            prop_assert!(r1.pending().len() <= max_len.max(1));
        }
    }
}
