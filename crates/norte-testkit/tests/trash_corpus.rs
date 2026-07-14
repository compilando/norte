//! El módulo puro `norte_vfs::trash` contra el corpus hostil canónico
//! (regla CLAUDE.md: TODO crate que toque paths se testea contra el corpus).
//! Ata que la construcción de rutas de papelera y el roundtrip de
//! `.norte-info` son byte-exactos y line-safe para los 19 nombres — una
//! regresión de pérdida silenciosa en el codec de wire se pondría roja aquí.

use norte_proto::{Segment, VPath};
use norte_testkit::corpus;
use norte_vfs::trash;

#[test]
fn trash_roundtrips_full_hostile_corpus() {
    let root = VPath::parse("sftp://host/").expect("root válido");

    for n in corpus::hostile_names() {
        let seg = Segment::new(n.bytes.clone())
            .unwrap_or_else(|_| panic!("corpus {} debe ser un Segment válido", n.id));
        let victim = root.join(seg.clone());

        // (a) plan() preserva el basename byte-exact bajo `.norte-trash/<id>/`.
        let paths =
            trash::plan(&victim, "1726000000123-0").unwrap_or_else(|_| panic!("plan {}", n.id));
        assert_eq!(
            paths
                .payload
                .file_name()
                .expect("payload tiene basename")
                .as_bytes(),
            n.bytes.as_slice(),
            "plan basename byte-exact: {}",
            n.id
        );

        // (b) info_encode/decode roundtripea el nombre hostil como segmento
        //     de la ruta original, y es line-safe (siempre 3 líneas, sin `\n`
        //     dentro del valor — el codec escapa controles y no-UTF8).
        let original = root.join(seg);
        let bytes = trash::info_encode(&original, 42);
        let text = std::str::from_utf8(&bytes).expect("info es ASCII");
        assert_eq!(text.lines().count(), 3, "line-safe: {}", n.id);

        let info = trash::info_decode(&bytes, &root).unwrap_or_else(|_| panic!("decode {}", n.id));
        assert_eq!(info.original, original, "roundtrip byte-exact: {}", n.id);
        assert_eq!(info.deleted_ms, 42, "{}", n.id);
    }
}
