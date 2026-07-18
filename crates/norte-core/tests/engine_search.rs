//! Integración `Engine::search_as` (M4 live search, T3): el walker BFS
//! cancelable emite hits en lotes por un canal, honra `max_hits`, salta
//! entradas ilegibles sin abortar y valida los criterios ANTES de crear la
//! Task. `MemProvider` in-memory → determinista, sin tocar disco.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use norte_core::{Actor, Engine};
use norte_proto::methods::{FsSearchParams, MatchInfo, SearchHits};
use norte_proto::{
    ByteRange, Capabilities, Entry, EntryKind, Error as ProtoError, TaskState, VPath,
};
use norte_testkit::MemProvider;
use norte_vfs::{ByteSink, ByteStream, EntryStream, Provider};
use tokio::sync::mpsc::Receiver;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

async fn write_file(mem: &MemProvider, wire: &str, content: &[u8]) {
    let mut sink = mem.write(&vp(wire)).await.expect("write abre");
    sink.write(Bytes::copy_from_slice(content))
        .await
        .expect("chunk");
    sink.commit().await.expect("commit");
}

async fn mkdir(mem: &MemProvider, wire: &str) {
    mem.mkdir(&vp(wire)).await.expect("mkdir");
}

/// Engine + `MemProvider` in-memory registrado bajo `mem`.
fn setup() -> (Engine, Arc<MemProvider>) {
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    (engine, mem)
}

/// Params base: solo raíz, todo lo demás vacío/false.
fn params(root: &str) -> FsSearchParams {
    FsSearchParams {
        root: vp(root),
        name_glob: None,
        name_regex: None,
        content: None,
        content_regex: None,
        case_sensitive: false,
        max_hits: None,
    }
}

/// Drena el canal hasta el cierre; devuelve `(entry, match)` aplanado.
async fn drain(mut rx: Receiver<SearchHits>) -> Vec<(Entry, Option<MatchInfo>)> {
    let mut out = Vec::new();
    while let Some(hits) = rx.recv().await {
        let matches = hits.matches.unwrap_or_default();
        for (i, e) in hits.entries.into_iter().enumerate() {
            out.push((e, matches.get(i).cloned()));
        }
    }
    out
}

fn paths(hits: &[(Entry, Option<MatchInfo>)]) -> Vec<String> {
    let mut p: Vec<String> = hits.iter().map(|(e, _)| e.path.display_lossy()).collect();
    p.sort();
    p
}

/// Forma de display de un wire path (para comparar contra [`paths`]).
fn disp(wire: &str) -> String {
    vp(wire).display_lossy()
}

// 1 ───────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn solo_nombre_encuentra_recursivo() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///a").await;
    mkdir(&mem, "mem:///a/sub").await;
    write_file(&mem, "mem:///a/x.rs", b"").await;
    write_file(&mem, "mem:///a/sub/y.rs", b"").await;
    write_file(&mem, "mem:///a/sub/z.txt", b"").await;

    let mut p = params("mem:///a");
    p.name_glob = Some("*.rs".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        paths(&hits),
        vec![disp("mem:///a/sub/y.rs"), disp("mem:///a/x.rs")]
    );
    // Nombre puro: sin contexto de contenido.
    assert!(hits.iter().all(|(_, m)| m.is_none()));
}

// 2 ───────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn contenido_multiencoding_salta_binarios() {
    let (engine, mem) = setup();
    write_file(&mem, "mem:///f1.txt", "hay un año aquí\n".as_bytes()).await;
    // Latin-1: frase con varios bytes altos para que el detector la clave.
    write_file(
        &mem,
        "mem:///f2.txt",
        b"El ni\xF1o comi\xF3 en el jard\xEDn hace un a\xF1o entero\n",
    )
    .await;
    // Binario: NUL + bytes de la aguja legacy → detect=Binary, se salta.
    write_file(&mem, "mem:///f3.bin", b"\x00\x00a\xF1o binario").await;

    let mut p = params("mem:///");
    p.content = Some("año".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        paths(&hits),
        vec![disp("mem:///f1.txt"), disp("mem:///f2.txt")]
    );
    // Contexto de contenido poblado (línea + preview) en ambos.
    for (_, m) in &hits {
        let m = m.as_ref().expect("match info de contenido");
        assert_eq!(m.line, Some(1));
        assert!(m.preview.as_ref().is_some_and(|s| s.contains('a')));
    }
}

// 3 ───────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn cancelacion_limpia_cierra_el_canal() {
    let (engine, mem) = setup();
    for i in 0..200 {
        write_file(
            &mem,
            &format!("mem:///f{i:03}.txt"),
            b"contiene ano y mas\n",
        )
        .await;
    }
    // Latencia por op: cada read del walker tarda, hay tiempo de cancelar.
    mem.faults()
        .set_latency_per_op(Some(Duration::from_millis(3)));

    let mut p = params("mem:///");
    p.content = Some("ano".to_owned());
    let (h, mut rx) = engine.search_as(p, Actor::User).await.expect("search");

    // Espera el primer lote (la búsqueda ya arrancó) y cancela.
    let first = rx.recv().await.expect("primer lote");
    assert!(!first.entries.is_empty());
    h.cancel();

    // El canal se cierra (drop de tx) y la Task termina Cancelled.
    let mut got = first.entries.len();
    while let Some(b) = rx.recv().await {
        got += b.entries.len();
    }
    assert!(got < 200, "cancelada a mitad: {got} < 200");
    assert_eq!(h.join().await, TaskState::Cancelled);
}

// 4 ───────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn max_hits_trunca_y_completa() {
    let (engine, mem) = setup();
    for i in 0..10 {
        write_file(&mem, &format!("mem:///m{i}.rs"), b"").await;
    }
    let mut p = params("mem:///");
    p.name_glob = Some("*.rs".to_owned());
    p.max_hits = Some(3);
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(hits.len(), 3, "exactamente max_hits");
    assert_eq!(h.join().await, TaskState::Completed);
}

// 5 ───────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn errores_por_entrada_no_abortan() {
    let (engine, mem) = setup();
    mkdir(&mem, "mem:///good").await;
    mkdir(&mem, "mem:///bad").await;
    write_file(&mem, "mem:///good/hit.rs", b"").await;
    write_file(&mem, "mem:///bad/otro.rs", b"").await;
    // El listado de `mem:///bad` falla: el walker lo salta y sigue.
    mem.faults().fail_list_at(&vp("mem:///bad"));

    let mut p = params("mem:///");
    p.name_glob = Some("*.rs".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    // Solo el fichero del subdir legible aparece.
    assert_eq!(paths(&hits), vec![disp("mem:///good/hit.rs")]);
}

// 6 ───────────────────────────────────────────────────────────────────────
#[tokio::test]
async fn criterios_invalidos_fallan_antes_de_la_task() {
    let (engine, _mem) = setup();

    // Cero criterios.
    let r = engine.search_as(params("mem:///"), Actor::User).await;
    assert!(r.is_err(), "sin criterios = Err antes de la Task");

    // Glob Y regex de nombre a la vez (excluyentes por eje).
    let mut p = params("mem:///");
    p.name_glob = Some("*.rs".to_owned());
    p.name_regex = Some("^.*$".to_owned());
    assert!(engine.search_as(p, Actor::User).await.is_err());

    // content Y content_regex a la vez.
    let mut p = params("mem:///");
    p.content = Some("x".to_owned());
    p.content_regex = Some("x".to_owned());
    assert!(engine.search_as(p, Actor::User).await.is_err());

    // Glob que no compila.
    let mut p = params("mem:///");
    p.name_glob = Some("a[".to_owned());
    assert!(engine.search_as(p, Actor::User).await.is_err());
}

/// Codifica `s` a UTF-16 con BOM (LE o BE).
fn utf16_bom(s: &str, le: bool) -> Vec<u8> {
    let mut out = if le {
        vec![0xFF, 0xFE]
    } else {
        vec![0xFE, 0xFF]
    };
    for cu in s.encode_utf16() {
        out.extend_from_slice(&if le {
            cu.to_le_bytes()
        } else {
            cu.to_be_bytes()
        });
    }
    out
}

// review MAJOR ── el match en una línea ≥2 de un UTF-16 NO se pierde ────────
#[tokio::test]
async fn utf16_match_en_linea_posterior_le_y_be() {
    let (engine, mem) = setup();
    // "x\naño\n": el match está en la LÍNEA 2 — cortar por el byte 0x0A crudo
    // desalinearía los pares y perdería el match (regresión del review).
    write_file(&mem, "mem:///le.txt", &utf16_bom("x\naño\n", true)).await;
    write_file(&mem, "mem:///be.txt", &utf16_bom("x\naño\n", false)).await;

    let mut p = params("mem:///");
    p.content = Some("año".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        paths(&hits),
        vec![disp("mem:///be.txt"), disp("mem:///le.txt")]
    );
    // Y la línea reportada es la 2 (no la 1).
    for (_, m) in &hits {
        assert_eq!(m.as_ref().and_then(|m| m.line), Some(2));
    }
}

// review MINOR-1 ── una línea gigante no explota la RAM (se trunca) ─────────
#[tokio::test]
async fn linea_gigante_se_trunca_pero_casa_al_inicio() {
    let (engine, mem) = setup();
    // Regex de contenido → ruta decode-por-líneas. Línea única de 4 MiB con la
    // aguja al PRINCIPIO: debe casar aunque la línea se trunque para el match.
    let mut giant = b"MATCH_ME ".to_vec();
    giant.extend(std::iter::repeat_n(b'a', 4 * 1024 * 1024));
    write_file(&mem, "mem:///huge.txt", &giant).await;

    let mut p = params("mem:///");
    p.content_regex = Some("MATCH_ME".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(paths(&hits), vec![disp("mem:///huge.txt")]);
    // El preview está acotado (no vuelca 4 MiB).
    let preview = hits[0].1.as_ref().and_then(|m| m.preview.clone()).unwrap();
    assert!(preview.chars().count() <= 160);
}

// 7 ── caso encoding-aware de los tres ficheros (deuda T2) ─────────────────
#[tokio::test]
async fn contenido_encoding_aware_tres_ficheros() {
    let (engine, mem) = setup();
    // Latin-1: "año" = 0xF1; frase larga para detección estable.
    write_file(
        &mem,
        "mem:///year_latin1.txt",
        b"Este documento cumple un a\xF1o; el ni\xF1o so\xF1\xF3 en espa\xF1ol.\n",
    )
    .await;
    // UTF-16LE con BOM: "un año\n" → se enruta por DECODE, no por byte-scan.
    let mut u16 = vec![0xFF, 0xFE];
    for cu in "un año\n".encode_utf16() {
        u16.extend_from_slice(&cu.to_le_bytes());
    }
    write_file(&mem, "mem:///year_utf16bom.txt", &u16).await;
    // CJK en UTF-8: contiene 0xF1 como byte LÍDER (U+44001) — NO debe casar
    // la aguja latina corta con la búsqueda encoding-aware.
    write_file(
        &mem,
        "mem:///cjk_utf8.txt",
        "汉字 \u{44001} texto\n".as_bytes(),
    )
    .await;

    let mut p = params("mem:///");
    p.content = Some("año".to_owned());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    assert_eq!(
        paths(&hits),
        vec![
            disp("mem:///year_latin1.txt"),
            disp("mem:///year_utf16bom.txt"),
        ],
        "Latin-1 y UTF-16-BOM casan; el CJK-UTF8 no (falso positivo evitado)"
    );
}

// 10 (security T4, MEDIA) ──────────────────────────────────────────────────
/// Provider que delega en un `MemProvider` real pero, al listar `inject_under`,
/// AÑADE entradas fabricadas cuyo path está FUERA del subtree — simula un
/// provider con bug (o malicioso) que devuelve NO-descendientes. `run_walk`
/// debe ignorarlas por completo (defensa en profundidad: el scope de la
/// búsqueda es invariante DURO del core, no confía en la corrección del list).
struct RogueList {
    inner: Arc<MemProvider>,
    inject_under: VPath,
    inject: Vec<Entry>,
}

#[async_trait]
impl Provider for RogueList {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, p: &VPath) -> Result<Entry, ProtoError> {
        self.inner.stat(p).await
    }
    async fn list(&self, p: &VPath) -> Result<EntryStream, ProtoError> {
        let mut inner = self.inner.list(p).await?;
        let mut items: Vec<Result<Entry, ProtoError>> = Vec::new();
        while let Some(e) = inner.next().await {
            items.push(e);
        }
        // Inyecta los no-descendientes SOLO al listar el dir del ataque.
        if p == &self.inject_under {
            for e in &self.inject {
                items.push(Ok(e.clone()));
            }
        }
        Ok(Box::pin(futures::stream::iter(items)))
    }
    async fn read(&self, p: &VPath, range: Option<ByteRange>) -> Result<ByteStream, ProtoError> {
        self.inner.read(p, range).await
    }
    async fn write(&self, p: &VPath) -> Result<Box<dyn ByteSink>, ProtoError> {
        self.inner.write(p).await
    }
    async fn mkdir(&self, p: &VPath) -> Result<(), ProtoError> {
        self.inner.mkdir(p).await
    }
    async fn remove(&self, p: &VPath) -> Result<(), ProtoError> {
        self.inner.remove(p).await
    }
    async fn rename(&self, from: &VPath, to: &VPath) -> Result<(), ProtoError> {
        self.inner.rename(from, to).await
    }
}

#[tokio::test]
async fn walk_ignora_entradas_fuera_del_root_aunque_el_provider_las_liste() {
    let mem = Arc::new(MemProvider::new());
    // Dentro del root del ataque: un fichero genuino con la aguja.
    mkdir(&mem, "mem:///proj").await;
    write_file(&mem, "mem:///proj/inside.txt", "año dentro".as_bytes()).await;
    // FUERA del root: un fichero con la aguja y un dir con un hijo con la aguja.
    write_file(&mem, "mem:///secret.txt", "año secreto".as_bytes()).await;
    mkdir(&mem, "mem:///other").await;
    write_file(&mem, "mem:///other/hidden.txt", "año oculto".as_bytes()).await;

    // El provider inyecta esos no-descendientes al listar mem:///proj.
    let rogue = Arc::new(RogueList {
        inner: Arc::clone(&mem),
        inject_under: vp("mem:///proj"),
        inject: vec![
            Entry {
                path: vp("mem:///secret.txt"),
                kind: EntryKind::File,
                size: None,
                mtime_ms: None,
            },
            Entry {
                path: vp("mem:///other"),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: None,
            },
        ],
    });
    let engine = Engine::new();
    engine.register_provider(rogue as Arc<dyn Provider>);

    let mut p = params("mem:///proj");
    p.content = Some("año".into());
    let (h, rx) = engine.search_as(p, Actor::User).await.expect("search");
    let hits = drain(rx).await;
    assert_eq!(h.join().await, TaskState::Completed);
    // SOLO el descendiente genuino: los no-descendientes jamás se leen (secret)
    // ni se descienden (other/hidden). El confinamiento no depende del provider.
    assert_eq!(paths(&hits), vec![disp("mem:///proj/inside.txt")]);
}
