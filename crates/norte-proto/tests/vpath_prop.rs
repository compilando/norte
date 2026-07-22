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
    // Charset de Authority: ASCII imprimible menos `%` (0x25) y `/` (0x2F). El
    // charset incluye `:` y `@`, así que un `user:pass@host` puede caer y ya
    // NO es válido (#46, proto 0.8.0): se FILTRA en vez de `expect`.
    let valid = proptest::string::string_regex("[!-$&-.0-~]{1,16}").expect("regex válida");
    proptest::option::of(valid.prop_filter_map("authority válida", |s| Authority::new(&s).ok()))
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

/// `VPath`s plausibles como EXTERIOR de un archivo (ADR 0018): scheme sin
/// prefijo de formato, ≥1 segmento, sin segmentos `!`.
fn arb_archive_outer() -> impl Strategy<Value = VPath> {
    arb_vpath().prop_filter("exterior componible", |p| {
        !p.is_root()
            && p.archive_split().is_ok_and(|r| r.is_none())
            && p.segments().all(|s| s != b"!")
    })
}

proptest! {
    /// El "roundtrip garantizado" del rustdoc de `archive_compose`:
    /// split(compose(f, outer, inner)) == (f, outer, inner).
    #[test]
    fn prop_archive_compose_split_roundtrip(
        outer in arb_archive_outer(),
        inner_bytes in proptest::collection::vec(arb_segment_bytes(), 0..6),
        format in proptest::sample::select(norte_proto::ARCHIVE_FORMATS),
    ) {
        // `outer` y `format` se generan de forma independiente: con tokens
        // compuestos como `tar+gz` en la whitelist, algunas combinaciones
        // (p. ej. format="tar" sobre un outer de scheme "gz+algo") formarían
        // un scheme ambiguo que `archive_compose` rechaza a propósito (ver
        // `archive_compose_rechaza_roundtrip_ambiguo_con_tar_gz` en
        // `tests/vpath.rs`, ADR 0028) — no es representativo del roundtrip
        // que esta propiedad ejercita, así que la combinación se descarta en
        // vez de fabricar un panic espurio del `.expect()`.
        prop_assume!(
            norte_proto::scheme_archive_format(&format!("{format}+{}", outer.scheme()))
                == Some(format)
        );
        let inner: Vec<Segment> = inner_bytes
            .into_iter()
            .filter(|b| b.as_slice() != b"!")
            .map(|b| Segment::new(b).expect("estrategia válida"))
            .collect();
        let p = VPath::archive_compose(format, &outer, &inner).expect("compose válido");
        let r = p.archive_split().expect("bien formado").expect("compuesto");
        prop_assert_eq!(r.format.as_str(), format);
        prop_assert_eq!(r.outer, outer);
        prop_assert_eq!(r.inner, inner);
        // Y el wire del compuesto reparsea al mismo path (transitividad con
        // prop_reparse_idempotent).
        prop_assert_eq!(VPath::parse(&p.to_wire()).expect("wire válido"), p);
    }

    /// #56: roundtrip por CAPAS — componer una segunda capa sobre un path de
    /// archivo bien formado y pelarla devuelve exactamente lo compuesto, y
    /// la capa de abajo queda intacta.
    #[test]
    fn prop_archive_nested_two_layers_roundtrip(
        outer in arb_archive_outer(),
        inner1_bytes in proptest::collection::vec(arb_segment_bytes(), 1..4),
        inner2_bytes in proptest::collection::vec(arb_segment_bytes(), 0..4),
        f1 in proptest::sample::select(norte_proto::ARCHIVE_FORMATS),
        f2 in proptest::sample::select(norte_proto::ARCHIVE_FORMATS),
    ) {
        prop_assume!(
            norte_proto::scheme_archive_format(&format!("{f1}+{}", outer.scheme()))
                == Some(f1)
        );
        // La capa 2 se antepone al scheme YA compuesto: misma guardia de
        // ambigüedad (p. ej. f2="tar" sobre "gz+…" resolvería "tar+gz").
        prop_assume!(
            norte_proto::scheme_archive_format(&format!("{f2}+{f1}+{}", outer.scheme()))
                == Some(f2)
        );
        let seg_ok = |b: &Vec<u8>| b.as_slice() != b"!";
        let inner1: Vec<Segment> = inner1_bytes.into_iter().filter(seg_ok)
            .map(|b| Segment::new(b).expect("estrategia válida")).collect();
        let inner2: Vec<Segment> = inner2_bytes.into_iter().filter(seg_ok)
            .map(|b| Segment::new(b).expect("estrategia válida")).collect();
        prop_assume!(!inner1.is_empty()); // la capa 1 nombra un contenedor real
        let capa1 = VPath::archive_compose(f1, &outer, &inner1).expect("capa 1");
        let capa2 = VPath::archive_compose(f2, &capa1, &inner2).expect("capa 2");
        let r = capa2.archive_split().expect("bien formado").expect("compuesto");
        prop_assert_eq!(r.format.as_str(), f2);
        prop_assert_eq!(&r.outer, &capa1);
        prop_assert_eq!(r.inner, inner2);
        let r1 = r.outer.archive_split().expect("bien formada").expect("compuesta");
        prop_assert_eq!(r1.format.as_str(), f1);
        prop_assert_eq!(r1.outer, outer);
        prop_assert_eq!(r1.inner, inner1);
        prop_assert_eq!(VPath::parse(&capa2.to_wire()).expect("wire válido"), capa2);
    }
}
