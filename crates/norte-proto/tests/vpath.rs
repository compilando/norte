//! Deterministic `VPath` tests: parse, navigation, display, serde, ordering.
//! Matrix designed test-first (M0 phase 3); the byte-exact hostile cases also
//! live as golden fixtures in `tests/golden/vpath/`.

use norte_proto::{Authority, Scheme, Segment, VPath, VPathError};

fn seg(bytes: &[u8]) -> Segment {
    Segment::new(bytes.to_vec()).expect("valid test segment")
}

fn path(wire: &str) -> VPath {
    VPath::parse(wire).expect("valid test wire")
}

// ---------- valid parses ----------

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
    // Lenient form: `sftp://h` ≡ `sftp://h/`; canonical with the slash.
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
    // Lenient decode: %41 ≡ 'A' and lowercase hex ≡ uppercase; to_wire canonicalizes.
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

// ---------- invalid parses ----------

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
    // No case-folding: the scheme is lowercase or it is not.
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
    // Strict form: no dir/file ambiguity. Only the root carries a trailing slash.
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
    // The invariant is validated POST-decode: %2E%2E does not smuggle in a `..`.
    expect_err("file:///%2E%2E", VPathError::DotSegment);
}

#[test]
fn err_encoded_slash() {
    // An escape cannot fabricate a separator.
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
    // The authority is validated at parse: space, control, non-ASCII and `%` outside.
    expect_err("file://a b/c", VPathError::InvalidAuthority);
    expect_err("file://a\nb/c", VPathError::InvalidAuthority);
    expect_err("sftp://ñ/a", VPathError::InvalidAuthority);
    expect_err("file://h%41/a", VPathError::InvalidAuthority);
}

/// A `:` in the userinfo (`user:pass@host`) = inline password: ROOT defense
/// against the secret ending up in config/logs (rule 10, #46, proto 0.8.0).
/// `host:port` and bracketed IPv6 stay valid.
#[test]
fn err_inline_password_in_authority() {
    use norte_proto::Authority;
    // Rejected: `:` before the `@`.
    assert!(Authority::new("user:pass@host").is_err());
    assert!(Authority::new("u:p@h:22").is_err());
    expect_err("sftp://oscar:hunter2@host/x", VPathError::InvalidAuthority);
    // Accepted: port `:` (with or without a user) and IPv6.
    assert!(Authority::new("host:22").is_ok());
    assert!(Authority::new("user@host:22").is_ok());
    assert!(Authority::new("[::1]:22").is_ok());
    assert!(Authority::new("user@[::1]:2222").is_ok());
}

// ---------- Segment constructor ----------

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

// ---------- Segment serde ----------

/// A `Segment` crosses the wire as ONE percent-encoded string, with the same
/// codec `VPath` uses, and non-UTF-8 bytes survive the round trip (rule 1).
#[test]
fn segment_round_trips_non_utf8_bytes_as_percent_encoded_string() {
    let raw = b"caf\xff\xfe.txt".to_vec();
    let s = seg(&raw);
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(json, "\"caf%FF%FE.txt\"");
    let back: Segment = serde_json::from_str(&json).unwrap();
    assert_eq!(back.as_bytes(), &raw[..]);
}

/// A name that already LOOKS like a percent-escape (`%41` ≡ `A`) is not a
/// silent-corruption trap: `Segment::new` takes the bytes literally (this is
/// a filename that contains a `%`, not an escape), so the codec must escape
/// that literal `%` on encode (`%` -> `%25`) and decode it back to the SAME
/// bytes — never to `aAb`, which is what a codec that forgot to escape the
/// literal `%` would silently produce (mirrors `serde_ser_is_wire`'s
/// `50%25` case for `VPath`, above).
#[test]
fn segment_round_trips_a_name_that_looks_like_an_escape() {
    let s = seg(b"a%41b");
    let json = serde_json::to_string(&s).unwrap();
    assert_eq!(json, "\"a%2541b\"");
    let back: Segment = serde_json::from_str(&json).unwrap();
    assert_eq!(back.as_bytes(), b"a%41b");
}

/// The WTF-8 lone-surrogate shape (Windows, `OsStr::as_encoded_bytes`, see
/// the module header) round trips like any other non-UTF-8 byte sequence.
#[test]
fn segment_serde_roundtrip_wtf8_lone_surrogate() {
    let s = seg(&[0xED, 0xA0, 0x80]);
    let json = serde_json::to_string(&s).unwrap();
    let back: Segment = serde_json::from_str(&json).unwrap();
    assert_eq!(s, back);
}

/// A malformed escape is a deserialization ERROR, never a lossy salvage.
#[test]
fn segment_rejects_a_malformed_percent_escape() {
    let err = serde_json::from_str::<Segment>("\"a%G1\"").unwrap_err();
    assert!(
        err.to_string().contains("malformed percent escape"),
        "unexpected error: {err}",
    );
}

/// A segment that `Segment::new` would refuse (a `/`, a NUL, `.`, `..`, the
/// empty string) is refused on the wire too — including the ESCAPED spelling
/// of a dot-segment (`%2E%2E`), which is the actual traversal-bypass attempt
/// (mirrors `err_encoded_dotdot` for `VPath`, above). The invariant is not
/// bypassable by deserializing, and asserting on the message rules out
/// rejection coming from the wrong stage (decode vs. `Segment::new`).
#[test]
fn segment_rejects_wire_forms_that_break_its_invariant() {
    for (wire, expect) in [
        ("\"a%2Fb\"", "invalid byte in segment"),
        ("\"a%00b\"", "NUL byte in segment"),
        ("\"..\"", "dot segment"),
        ("\"%2E%2E\"", "dot segment"),
        ("\".\"", "dot segment"),
        ("\"\"", "empty path segment"),
    ] {
        let err = serde_json::from_str::<Segment>(wire).unwrap_err();
        assert!(
            err.to_string().contains(expect),
            "{wire} should be rejected with a message containing {expect:?}, got {err}",
        );
    }
}

// ---------- Authority constructor ----------

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

// ---------- navigation ----------

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
    // VPath imposes no length limits: that is a provider capability.
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

// ---------- controls on the wire ----------

#[test]
fn wire_escapes_controls() {
    // C0 and DEL never travel raw even if they are valid UTF-8 (ADR 0001).
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

// ---------- display / serde / ordering ----------

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
    // Controls (valid as name bytes) never reach the terminal raw.
    let p = path("file:///").join(seg(b"a\x1b]0;x\x07b"));
    let d = p.display_lossy();
    assert!(
        !d.chars().any(char::is_control),
        "display with controls: {d:?}"
    );
    assert!(d.contains('\u{FFFD}'));
}

#[test]
fn display_never_reparses() {
    // The display has no wire form: parse(display) ALWAYS fails (ADR 0001).
    for w in [
        "file:///",
        "sftp://h:22/a/b",
        "file:///50%25",
        "file:///%FF",
    ] {
        let d = path(w).display_lossy();
        assert!(VPath::parse(&d).is_err(), "display re-parsed: {d}");
    }
}

#[test]
fn nfc_nfd_distinct() {
    // macOS normalizes to NFD; VPath never does: NFC and NFD are DIFFERENT paths.
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
fn archive_compose_basic() {
    let outer = path("file:///home/o/a.zip");
    let root = VPath::archive_compose("zip", &outer, &[]).expect("root compose");
    assert_eq!(root.to_wire(), "zip+file:///home/o/a.zip/!");
    let child = VPath::archive_compose("zip", &outer, &[seg(b"docs"), seg(b"x.txt")])
        .expect("compose with an interior");
    assert_eq!(child.to_wire(), "zip+file:///home/o/a.zip/!/docs/x.txt");
}

#[test]
fn archive_compose_preserves_authority() {
    let outer = path("sftp://user@host:22/d/a.tar");
    let p = VPath::archive_compose("tar", &outer, &[seg(b"x")]).expect("compose");
    assert_eq!(p.to_wire(), "tar+sftp://user@host:22/d/a.tar/!/x");
    assert_eq!(p.authority(), Some("user@host:22"));
}

#[test]
fn archive_compose_rejects_unknown_format() {
    let outer = path("file:///a.7z");
    // `7z` is NOT in the whitelist; `rar` has been since 0.47.0 (roadmap item
    // 11), so the unknown-format example had to change token.
    assert!(VPath::archive_compose("7z", &outer, &[]).is_err());
    assert!(VPath::archive_compose("", &outer, &[]).is_err());
}

/// `rar` has been an archive format since 0.47.0: it composes and splits like
/// any other, and the outer part comes out intact.
#[test]
fn rar_is_an_archive_format() {
    let p = VPath::parse("rar+file:///a.rar/!/x.txt").unwrap();
    let r = p.archive_split().unwrap().unwrap();
    assert_eq!(r.format, "rar");
    assert_eq!(r.outer.to_wire(), "file:///a.rar");
    assert_eq!(r.inner[0].as_bytes(), b"x.txt");
}

#[test]
fn archive_compose_accepts_a_well_formed_compound_outer() {
    // #56 (v1 used to reject this): composing over a path that is ALREADY
    // compound and well-formed = nesting one more layer.
    let outer = path("file:///a.zip");
    let composed = VPath::archive_compose("zip", &outer, &[seg(b"i.tar")]).expect("layer 1");
    let nested = VPath::archive_compose("tar", &composed, &[]).expect("nested layer 2");
    assert_eq!(nested.to_wire(), "tar+zip+file:///a.zip/!/i.tar/!");
}

#[test]
fn archive_compose_rejects_a_marker_on_either_side() {
    // Outer with a `!` segment: that file is not addressable (ADR 0018).
    let outer = path("file:///dir/!/a.zip");
    assert!(VPath::archive_compose("zip", &outer, &[]).is_err());
    // Inner with `!`: compose never fabricates paths the index omits.
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
        .expect("well-formed split")
        .expect("is compound");
    assert_eq!(r.format, "zip");
    assert_eq!(r.outer, outer);
    assert_eq!(r.inner, inner.to_vec());
}

#[test]
fn archive_split_flat_scheme_is_none() {
    assert!(path("file:///a.zip").archive_split().expect("ok").is_none());
    // `s3+v2.x-y` carries a `+` but `s3` is NOT a format: a legitimate
    // provider scheme (pinned in goldens), not a compound one.
    assert!(
        path("s3+v2.x-y://bucket/key")
            .archive_split()
            .expect("ok")
            .is_none()
    );
}

#[test]
fn archive_split_no_marker_is_err() {
    assert!(path("zip+file:///a.zip").archive_split().is_err());
}

#[test]
fn archive_split_nested_peels_one_layer() {
    // #56 (ADR 0018 A3): right-to-left resolution — the OUTERMOST layer
    // (leftmost format) cuts at the LAST marker; the resulting outer part is
    // itself an archive path (peeled recursively).
    let r = path("zip+tar+file:///a.tar/!/i.zip/!/x")
        .archive_split()
        .expect("ok")
        .expect("compound");
    assert_eq!(r.format, "zip");
    assert_eq!(r.outer.to_wire(), "tar+file:///a.tar/!/i.zip");
    assert_eq!(r.inner, vec![seg(b"x")]);
    // The inner layer is peeled with the v1 rule (flat interior = FIRST marker).
    let r2 = r.outer.archive_split().expect("ok").expect("compound");
    assert_eq!(r2.format, "tar");
    assert_eq!(r2.outer.to_wire(), "file:///a.tar");
    assert_eq!(r2.inner, vec![seg(b"i.zip")]);
}

#[test]
fn archive_split_nested_rogue_marker_goes_to_the_deep_layer_s_interior() {
    // Three markers in a two-layer path: the zip layer takes the LAST one,
    // the tar layer (FLAT interior) cuts at the FIRST — the leftover `!`
    // stays as a segment of tar's interior (its index never contains it →
    // NotFound downstream, it never addresses a real object).
    let r = path("zip+tar+file:///a.tar/!/i.zip/!/x/!/rogue")
        .archive_split()
        .expect("ok")
        .expect("compound");
    assert_eq!(r.inner, vec![seg(b"rogue")]);
    let r2 = r.outer.archive_split().expect("ok").expect("compound");
    assert_eq!(r2.outer.to_wire(), "file:///a.tar");
    assert_eq!(r2.inner, vec![seg(b"i.zip"), seg(b"!"), seg(b"x")]);
}

#[test]
fn archive_compose_nested_roundtrip() {
    // #56: composing OVER a well-formed archive path is legal; the roundtrip
    // peels layer by layer exactly what was composed.
    let outer = path("file:///a.tar");
    let layer1 = VPath::archive_compose("tar", &outer, &[seg(b"i.zip")]).expect("tar layer");
    let layer2 =
        VPath::archive_compose("zip", &layer1, &[seg(b"docs"), seg(b"x.txt")]).expect("zip layer");
    assert_eq!(
        layer2.to_wire(),
        "zip+tar+file:///a.tar/!/i.zip/!/docs/x.txt"
    );
    let r = layer2.archive_split().expect("ok").expect("compound");
    assert_eq!(r.format, "zip");
    assert_eq!(r.outer, layer1);
    assert_eq!(r.inner, vec![seg(b"docs"), seg(b"x.txt")]);
}

#[test]
fn archive_compose_still_rejects_a_marker_on_a_flat_outer() {
    // #56's relaxation is ONLY for outers that are themselves well-formed
    // archive paths: a FLAT outer with a `!` is still forbidden.
    let outer = path("file:///a.zip/!/x");
    assert!(VPath::archive_compose("zip", &outer, &[]).is_err());
}

#[test]
fn archive_split_tolerates_an_extra_inner_marker() {
    // Cuts at the FIRST `!`; the extra `!`s go to the interior (the
    // provider's index never contains them → NotFound downstream, ADR 0018).
    let r = path("zip+file:///a.zip/!/x/!/y")
        .archive_split()
        .expect("ok")
        .expect("compound");
    assert_eq!(r.outer.to_wire(), "file:///a.zip");
    assert_eq!(r.inner, vec![seg(b"x"), seg(b"!"), seg(b"y")]);
}

#[test]
fn archive_split_tolerates_a_root_outer() {
    // Syntactically valid; the provider will give TypeMismatch (the root is
    // not an archive). Split does no semantics.
    let r = path("zip+file:///!")
        .archive_split()
        .expect("ok")
        .expect("compound");
    assert!(r.outer.is_root());
    assert!(r.inner.is_empty());
}

#[test]
fn archive_over_a_provider_with_a_plus_in_its_scheme() {
    // The case that motivated the whitelist (ADR 0018): composing over a
    // provider whose legitimate scheme carries a `+` — decomposition cuts at
    // the FIRST `+` and returns the provider's scheme intact.
    let outer = path("s3+v2.x-y://bucket/key.zip");
    let p = VPath::archive_compose("zip", &outer, &[seg(b"x")]).expect("compose");
    assert_eq!(p.to_wire(), "zip+s3+v2.x-y://bucket/key.zip/!/x");
    let r = p.archive_split().expect("ok").expect("compound");
    assert_eq!(r.format, "zip");
    assert_eq!(r.outer, outer);
    assert_eq!(r.outer.scheme(), "s3+v2.x-y");
}

#[test]
fn archive_split_empty_inner_scheme_is_err() {
    // `zip+` is a grammatically valid Scheme; split rejects it when
    // reconstructing the empty inner scheme.
    assert_eq!(
        path("zip+:///x/!").archive_split().unwrap_err(),
        VPathError::InvalidScheme
    );
}

// ---------- tar+gz: compound token, longest-match (ADR 0028, #55) ----------

#[test]
fn targz_compose_and_split_roundtrip() {
    let outer = path("file:///home/o/a.tgz");
    let p = VPath::archive_compose("tar+gz", &outer, &[seg(b"x")]).expect("tar+gz compose");
    assert_eq!(p.to_wire(), "tar+gz+file:///home/o/a.tgz/!/x");
    let r = p
        .archive_split()
        .expect("well-formed split")
        .expect("is compound");
    assert_eq!(r.format, "tar+gz");
    assert_eq!(r.outer, outer);
    assert_eq!(r.outer.scheme(), "file");
    assert_eq!(r.inner, vec![seg(b"x")]);
}

#[test]
fn targz_is_not_confused_with_a_tar_prefix() {
    // Longest-match: must NOT be read as format `tar` with an orphan
    // `gz+file` interior (what `split_once('+')` would give).
    let r = path("tar+gz+file:///a.tgz/!/x")
        .archive_split()
        .expect("well-formed split")
        .expect("is compound");
    assert_eq!(r.format, "tar+gz");
    assert_ne!(r.format, "tar");
    assert_eq!(r.outer.scheme(), "file");
    assert_eq!(r.outer.to_wire(), "file:///a.tgz");
}

#[test]
fn gz_alone_is_not_compound() {
    // `gz` is not a registered format (it only exists as `tar+gz`'s suffix):
    // a `gz+file` scheme is, syntactically, a legitimate non-compound provider.
    assert!(path("gz+file:///x").archive_split().expect("ok").is_none());
}

#[test]
fn targz_compound_interior_peels_as_the_outer_layer() {
    // #56 (v1 used to reject this): the whole `tar+gz` token is removed first
    // (longest-match) and the compound interior is peeled recursively. With a
    // SINGLE marker, this layer takes it whole: the outer part is left WITHOUT
    // a marker and its own split fails downstream (an honest malformed case).
    let r = path("tar+gz+tar+file:///a.tar/!/x")
        .archive_split()
        .expect("ok")
        .expect("compound");
    assert_eq!(r.format, "tar+gz");
    assert_eq!(r.outer.to_wire(), "tar+file:///a.tar");
    assert_eq!(r.inner, vec![seg(b"x")]);
    assert!(
        r.outer.archive_split().is_err(),
        "deep layer with no marker"
    );
    let r = path("tar+gz+zip+file:///a.zip/!/x")
        .archive_split()
        .expect("ok")
        .expect("compound");
    assert_eq!(r.format, "tar+gz");
    assert_eq!(r.outer.scheme(), "zip+file");
}

#[test]
fn compose_zip_over_targz_nests() {
    // #56 (v1 used to reject this): zip over tar.gz = two layers.
    let outer = path("file:///a.tgz");
    let composed = VPath::archive_compose("tar+gz", &outer, &[seg(b"i.zip")]).expect("layer 1");
    let nested = VPath::archive_compose("zip", &composed, &[seg(b"f")]).expect("layer 2");
    assert_eq!(nested.to_wire(), "zip+tar+gz+file:///a.tgz/!/i.zip/!/f");
    let r = nested.archive_split().expect("ok").expect("compound");
    assert_eq!(r.format, "zip");
    assert_eq!(r.outer, composed);
}

#[test]
fn compose_targz_over_zip_nests_symmetrically() {
    // #56, symmetric case: `tar+gz` over an outer composed with `zip` nests —
    // and split's longest-match returns `tar+gz`, never `tar` over an orphan
    // `gz+zip+file` interior (the roundtrip guard guarantees it).
    let outer = path("file:///a.zip");
    let composed = VPath::archive_compose("zip", &outer, &[seg(b"i.tgz")]).expect("layer 1");
    let nested = VPath::archive_compose("tar+gz", &composed, &[]).expect("layer 2");
    let r = nested.archive_split().expect("ok").expect("compound");
    assert_eq!(r.format, "tar+gz");
    assert_eq!(r.outer, composed);
}

#[test]
fn archive_compose_rejects_an_ambiguous_roundtrip_with_tar_gz() {
    // BLOCKER found by protocol-guardian: an `outer` with scheme "gz+mem" is
    // NOT compound by itself (`gz` alone is not a registered format), so the
    // existing nesting guard lets it through. But composing "tar" over it
    // would form the scheme "tar+gz+mem", which `scheme_format_prefix`
    // (longest-match) resolves as format "tar+gz" over "mem" — NOT as "tar"
    // over "gz+mem". This would break `archive_compose`'s rustdoc's roundtrip
    // guarantee if allowed: it must be rejected.
    let outer = path("gz+mem:///x");
    assert!(
        VPath::archive_compose("tar", &outer, &[]).is_err(),
        "compose must reject the tar+(gz+mem) vs (tar+gz)+mem ambiguity"
    );
}

#[test]
fn display_lossy_masks_bidi_override() {
    // U+202E (RIGHT-TO-LEFT OVERRIDE, bytes E2 80 AE) is NOT is_control but
    // allows visually spoofing the name (issue #21): it must become `�`.
    let p = path("file:///factura%E2%80%AEgpj.exe");
    let shown = p.display_lossy();
    assert!(
        !shown.contains('\u{202E}'),
        "the raw RTL override must not reach the display: {shown:?}"
    );
    assert!(shown.contains('\u{FFFD}'), "marked with �: {shown:?}");
    // The visible text is still there (only the formatter is masked).
    assert!(shown.contains("factura") && shown.contains("gpj.exe"));
}

#[test]
fn display_lossy_masks_isolates_but_not_zwj() {
    // Bidi isolates (U+2066 LRI) are masked…
    let p = path("file:///a%E2%81%A6b");
    let shown = p.display_lossy();
    assert!(!shown.contains('\u{2066}'));
    assert!(shown.contains('\u{FFFD}'));

    // …but the zero-width joiner (U+200D) is NOT: it is legitimate in emoji/scripts.
    let emoji = path("file:///a%E2%80%8Db");
    let shown = emoji.display_lossy();
    assert!(
        shown.contains('\u{200D}'),
        "the legitimate ZWJ is preserved"
    );
}
