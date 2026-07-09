//! Property-based tests de `VPath` (spec §12): el roundtrip byte-exacto es
//! la propiedad fundacional del proyecto — un path jamás se corrompe.
//!
//! Las estrategias canónicas migrarán a `norte-testkit::strategies` en la
//! fase de testkit; aquí viven las definiciones originales.

use norte_proto::{Authority, Scheme, Segment, VPath};
use proptest::prelude::*;

fn arb_segment_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 1..64)
        .prop_filter("sin NUL ni separador", |b| {
            !b.contains(&0x00) && !b.contains(&0x2F)
        })
        .prop_filter("sin dot-segments", |b| b != b"." && b != b"..")
}

fn arb_scheme() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z][a-z0-9+.-]{0,10}").expect("regex válida")
}

fn arb_authority() -> impl Strategy<Value = Option<Authority>> {
    // Charset completo de Authority: ASCII imprimible menos `%` (0x25) y `/` (0x2F).
    let valid = proptest::string::string_regex("[!-$&-.0-~]{1,16}").expect("regex válida");
    proptest::option::of(
        valid.prop_map(|s| Authority::new(&s).expect("estrategia genera authorities válidas")),
    )
}

prop_compose! {
    fn arb_vpath()(
        scheme in arb_scheme(),
        authority in arb_authority(),
        segs in proptest::collection::vec(arb_segment_bytes(), 0..8),
    ) -> VPath {
        let scheme = Scheme::new(&scheme).expect("estrategia genera schemes válidos");
        let mut p = VPath::root(scheme, authority);
        for s in segs {
            p = p.join(Segment::new(s).expect("estrategia genera segmentos válidos"));
        }
        p
    }
}

proptest! {
    /// LA propiedad crítica: bytes arbitrarios → wire → parse → bytes idénticos.
    #[test]
    fn prop_roundtrip_bytes(p in arb_vpath()) {
        let wire = p.to_wire();
        let q = VPath::parse(&wire).expect("to_wire siempre produce wire parseable");
        let a: Vec<&[u8]> = p.segments().collect();
        let b: Vec<&[u8]> = q.segments().collect();
        prop_assert_eq!(a, b);
        prop_assert_eq!(p.scheme(), q.scheme());
        prop_assert_eq!(p.authority(), q.authority());
    }

    /// parse jamás panica, con cualquier basura.
    #[test]
    fn prop_parse_never_panics(s in ".*") {
        let _ = VPath::parse(&s);
    }

    /// Basura sesgada hacia la gramática (escapes truncados, dobles slashes…).
    #[test]
    fn prop_parse_never_panics_grammarlike(
        s in "[a-zA-Z0-9]{0,4}(://)?[a-z0-9%/.]{0,30}%?[0-9A-Fa-f]?"
    ) {
        let _ = VPath::parse(&s);
    }

    /// Todo parse Ok cumple las invariantes de segmento.
    #[test]
    fn prop_parse_ok_implies_invariants(s in ".*") {
        if let Ok(p) = VPath::parse(&s) {
            for seg in p.segments() {
                prop_assert!(!seg.is_empty());
                prop_assert!(!seg.contains(&0x00));
                prop_assert!(!seg.contains(&0x2F));
                prop_assert!(seg != b"." && seg != b"..");
            }
        }
    }

    /// Reparse idempotente: to_wire es punto fijo tras un parse.
    #[test]
    fn prop_reparse_idempotent(s in ".*") {
        if let Ok(p) = VPath::parse(&s) {
            let w1 = p.to_wire();
            let q = VPath::parse(&w1).expect("wire canónico parsea");
            prop_assert_eq!(&p, &q);
            prop_assert_eq!(w1, q.to_wire());
        }
    }

    /// El wire es inyectivo: wires iguales ⇒ paths iguales.
    #[test]
    fn prop_wire_injective(p1 in arb_vpath(), p2 in arb_vpath()) {
        if p1.to_wire() == p2.to_wire() {
            prop_assert_eq!(p1, p2);
        }
    }

    /// join/parent/file_name coherentes.
    #[test]
    fn prop_join_parent_inverse(p in arb_vpath(), s in arb_segment_bytes()) {
        let seg = Segment::new(s).expect("segmento válido");
        let child = p.join(seg.clone());
        prop_assert_eq!(child.parent(), Some(p));
        prop_assert_eq!(child.file_name(), Some(&seg));
    }

    /// Serde roundtrip por JSON real.
    #[test]
    fn prop_serde_roundtrip(p in arb_vpath()) {
        let json = serde_json::to_string(&p).expect("serializable");
        let q: VPath = serde_json::from_str(&json).expect("deserializable");
        prop_assert_eq!(p, q);
    }

    /// display_lossy termina siempre; segmentos UTF-8 limpios (sin U+FFFD
    /// legítimo ni controles) no introducen `�`.
    #[test]
    fn prop_display_never_panics(p in arb_vpath()) {
        let d = p.display_lossy();
        let all_clean_utf8 = p.segments().all(|s| {
            std::str::from_utf8(s).is_ok_and(|t| {
                !t.contains(char::REPLACEMENT_CHARACTER) && !t.chars().any(char::is_control)
            })
        });
        if all_clean_utf8 {
            let clean = !d.contains(char::REPLACEMENT_CHARACTER);
            prop_assert!(clean);
        }
    }

    /// El wire jamás lleva controles crudos (terminal injection en logs).
    #[test]
    fn prop_wire_no_raw_controls(p in arb_vpath()) {
        prop_assert!(!p.to_wire().chars().any(|c| c.is_ascii_control()));
    }

    /// El display jamás es un wire válido: imposible reconstruir un path desde él.
    #[test]
    fn prop_display_never_reparses(p in arb_vpath()) {
        prop_assert!(VPath::parse(&p.display_lossy()).is_err());
    }

    /// El display jamás emite caracteres de control, vengan de donde vengan los bytes.
    #[test]
    fn prop_display_no_controls(p in arb_vpath()) {
        prop_assert!(!p.display_lossy().chars().any(char::is_control));
    }
}
