//! The pure `norte_vfs::trash` module against the canonical hostile corpus
//! (CLAUDE.md rule: EVERY crate that touches paths is tested against the
//! corpus). Pins that trash path construction and the `.norte-info`
//! roundtrip are byte-exact and line-safe for all 19 names — a silent-loss
//! regression in the wire codec would go red here.

use norte_proto::{Segment, VPath};
use norte_testkit::corpus;
use norte_vfs::trash;

#[test]
fn trash_roundtrips_full_hostile_corpus() {
    let root = VPath::parse("sftp://host/").expect("valid root");

    for n in corpus::hostile_names() {
        let seg = Segment::new(n.bytes.clone())
            .unwrap_or_else(|_| panic!("corpus {} must be a valid Segment", n.id));
        let victim = root.join(seg.clone());

        // (a) plan() preserves the byte-exact basename under `.norte-trash/<id>/`.
        let paths =
            trash::plan(&victim, "1726000000123-0").unwrap_or_else(|_| panic!("plan {}", n.id));
        assert_eq!(
            paths
                .payload
                .file_name()
                .expect("payload has a basename")
                .as_bytes(),
            n.bytes.as_slice(),
            "plan basename byte-exact: {}",
            n.id
        );

        // (b) info_encode/decode roundtrips the hostile name as a segment of
        //     the original path, and is line-safe (always 3 lines, no `\n`
        //     inside the value — the codec escapes controls and non-UTF8).
        let original = root.join(seg);
        let bytes = trash::info_encode(&original, 42);
        let text = std::str::from_utf8(&bytes).expect("info is ASCII");
        assert_eq!(text.lines().count(), 3, "line-safe: {}", n.id);

        let info = trash::info_decode(&bytes, &root).unwrap_or_else(|_| panic!("decode {}", n.id));
        assert_eq!(info.original, original, "byte-exact roundtrip: {}", n.id);
        assert_eq!(info.deleted_ms, 42, "{}", n.id);
    }
}
