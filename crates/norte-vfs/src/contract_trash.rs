//! [`logical_trash_contract!`]: suite contractual de la papelera lógica
//! `.norte-trash/` (ADR 0019). La invocan los providers SIN trash nativo del
//! OS que ganan papelera vía [`logical_trash`](crate::logical_trash) (sftp,
//! object) y también el `MemProvider` del testkit como espejo determinista.
//!
//! A diferencia de [`provider_contract!`](crate::provider_contract), estos
//! casos ejercen [`logical_trash`](crate::logical_trash) DIRECTAMENTE (no
//! `Provider::trash`): el helper es la lógica compartida, y `MemProvider`
//! tiene además su propio `trash()` NATIVO (el subárbol se esfuma) que aquí no
//! interesa. El instante `now_ms` se inyecta fijo → el `<id>` es determinista
//! y los paths se asertan exactos.

/// Genera la suite contractual de la papelera lógica dentro de un módulo de
/// test.
///
/// - `mod`: nombre del módulo generado.
/// - `factory`: expresión que construye un provider FRESCO, escribible, con
///   `rename`/`mkdir`/`write` funcionales (se evalúa una vez por test).
/// - `root`: expresión que construye el `VPath` raíz del provider.
///
/// ```ignore
/// norte_vfs::logical_trash_contract! {
///     mod trash_mem,
///     factory: norte_testkit::MemProvider::new(),
///     root: norte_testkit::MemProvider::root(),
/// }
/// ```
#[macro_export]
macro_rules! logical_trash_contract {
    (
        mod $name:ident,
        factory: $factory:expr,
        root: $root:expr $(,)?
    ) => {
        mod $name {
            #![allow(clippy::redundant_clone)]

            #[allow(unused_imports)]
            use super::*;

            use $crate::__private::bytes::Bytes;
            use $crate::__private::futures::StreamExt;
            use $crate::__private::norte_proto::{EntryKind, Error, Segment, VPath};
            use $crate::Provider;

            /// Instante FIJO del borrado → `<id>` determinista.
            const NOW_MS: u64 = 0x0123_4567_89ab;

            fn seg(bytes: &[u8]) -> Segment {
                Segment::new(bytes.to_vec()).expect("segmento válido de contrato")
            }

            fn child(base: &VPath, name: &[u8]) -> VPath {
                base.join(seg(name))
            }

            /// El `<id>` que produce el helper para `NOW_MS` y el reintento `n`.
            fn trash_id(retry: u32) -> Vec<u8> {
                if retry == 0 {
                    format!("{NOW_MS:016x}").into_bytes()
                } else {
                    format!("{NOW_MS:016x}-{retry}").into_bytes()
                }
            }

            /// `root/.norte-trash/files/<id>`.
            fn trashed_file(root: &VPath, retry: u32) -> VPath {
                child(
                    &child(&child(root, b".norte-trash"), b"files"),
                    &trash_id(retry),
                )
            }

            /// `root/.norte-trash/meta/<id>.json`.
            fn trashed_meta(root: &VPath, retry: u32) -> VPath {
                let mut name = trash_id(retry);
                name.extend_from_slice(b".json");
                child(&child(&child(root, b".norte-trash"), b"meta"), &name)
            }

            fn hex(bytes: &[u8]) -> String {
                use std::fmt::Write as _;
                let mut out = String::with_capacity(bytes.len() * 2);
                for b in bytes {
                    let _ = write!(out, "{b:02x}");
                }
                out
            }

            async fn write_all<P: Provider>(p: &P, path: &VPath, content: &[u8]) {
                let mut sink = p.write(path).await.expect("write abre");
                sink.write(Bytes::copy_from_slice(content))
                    .await
                    .expect("chunk entra");
                sink.commit().await.expect("commit publica");
            }

            async fn read_all<P: Provider>(p: &P, path: &VPath) -> Vec<u8> {
                let mut stream = p.read(path, None).await.expect("read abre");
                let mut out = Vec::new();
                while let Some(chunk) = stream.next().await {
                    out.extend_from_slice(&chunk.expect("chunk sin error"));
                }
                out
            }

            // ---------- archivo ----------

            #[tokio::test]
            async fn trash_file_moves_to_files_and_writes_meta() {
                let p = $factory;
                let root: VPath = $root;
                let victima = child(&root, b"victima");
                write_all(&p, &victima, b"hola mundo").await;

                $crate::logical_trash(&p, &victima, NOW_MS)
                    .await
                    .expect("trash lógico");

                // El origen desaparece.
                assert!(matches!(p.stat(&victima).await, Err(Error::NotFound)));

                // El dato vive en files/<id> con los mismos bytes.
                let dest = trashed_file(&root, 0);
                assert_eq!(
                    p.stat(&dest).await.expect("dato en papelera").kind,
                    EntryKind::File
                );
                assert_eq!(read_all(&p, &dest).await, b"hola mundo");

                // El sidecar guarda el wire de origen (hex) y el instante.
                let meta = read_all(&p, &trashed_meta(&root, 0)).await;
                let meta = String::from_utf8(meta).expect("sidecar ASCII");
                assert!(
                    meta.contains(&format!(
                        r#""orig_hex":"{}""#,
                        hex(victima.to_wire().as_bytes())
                    )),
                    "sidecar sin orig_hex correcto: {meta}"
                );
                assert!(
                    meta.contains(&format!(r#""deleted_at_ms":{NOW_MS}"#)),
                    "sidecar sin deleted_at_ms: {meta}"
                );
            }

            // ---------- subárbol ----------

            #[tokio::test]
            async fn trash_dir_moves_whole_subtree() {
                let p = $factory;
                let root: VPath = $root;
                let dir = child(&root, b"proyecto");
                p.mkdir(&dir).await.expect("mkdir dir");
                write_all(&p, &child(&dir, b"a.txt"), b"AAA").await;
                write_all(&p, &child(&dir, b"b.txt"), b"BBB").await;

                $crate::logical_trash(&p, &dir, NOW_MS)
                    .await
                    .expect("trash lógico de dir");

                assert!(matches!(p.stat(&dir).await, Err(Error::NotFound)));

                let dest = trashed_file(&root, 0);
                assert_eq!(
                    p.stat(&dest).await.expect("dir en papelera").kind,
                    EntryKind::Dir
                );
                assert_eq!(read_all(&p, &child(&dest, b"a.txt")).await, b"AAA");
                assert_eq!(read_all(&p, &child(&dest, b"b.txt")).await, b"BBB");
            }

            // ---------- colisión de <id> ----------

            #[tokio::test]
            async fn trash_id_collision_gets_suffix() {
                let p = $factory;
                let root: VPath = $root;
                let f1 = child(&root, b"uno");
                let f2 = child(&root, b"dos");
                write_all(&p, &f1, b"1").await;
                write_all(&p, &f2, b"2").await;

                // Mismo NOW_MS: el segundo debe caer en <id>-1, no pisar al primero.
                $crate::logical_trash(&p, &f1, NOW_MS)
                    .await
                    .expect("trash 1");
                $crate::logical_trash(&p, &f2, NOW_MS)
                    .await
                    .expect("trash 2");

                assert_eq!(read_all(&p, &trashed_file(&root, 0)).await, b"1");
                assert_eq!(read_all(&p, &trashed_file(&root, 1)).await, b"2");
            }

            // ---------- guardas ----------

            #[tokio::test]
            async fn trash_root_is_invalid() {
                let p = $factory;
                let root: VPath = $root;
                assert!(matches!(
                    $crate::logical_trash(&p, &root, NOW_MS).await,
                    Err(Error::InvalidPath)
                ));
            }

            #[tokio::test]
            async fn trash_inside_trash_is_invalid() {
                let p = $factory;
                let root: VPath = $root;
                // Reciclar algo YA dentro de la papelera: rechazo limpio, jamás
                // un no-op silencioso (el nodo ni siquiera necesita existir: la
                // guarda es lo primero).
                let inside = child(&child(&child(&root, b".norte-trash"), b"files"), b"x");
                assert!(matches!(
                    $crate::logical_trash(&p, &inside, NOW_MS).await,
                    Err(Error::InvalidPath)
                ));
            }
        }
    };
}
