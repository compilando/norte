//! Los constructores que comparten los tests de `app`: paths, entradas,
//! panes y `App` ya montadas. Vive fuera de cualquier `mod tests` porque lo
//! usan los de siete módulos hermanos, y duplicarlo en cada uno era la
//! alternativa.

use super::pane::Pane;
use super::*;
use norte_proto::{Entry, EntryKind, Scheme};

pub fn root() -> VPath {
    VPath::root(Scheme::new("mem").unwrap(), None)
}

pub fn file(name: &str) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: root().join(norte_proto::Segment::new(name.as_bytes().to_vec()).unwrap()),
        kind: EntryKind::File,
        size: Some(1),
        mtime_ms: None,
    }
}

pub fn names(p: &Pane) -> Vec<String> {
    p.entries()
        .iter()
        .map(|e| String::from_utf8_lossy(e.path.file_name().unwrap().as_bytes()).into_owned())
        .collect()
}

pub fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire de test")
}

/// Pane sobre `mem://` con archivos nombrados como se pida: #54, `Pane`
/// (vía `PaneState::new`) normaliza el orden internamente (dirs primero,
/// NFC, empate por bytes) — los tests del quick search razonan sobre el
/// índice real YA ORDENADO, no sobre el orden de llegada de `names`.
pub fn pane_con(names: &[&str]) -> Pane {
    Pane::new(root(), names.iter().map(|n| file(n)).collect())
}

pub fn app_dos_panes() -> App {
    App::new(pane_con(&["a"]), pane_con(&["b"]))
}

/// Unas caps cualesquiera: lo que se prueba es el CACHÉ por scheme, no
/// qué flags trae el provider.
pub fn caps_de_test() -> norte_proto::Capabilities {
    norte_proto::Capabilities {
        flags: norte_proto::CapabilityFlags::RENAME_ATOMIC,
        max_path: None,
    }
}

/// `App` con cada pane sobre SU dir (el `app_dos_panes` de arriba pone
/// los dos sobre `root()`, que no distingue lados).
pub fn app_en(left: &str, right: &str) -> App {
    App::new(
        Pane::new(vp(left), Vec::new()),
        Pane::new(vp(right), Vec::new()),
    )
}

pub fn e(wire: &str, k: EntryKind) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: VPath::parse(wire).unwrap(),
        kind: k,
        size: None,
        mtime_ms: None,
    }
}

/// App de un solo listado de nombres, sobre `mem://` (task 9, #103):
/// vía `pane_con` — mismo orden real que pinta la UI — con foco en el
/// pane lleno; el otro vacío. Nombre distinto de `app_with_sized_entries`
/// (`tests/status_marks.rs`): esa lleva tamaño explícito, esta solo
/// nombres.
pub fn app_with_entries(names: &[&str]) -> App {
    App::new(pane_con(names), Pane::new(root(), Vec::new()))
}

/// Como [`app_with_entries`], con el pane INACTIVO plantado en `dst`
/// (vacío): el destino ortodoxo de F5/F6 es el DIRECTORIO del otro pane
/// (#103 T10), así que los tests del lote necesitan un destino distinto
/// de la raíz de origen.
pub fn app_with_two_panes(names: &[&str], dst: &str) -> App {
    App::new(
        pane_con(names),
        Pane::new(VPath::parse(dst).unwrap(), Vec::new()),
    )
}

/// Una notif `connection.degraded` como la del wire (#44).
pub fn degradacion_de_test(scheme: &str, host: &str) -> norte_proto::methods::ConnectionDegraded {
    norte_proto::methods::ConnectionDegraded {
        scheme: scheme.to_owned(),
        host: host.to_owned(),
        reason: "ftp-plaintext".to_owned(),
        detail: None,
    }
}

/// Fila de comparación de un lado (huérfano) con la clase y el tamaño
/// pedidos, para las pruebas de `compare_size_probe_targets` (#157).
pub fn fila_huerfana(
    id: u64,
    kind: EntryKind,
    size: Option<u64>,
) -> norte_proto::methods::CompareRow {
    use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareVerdict};
    norte_proto::methods::CompareRow {
        id,
        left: Some(Entry {
            attrs: std::collections::BTreeMap::new(),
            path: root().join(norte_proto::Segment::new(format!("f{id}").into_bytes()).unwrap()),
            kind,
            size,
            mtime_ms: None,
        }),
        right: None,
        verdict: CompareVerdict::OnlyLeft,
        criterion: CompareCriterion::Presence,
        confidence: CompareConfidence::Certain,
        newer: None,
        reason: None,
        side: None,
        paired_under: None,
    }
}
