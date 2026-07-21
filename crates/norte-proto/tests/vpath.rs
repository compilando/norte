//! Tests deterministas de `VPath`: parse, navegación, display, serde, orden.
//! Matriz diseñada test-first (fase 3 de M0); los casos hostiles byte-exactos
//! viven además como golden fixtures en `tests/golden/vpath/`.

use norte_proto::{Authority, Scheme, Segment, VPath, VPathError};

fn seg(bytes: &[u8]) -> Segment {
    Segment::new(bytes.to_vec()).expect("segmento válido de test")
}

fn path(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

// ---------- parse válidos ----------

#[test]
fn parse_minimal() {
    let p = path("file:///");
    assert_eq!(p.scheme(), "file");
    assert_eq!(p.authority(), None);
    assert_eq!(p.segments().count(), 0);
    assert!(p.is_root());
}

#[test]
fn parse_local_simple() {
    let p = path("file:///home/user");
    let segs: Vec<&[u8]> = p.segments().collect();
    assert_eq!(segs, vec![b"home".as_slice(), b"user".as_slice()]);
}

#[test]
fn parse_with_authority() {
    let p = path("sftp://host:22/dir/f.txt");
    assert_eq!(p.authority(), Some("host:22"));
    let segs: Vec<&[u8]> = p.segments().collect();
    assert_eq!(segs, vec![b"dir".as_slice(), b"f.txt".as_slice()]);
}

#[test]
fn parse_authority_root_without_slash() {
    // Forma leniente: `sftp://h` ≡ `sftp://h/`; canónica con slash.
    let p = path("sftp://h");
    assert!(p.is_root());
    assert_eq!(p.to_wire(), "sftp://h/");
}

#[test]
fn parse_scheme_charset() {
    let p = path("s3+v2.x-y://b/k");
    assert_eq!(p.scheme(), "s3+v2.x-y");
}

#[test]
fn parse_pct_space() {
    let p = path("file:///a%20b");
    assert_eq!(p.file_name().unwrap().as_bytes(), b"a b");
}

#[test]
fn parse_pct_percent() {
    let p = path("file:///50%25");
    assert_eq!(p.file_name().unwrap().as_bytes(), b"50%");
}

#[test]
fn parse_pct_non_utf8() {
    let p = path("file:///%FF%FE");
    assert_eq!(p.file_name().unwrap().as_bytes(), &[0xFF, 0xFE]);
}

#[test]
fn parse_pct_lenient_canonical() {
    // Decode leniente: %41 ≡ 'A' y hex minúscula ≡ mayúscula; to_wire canonicaliza.
    let p = path("file:///%41");
    assert_eq!(p.file_name().unwrap().as_bytes(), b"A");
    assert_eq!(p.to_wire(), "file:///A");
    let q = path("file:///%ff");
    assert_eq!(q.file_name().unwrap().as_bytes(), &[0xFF]);
    assert_eq!(q.to_wire(), "file:///%FF");
}

#[test]
fn parse_utf8_passthrough() {
    let p = path("file:///cañón");
    assert_eq!(p.file_name().unwrap().as_bytes(), "cañón".as_bytes());
    assert_eq!(p.to_wire(), "file:///cañón");
}

// ---------- parse inválidos ----------

fn expect_err(wire: &str, kind: VPathError) {
    assert_eq!(VPath::parse(wire).unwrap_err(), kind, "wire: {wire:?}");
}

#[test]
fn err_empty() {
    expect_err("", VPathError::MissingScheme);
}

#[test]
fn err_no_scheme() {
    expect_err("/abs/path", VPathError::MissingScheme);
}

#[test]
fn err_scheme_upper() {
    // Sin case-folding: el scheme es lowercase o no es.
    expect_err("FILE:///a", VPathError::InvalidScheme);
}

#[test]
fn err_scheme_digit_first() {
    expect_err("9p://a", VPathError::InvalidScheme);
}

#[test]
fn err_empty_segment() {
    expect_err("file:///a//b", VPathError::EmptySegment);
}

#[test]
fn err_trailing_slash() {
    // Forma estricta: sin ambigüedad dir/file. Solo la raíz lleva slash final.
    expect_err("file:///a/", VPathError::EmptySegment);
}

#[test]
fn err_dot() {
    expect_err("file:///a/./b", VPathError::DotSegment);
}

#[test]
fn err_dotdot() {
    expect_err("file:///a/../b", VPathError::DotSegment);
}

#[test]
fn err_encoded_dotdot() {
    // La invariante se valida POST-decode: %2E%2E no cuela un `..`.
    expect_err("file:///%2E%2E", VPathError::DotSegment);
}

#[test]
fn err_encoded_slash() {
    // Un escape no puede fabricar un separador.
    expect_err("file:///a%2Fb", VPathError::InvalidByte);
}

#[test]
fn err_encoded_nul() {
    expect_err("file:///a%00b", VPathError::NulByte);
}

#[test]
fn err_bad_escape_nonhex() {
    expect_err("file:///a%G1", VPathError::BadEscape);
}

#[test]
fn err_bad_escape_eof() {
    expect_err("file:///a%", VPathError::BadEscape);
}

#[test]
fn err_bad_escape_short() {
    expect_err("file:///a%4", VPathError::BadEscape);
}

#[test]
fn err_invalid_authority() {
    // La authority se valida en parse: espacio, control, no-ASCII y `%` fuera.
    expect_err("file://a b/c", VPathError::InvalidAuthority);
    expect_err("file://a\nb/c", VPathError::InvalidAuthority);
    expect_err("sftp://ñ/a", VPathError::InvalidAuthority);
    expect_err("file://h%41/a", VPathError::InvalidAuthority);
}

/// Un `:` en el userinfo (`user:pass@host`) = password inline: defensa RAÍZ
/// contra que el secreto acabe en config/logs (regla 10, #46, proto 0.8.0).
/// El `host:port` y el IPv6 con corchetes siguen siendo válidos.
#[test]
fn err_inline_password_en_authority() {
    use norte_proto::Authority;
    // Rechazados: `:` antes del `@`.
    assert!(Authority::new("user:pass@host").is_err());
    assert!(Authority::new("u:p@h:22").is_err());
    expect_err("sftp://oscar:hunter2@host/x", VPathError::InvalidAuthority);
    // Aceptados: `:` de puerto (con o sin usuario) y IPv6.
    assert!(Authority::new("host:22").is_ok());
    assert!(Authority::new("user@host:22").is_ok());
    assert!(Authority::new("[::1]:22").is_ok());
    assert!(Authority::new("user@[::1]:2222").is_ok());
}

// ---------- constructor de Segment ----------

#[test]
fn segment_rejects_invalid() {
    assert_eq!(Segment::new(vec![]).unwrap_err(), VPathError::EmptySegment);
    assert_eq!(
        Segment::new(b"a/b".to_vec()).unwrap_err(),
        VPathError::InvalidByte
    );
    assert_eq!(
        Segment::new(b"a\0b".to_vec()).unwrap_err(),
        VPathError::NulByte
    );
    assert_eq!(
        Segment::new(b".".to_vec()).unwrap_err(),
        VPathError::DotSegment
    );
    assert_eq!(
        Segment::new(b"..".to_vec()).unwrap_err(),
        VPathError::DotSegment
    );
}

// ---------- constructor de Authority ----------

#[test]
fn authority_newtype_validates() {
    assert!(Authority::new("host:22").is_ok());
    assert!(Authority::new("user@host").is_ok());
    assert!(Authority::new("[::1]:22").is_ok());
    for bad in ["", "a/b", "a b", "a%2Fb", "añ", "a\tb"] {
        assert_eq!(
            Authority::new(bad).unwrap_err(),
            VPathError::InvalidAuthority,
            "authority: {bad:?}"
        );
    }
}

// ---------- navegación ----------

#[test]
fn join_appends() {
    let p = path("file:///a").join(seg(b"b"));
    assert_eq!(p.to_wire(), "file:///a/b");
}

#[test]
fn parent_pops() {
    assert_eq!(path("file:///a/b").parent(), Some(path("file:///a")));
}

#[test]
fn parent_of_root() {
    assert_eq!(path("file:///").parent(), None);
}

#[test]
fn parent_keeps_authority() {
    assert_eq!(path("sftp://h/a").parent(), Some(path("sftp://h/")));
}

#[test]
fn file_name_last() {
    assert_eq!(
        path("file:///a/b.txt").file_name().unwrap().as_bytes(),
        b"b.txt"
    );
}

#[test]
fn file_name_root() {
    assert_eq!(path("file:///").file_name(), None);
}

#[test]
fn with_file_name_replaces() {
    let p = path("file:///a/x").with_file_name(seg(b"y")).unwrap();
    assert_eq!(p.to_wire(), "file:///a/y");
}

#[test]
fn with_file_name_on_root_is_none() {
    assert_eq!(path("file:///").with_file_name(seg(b"y")), None);
}

#[test]
fn long_path_no_limit() {
    // VPath no impone límites de longitud: eso es capability del provider.
    let mut p = path("file:///");
    for _ in 0..300 {
        p = p.join(seg(b"x"));
    }
    assert_eq!(p.segments().count(), 300);
    let big = seg(&[b'y'; 4096]);
    assert_eq!(
        path("file:///")
            .join(big)
            .file_name()
            .unwrap()
            .as_bytes()
            .len(),
        4096
    );
}

// ---------- controles en el wire ----------

#[test]
fn wire_escapes_controls() {
    // C0 y DEL jamás viajan crudos aunque sean UTF-8 válido (ADR 0001).
    let p = path("file:///").join(seg(b"a\nb"));
    assert_eq!(p.to_wire(), "file:///a%0Ab");
    let q = path("file:///a%0Ab");
    assert_eq!(q.file_name().unwrap().as_bytes(), b"a\nb");
    assert_eq!(path("file:///").join(seg(&[0x7F])).to_wire(), "file:///%7F");
    assert_eq!(
        path("file:///").join(seg(b"\x1b]0;pwned\x07")).to_wire(),
        "file:///%1B]0;pwned%07"
    );
}

// ---------- display / serde / orden ----------

#[test]
fn display_utf8_identity() {
    let p = path("file:///home/cañón");
    assert!(!p.display_lossy().contains('\u{FFFD}'));
    assert_eq!(p.display_lossy(), "⟨file⟩/home/cañón");
}

#[test]
fn display_with_authority() {
    assert_eq!(
        path("sftp://host:22/dir/f.txt").display_lossy(),
        "⟨sftp host:22⟩/dir/f.txt"
    );
}

#[test]
fn display_lossy_marked() {
    let p = path("file:///").join(seg(&[0xFF]));
    assert!(p.display_lossy().contains('\u{FFFD}'));
}

#[test]
fn display_masks_controls() {
    // Controles (válidos como bytes de nombre) jamás llegan crudos al terminal.
    let p = path("file:///").join(seg(b"a\x1b]0;x\x07b"));
    let d = p.display_lossy();
    assert!(
        !d.chars().any(char::is_control),
        "display con controles: {d:?}"
    );
    assert!(d.contains('\u{FFFD}'));
}

#[test]
fn display_never_reparses() {
    // El display no tiene forma wire: parse(display) falla SIEMPRE (ADR 0001).
    for w in [
        "file:///",
        "sftp://h:22/a/b",
        "file:///50%25",
        "file:///%FF",
    ] {
        let d = path(w).display_lossy();
        assert!(VPath::parse(&d).is_err(), "display reparseó: {d}");
    }
}

#[test]
fn nfc_nfd_distinct() {
    // macOS normaliza a NFD; VPath jamás: NFC y NFD son paths DISTINTOS.
    let nfc = path("file:///caf\u{e9}");
    let nfd = path("file:///cafe\u{301}");
    assert_ne!(nfc, nfd);
    assert_ne!(nfc.to_wire(), nfd.to_wire());
}

#[test]
fn serde_ser_is_wire() {
    let p = path("file:///a/50%25");
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(json, format!("\"{}\"", p.to_wire()));
}

#[test]
fn serde_roundtrip() {
    let p = path("file:///a").join(seg(&[0xED, 0xA0, 0x80]));
    let json = serde_json::to_string(&p).unwrap();
    let q: VPath = serde_json::from_str(&json).unwrap();
    assert_eq!(p, q);
}

#[test]
fn serde_de_invalid_errors() {
    assert!(serde_json::from_str::<VPath>("\"file:///a%\"").is_err());
}

#[test]
fn eq_by_bytes_not_by_wire() {
    assert_eq!(path("file:///%41"), path("file:///A"));
}

#[test]
fn ord_total() {
    assert!(path("file:///a") < path("file:///b"));
    assert!(path("file:///b") < path("sftp://h/a"));
}

#[test]
fn hash_consistent_with_eq() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let h = |p: &VPath| {
        let mut s = DefaultHasher::new();
        p.hash(&mut s);
        s.finish()
    };
    assert_eq!(h(&path("file:///%41")), h(&path("file:///A")));
}

#[test]
fn scheme_newtype_validates() {
    assert!(Scheme::new("file").is_ok());
    assert_eq!(Scheme::new("FILE").unwrap_err(), VPathError::InvalidScheme);
    assert_eq!(Scheme::new("").unwrap_err(), VPathError::InvalidScheme);
}

// ---------- archive_compose / archive_split (ADR 0018) ----------

#[test]
fn archive_compose_basico() {
    let outer = path("file:///home/o/a.zip");
    let root = VPath::archive_compose("zip", &outer, &[]).expect("compose raíz");
    assert_eq!(root.to_wire(), "zip+file:///home/o/a.zip/!");
    let hijo = VPath::archive_compose("zip", &outer, &[seg(b"docs"), seg(b"x.txt")])
        .expect("compose con interior");
    assert_eq!(hijo.to_wire(), "zip+file:///home/o/a.zip/!/docs/x.txt");
}

#[test]
fn archive_compose_preserva_authority() {
    let outer = path("sftp://user@host:22/d/a.tar");
    let p = VPath::archive_compose("tar", &outer, &[seg(b"x")]).expect("compose");
    assert_eq!(p.to_wire(), "tar+sftp://user@host:22/d/a.tar/!/x");
    assert_eq!(p.authority(), Some("user@host:22"));
}

#[test]
fn archive_compose_rechaza_formato_desconocido() {
    let outer = path("file:///a.rar");
    assert!(VPath::archive_compose("rar", &outer, &[]).is_err());
    assert!(VPath::archive_compose("", &outer, &[]).is_err());
}

#[test]
fn archive_compose_rechaza_exterior_ya_compuesto() {
    // v1 una capa (ADR 0018): componer sobre un path ya compuesto = anidar.
    let outer = path("file:///a.zip");
    let composed = VPath::archive_compose("zip", &outer, &[]).expect("capa 1");
    assert!(VPath::archive_compose("tar", &composed, &[]).is_err());
}

#[test]
fn archive_compose_rechaza_marcador_en_ambos_lados() {
    // Exterior con segmento `!`: ese archivo no es direccionable (ADR 0018).
    let outer = path("file:///dir/!/a.zip");
    assert!(VPath::archive_compose("zip", &outer, &[]).is_err());
    // Interior con `!`: compose jamás fabrica paths que el índice omite.
    let ok = path("file:///a.zip");
    assert!(VPath::archive_compose("zip", &ok, &[seg(b"!")]).is_err());
}

#[test]
fn archive_split_roundtrip() {
    let outer = path("sftp://host/d/a.zip");
    let inner = [seg(b"sub"), seg(b"f.bin")];
    let p = VPath::archive_compose("zip", &outer, &inner).expect("compose");
    let r = p
        .archive_split()
        .expect("split bien formado")
        .expect("es compuesto");
    assert_eq!(r.format, "zip");
    assert_eq!(r.outer, outer);
    assert_eq!(r.inner, inner.to_vec());
}

#[test]
fn archive_split_scheme_plano_es_none() {
    assert!(path("file:///a.zip").archive_split().expect("ok").is_none());
    // `s3+v2.x-y` lleva `+` pero `s3` NO es formato: scheme de provider
    // legítimo (pinneado en goldens), no compuesto.
    assert!(
        path("s3+v2.x-y://bucket/key")
            .archive_split()
            .expect("ok")
            .is_none()
    );
}

#[test]
fn archive_split_sin_marcador_es_err() {
    assert!(path("zip+file:///a.zip").archive_split().is_err());
}

#[test]
fn archive_split_anidado_es_err_v1() {
    assert!(
        path("zip+tar+file:///a.tar/!/i.zip/!/x")
            .archive_split()
            .is_err()
    );
}

#[test]
fn archive_split_marcador_extra_interior_tolerado() {
    // Corta en el PRIMER `!`; los `!` extra van al interior (el índice del
    // provider jamás los contiene → NotFound aguas abajo, ADR 0018).
    let r = path("zip+file:///a.zip/!/x/!/y")
        .archive_split()
        .expect("ok")
        .expect("compuesto");
    assert_eq!(r.outer.to_wire(), "file:///a.zip");
    assert_eq!(r.inner, vec![seg(b"x"), seg(b"!"), seg(b"y")]);
}

#[test]
fn archive_split_raiz_exterior_tolerada() {
    // Sintácticamente válido; el provider dará TypeMismatch (la raíz no es
    // un archivo). El split no hace semántica.
    let r = path("zip+file:///!")
        .archive_split()
        .expect("ok")
        .expect("compuesto");
    assert!(r.outer.is_root());
    assert!(r.inner.is_empty());
}

#[test]
fn archive_sobre_provider_con_mas_en_el_scheme() {
    // El caso que motivó la whitelist (ADR 0018): componer sobre un provider
    // cuyo scheme legítimo lleva `+` — la descomposición corta en el PRIMER
    // `+` y devuelve el scheme del provider intacto.
    let outer = path("s3+v2.x-y://bucket/key.zip");
    let p = VPath::archive_compose("zip", &outer, &[seg(b"x")]).expect("compose");
    assert_eq!(p.to_wire(), "zip+s3+v2.x-y://bucket/key.zip/!/x");
    let r = p.archive_split().expect("ok").expect("compuesto");
    assert_eq!(r.format, "zip");
    assert_eq!(r.outer, outer);
    assert_eq!(r.outer.scheme(), "s3+v2.x-y");
}

#[test]
fn archive_split_scheme_interior_vacio_es_err() {
    // `zip+` es un Scheme válido gramaticalmente; el split lo rechaza al
    // reconstruir el scheme interior vacío.
    assert_eq!(
        path("zip+:///x/!").archive_split().unwrap_err(),
        VPathError::InvalidScheme
    );
}

// ---------- tar+gz: token compuesto, longest-match (ADR 0028, #55) ----------

#[test]
fn targz_compose_y_split_roundtrip() {
    let outer = path("file:///home/o/a.tgz");
    let p = VPath::archive_compose("tar+gz", &outer, &[seg(b"x")]).expect("compose tar+gz");
    assert_eq!(p.to_wire(), "tar+gz+file:///home/o/a.tgz/!/x");
    let r = p
        .archive_split()
        .expect("split bien formado")
        .expect("es compuesto");
    assert_eq!(r.format, "tar+gz");
    assert_eq!(r.outer, outer);
    assert_eq!(r.outer.scheme(), "file");
    assert_eq!(r.inner, vec![seg(b"x")]);
}

#[test]
fn targz_no_confunde_prefijo_tar() {
    // Longest-match: NO debe leerse como formato `tar` con un interior
    // huérfano `gz+file` (lo que daría `split_once('+')`).
    let r = path("tar+gz+file:///a.tgz/!/x")
        .archive_split()
        .expect("split bien formado")
        .expect("es compuesto");
    assert_eq!(r.format, "tar+gz");
    assert_ne!(r.format, "tar");
    assert_eq!(r.outer.scheme(), "file");
    assert_eq!(r.outer.to_wire(), "file:///a.tgz");
}

#[test]
fn gz_solo_no_es_compuesto() {
    // `gz` no es un formato registrado (solo existe como sufijo de `tar+gz`):
    // un scheme `gz+file` es, sintácticamente, un provider legítimo no compuesto.
    assert!(path("gz+file:///x").archive_split().expect("ok").is_none());
}

#[test]
fn targz_interior_compuesto_rechazado() {
    // La guardia de anidamiento (v1 = una capa) sigue vigente sobre el
    // interior UNA VEZ quitado el token completo `tar+gz`.
    assert!(
        path("tar+gz+tar+file:///a.tar/!/x")
            .archive_split()
            .is_err()
    );
    assert!(
        path("tar+gz+zip+file:///a.zip/!/x")
            .archive_split()
            .is_err()
    );
}

#[test]
fn compose_outer_compuesto_sigue_rechazado() {
    let outer = path("file:///a.tgz");
    let composed = VPath::archive_compose("tar+gz", &outer, &[]).expect("capa 1");
    assert!(VPath::archive_compose("zip", &composed, &[]).is_err());
}

#[test]
fn compose_outer_compuesto_sigue_rechazado_simetrico() {
    // Caso simétrico al anterior: componer `tar+gz` sobre un outer ya
    // compuesto por `zip` también se rechaza (misma línea de guardia,
    // ejercitada con el formato compuesto en el rol de exterior/interior
    // invertido respecto al test de arriba).
    let outer = path("file:///a.zip");
    let composed = VPath::archive_compose("zip", &outer, &[]).expect("capa 1");
    assert!(VPath::archive_compose("tar+gz", &composed, &[]).is_err());
}

#[test]
fn archive_compose_rechaza_roundtrip_ambiguo_con_tar_gz() {
    // BLOCKER hallado por protocol-guardian: `outer` con scheme "gz+mem" NO
    // es compuesto por sí mismo (`gz` solo no es un formato registrado), así
    // que la guardia de anidamiento existente lo deja pasar. Pero componer
    // "tar" sobre él formaría el scheme "tar+gz+mem", que
    // `scheme_format_prefix` (longest-match) resuelve como formato "tar+gz"
    // sobre "mem" — NO como "tar" sobre "gz+mem". Esto rompería la garantía
    // de roundtrip del rustdoc de `archive_compose` si se permitiera: debe
    // rechazarse.
    let outer = path("gz+mem:///x");
    assert!(
        VPath::archive_compose("tar", &outer, &[]).is_err(),
        "compose debe rechazar la ambigüedad tar+(gz+mem) vs (tar+gz)+mem"
    );
}

#[test]
fn display_lossy_masks_bidi_override() {
    // U+202E (RIGHT-TO-LEFT OVERRIDE, bytes E2 80 AE) NO es is_control pero
    // permite spoofing visual del nombre (issue #21): debe ir a `�`.
    let p = path("file:///factura%E2%80%AEgpj.exe");
    let shown = p.display_lossy();
    assert!(
        !shown.contains('\u{202E}'),
        "el override RTL crudo no debe llegar al display: {shown:?}"
    );
    assert!(shown.contains('\u{FFFD}'), "se marca con �: {shown:?}");
    // El texto visible sigue ahí (solo el formateador se enmascara).
    assert!(shown.contains("factura") && shown.contains("gpj.exe"));
}

#[test]
fn display_lossy_masks_isolates_but_not_zwj() {
    // Los aisladores bidi (U+2066 LRI) se enmascaran…
    let p = path("file:///a%E2%81%A6b");
    let shown = p.display_lossy();
    assert!(!shown.contains('\u{2066}'));
    assert!(shown.contains('\u{FFFD}'));

    // …pero el zero-width joiner (U+200D) NO: es legítimo en emoji/escrituras.
    let emoji = path("file:///a%E2%80%8Db");
    let shown = emoji.display_lossy();
    assert!(shown.contains('\u{200D}'), "el ZWJ legítimo se conserva");
}
