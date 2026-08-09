//! Property-based del keymap engine (spec §12: "resolución de keybindings
//! — ninguna secuencia ambigua"). Si `Effective::build` acepta un keymap,
//! TODA secuencia ligada se resuelve determinista: Pending en cada prefijo
//! y Run exactamente en la última tecla; y ningún stream arbitrario puede
//! panicar ni dejar pending por encima de la secuencia más larga.
//!
//! K2a añade la segunda máquina de estados del resolver — el acumulador del
//! contador numérico — con la misma disciplina: sobre CUALQUIER stream, el
//! contador ni desborda ni se pega a la tecla siguiente.

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

// --- K2a: el acumulador del contador ------------------------------------

/// El techo del acumulador (`MAX_COUNT` en `resolve.rs`, privado). Escrito
/// aquí a mano A PROPÓSITO: derivarlo del motor haría que el test siguiera a
/// la implementación en vez de pinearla, y el número es la promesa («cuatro
/// dígitos, el quinto se descarta»), no un detalle.
const MAX_COUNT: u32 = 9_999;

/// Comandos que los keymaps generados ligan. `cursor.page-down` está en el
/// CATÁLOGO pero NO en [`CONOCIDOS`]: así es como se fabrica un
/// [`Resolution::Unavailable`] — la cuarta resolución terminal, y la única
/// cuyo camino de limpieza del contador no pinea ningún ejemplo. Los otros
/// dos cubren los dos lados del catálogo (`app.quit` no acepta contador,
/// `cursor.down` sí).
const COMANDOS_CONTADOR: &[&str] = &["app.quit", "cursor.down", "cursor.page-down"];

/// Lo que este «frontend» implementa de verdad.
const CONOCIDOS: &[&str] = &["app.quit", "cursor.down"];

/// Teclas LIGABLES con contadores encendidos. El `0` va dentro a propósito
/// (sigue siendo ligable: un contador jamás empieza por cero); los dígitos
/// `1`-`9` no, porque con `counts = true` ligarlos es error de CARGA y todos
/// los casos se irían por el `else` sin resolver nada. `esc` tampoco: solo
/// vale como binding suelto y las secuencias generadas lo tumbarían al
/// cargar.
const TECLAS_LIGABLES: &[&str] = &["a", "g", "q", "0", "enter"];

/// Teclas del STREAM: dígitos (el contador), teclas ligables, una sin ligar
/// (`z`, que es un miss) y `esc` (la cancelación).
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

/// Una pulsación del stream, cargada hacia los dígitos: con reparto uniforme
/// casi ningún caso llegaría a teclear cinco dígitos seguidos, que es justo
/// donde vive el techo.
fn arb_pulsacion() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        7 => proptest::sample::select(DIGITOS),
        3 => proptest::sample::select(TECLAS_STREAM),
    ]
}

proptest! {
    /// El acumulador del contador es la SEGUNDA máquina de estados del
    /// resolver, y los ejemplos de `keymap/mod.rs` la pinean tecla a tecla.
    /// Lo que no pinean es lo que debe valer para CUALQUIER stream:
    ///
    /// 1. **El contador jamás sobrevive a una resolución terminal.** `Run`,
    ///    `Reset` y `Unavailable` lo dejan en `None`, siempre. Un número
    ///    pegado a la pulsación siguiente es el peor fallo que este mecanismo
    ///    puede tener, y `Unavailable` —el único de los tres sin ejemplo— es
    ///    donde más fácil se cuela: es un `Exact` como el de `Run`, pero por
    ///    otra rama.
    /// 2. **El contador que llega con el comando es el que se tecleó.**
    ///    `Repeat(n)`/`Ignored(n)` valen exactamente lo que había en vuelo
    ///    antes de la tecla, y `None` solo aparece si no había nada: el
    ///    resolver ni inventa ni redondea.
    /// 3. **`Repeat` es del catálogo, no del resolver.** Solo lo emite para un
    ///    comando cuyo `CommandDef` dice `counts: true`; el resto es
    ///    `Ignored`. Es la regla que decide si `5` sobre un comando dispara
    ///    una vez o cinco mil.
    /// 4. **El acumulador no desborda ni retrocede.** Con cualquier ristra de
    ///    dígitos de cualquier largo se queda en `1..=MAX_COUNT` y es
    ///    monótono: un quinto dígito se DESCARTA, jamás envuelve `u32` a un
    ///    número que nadie tecleó.
    /// 5. **Sin el flag del preset no hay contador que valga.** El mismo
    ///    keymap sin `counts = true` no emite un solo `Counting` ni un solo
    ///    `Count` distinto de `None` — `orthodox` y `cua` no pueden criar
    ///    contadores por la espalda. Y el `0` sigue ligable con contadores:
    ///    los dos keymaps cargan o fallan a la vez.
    #[test]
    fn el_contador_ni_desborda_ni_se_pega_a_la_tecla_siguiente(
        pane in arb_bindings_contador(),
        global in arb_bindings_contador(),
        stream in proptest::collection::vec(arb_pulsacion(), 0..24),
    ) {
        let cuerpo = format!(
            "{}{}",
            to_toml("pane", &pane, "keymap"),
            to_toml("global", &global, "keymap"),
        );
        let con = parse_keymap(&format!("counts = true\n\n{cuerpo}")).expect("TOML generado válido");
        let sin = parse_keymap(&cuerpo).expect("TOML generado válido");
        let (con, sin) = (
            Effective::build(&con, None, CONOCIDOS),
            Effective::build(&sin, None, CONOCIDOS),
        );
        // Propiedad 5, primera mitad: encender los contadores no cambia QUÉ
        // keymaps son legales, porque la única tecla que los dos se disputan
        // —el `0`— está exenta de la regla de carga.
        prop_assert_eq!(
            con.is_ok(),
            sin.is_ok(),
            "el flag de contadores cambió la legalidad del keymap"
        );
        let (Ok(con), Ok(sin)) = (con, sin) else {
            // Rechazado al cargar (ambiguo/duplicado): el contrato de arriba.
            return Ok(());
        };

        let mut r = Resolver::new(con);
        let mut r_sin = Resolver::new(sin);
        for k in &stream {
            let c = parse_chord(k).expect("tecla del alfabeto");
            let antes = r.count();
            let res = r.push(c);
            match &res {
                Resolution::Counting(n) => {
                    // 4: ni desborda ni retrocede.
                    prop_assert!((1..=MAX_COUNT).contains(n), "fuera de rango: {:?}", res);
                    prop_assert_eq!(r.count(), Some(*n), "lo que se pinta es lo que hay");
                    if let Some(previo) = antes {
                        prop_assert!(*n >= previo, "el contador retrocedió: {previo} → {n}");
                        // Un dígito solo puede DESCARTARSE por el techo. Sin
                        // esto, un acumulador que dejara de sumar antes de
                        // tiempo seguiría siendo monótono y pasaría.
                        prop_assert!(
                            *n != previo || previo > MAX_COUNT / 10,
                            "dígito descartado por debajo del techo: {previo}"
                        );
                    }
                    prop_assert!(
                        r.pending().is_empty(),
                        "un contador jamás se abre con secuencia en vuelo"
                    );
                }
                Resolution::Run { command, count } => {
                    // 1: el contador NO sobrevive al comando.
                    prop_assert_eq!(r.count(), None, "contador vivo tras {:?}", res);
                    // 3: la autoridad es el catálogo.
                    let acepta = norte_frontend::keymap::catalogue::lookup(command)
                        .is_some_and(|d| d.counts);
                    match count {
                        // 2: ni inventa…
                        Count::None => prop_assert_eq!(antes, None, "contador inventado"),
                        // …ni redondea.
                        Count::Repeat(n) => {
                            prop_assert_eq!(antes, Some(*n), "contador alterado");
                            prop_assert!(acepta, "{command} no acepta contador y le llegó Repeat");
                        }
                        Count::Ignored(n) => {
                            prop_assert_eq!(antes, Some(*n), "contador alterado");
                            prop_assert!(!acepta, "{command} acepta contador y le llegó Ignored");
                        }
                    }
                }
                // 1: las otras dos terminales lo limpian igual.
                Resolution::Reset | Resolution::Unavailable { .. } => {
                    prop_assert_eq!(r.count(), None, "contador vivo tras {:?}", res);
                }
                // Una secuencia a medias NO toca el contador: en `12gg`
                // conviven.
                Resolution::Pending(_) => prop_assert_eq!(r.count(), antes, "el contador se movió"),
            }

            // 5, segunda mitad: el mismo stream sin el flag del preset.
            let res_sin = r_sin.push(c);
            prop_assert!(
                !matches!(res_sin, Resolution::Counting(_)),
                "sin el flag no hay Counting: {:?}", res_sin
            );
            prop_assert!(
                !matches!(res_sin, Resolution::Run { count, .. } if count != Count::None),
                "sin el flag todo Run llega con Count::None: {:?}", res_sin
            );
            prop_assert_eq!(r_sin.count(), None, "sin el flag no hay contador que acumular");
        }
    }
}
