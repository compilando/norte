//! El pie de un listado (spec 2026-09-10): cuántos directorios y ficheros
//! hay y cuánto pesan, qué está marcado, y el espacio libre del volumen.
//!
//! Una sola redacción para los dos frontends, como las notas de la cabecera
//! (`notes`): la TUI lo pone en el borde inferior del panel y la ventana en
//! una fila bajo el listado, y dos redacciones del mismo hecho es de donde
//! salió media auditoría de paridad (ADR 0077).

use norte_i18n::{Lang, ta_in};
use norte_proto::{Entry, EntryKind};

/// Lo que hay en un listado, contado.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    /// Directorios (los symlinks a directorio cuentan como directorio).
    pub dirs: usize,
    /// Todo lo demás.
    pub files: usize,
    /// Bytes de lo que declara tamaño. Un provider que no lo trae (el local,
    /// para los directorios) no suma: la cuenta es de lo que se sabe.
    pub bytes: u64,
}

/// Cuenta `entries`, saltándose la fila `..` si `parent_row` la pone en
/// cabeza: es sintética y no está en el directorio.
#[must_use]
pub fn counts(entries: &[Entry], parent_row: bool) -> Counts {
    let mut c = Counts::default();
    for e in entries.iter().skip(usize::from(parent_row)) {
        match e.kind {
            EntryKind::Dir => c.dirs += 1,
            _ => c.files += 1,
        }
        c.bytes = c.bytes.saturating_add(e.size.unwrap_or(0));
    }
    c
}

/// Lo marcado, en crudo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Marked {
    /// Cuántas entradas.
    pub n: usize,
    /// Cuánto pesan las que declaran tamaño.
    pub bytes: u64,
    /// Cuántas de ellas son directorios.
    pub dirs: usize,
}

/// El pie, redactado: `12 dirs · 84 files · 1.3 GiB`, más `2 marked, 4.0
/// MiB` si hay marcas y `120 GiB free` si se sabe.
///
/// ```
/// use norte_frontend::footer::{Counts, Marked, pane_footer};
/// use norte_i18n::Lang;
///
/// let c = Counts { dirs: 2, files: 3, bytes: 2048 };
/// let s = pane_footer(c, Marked::default(), None, Lang::En);
/// assert!(s.contains('2') && s.contains('3') && s.contains("KiB"), "{s}");
/// assert!(!s.contains("marked") && !s.contains("free"), "{s}");
/// let s = pane_footer(c, Marked { n: 1, bytes: 10, dirs: 0 }, Some(1 << 30), Lang::En);
/// assert!(s.contains("marked") && s.contains("free"), "{s}");
/// ```
#[must_use]
pub fn pane_footer(counts: Counts, marked: Marked, free: Option<u64>, lang: Lang) -> String {
    let mut out = ta_in(
        lang,
        "pane-footer-counts",
        &[
            ("dirs", &counts.dirs.to_string()),
            ("files", &counts.files.to_string()),
            ("size", &crate::human_bytes(counts.bytes)),
        ],
    );
    let marcado = crate::notes::marked(marked.n, marked.bytes, marked.dirs, lang);
    if !marcado.is_empty() {
        out.push_str(" · ");
        out.push_str(&marcado);
    }
    if let Some(free) = free {
        out.push_str(" · ");
        out.push_str(&ta_in(
            lang,
            "pane-footer-free",
            &[("free", &crate::human_bytes(free))],
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::{Segment, VPath};

    fn entry(name: &str, kind: EntryKind, size: Option<u64>) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse("file:///d")
                .unwrap()
                .join(Segment::new(name.as_bytes().to_vec()).unwrap()),
            kind,
            size,
            mtime_ms: None,
        }
    }

    /// La fila `..` no cuenta, un directorio sin tamaño no suma, y el
    /// symlink cuenta como fichero.
    #[test]
    fn cuenta_sin_la_fila_padre_y_solo_lo_que_declara_tamano() {
        // La fila padre es SINTÉTICA (un `..` no es un segmento válido);
        // aquí cualquier primera entrada hace de ella.
        let entries = [
            entry("padre", EntryKind::Dir, None),
            entry("a", EntryKind::Dir, None),
            entry("b", EntryKind::File, Some(100)),
            entry("c", EntryKind::Symlink, Some(5)),
        ];
        assert_eq!(
            counts(&entries, true),
            Counts {
                dirs: 1,
                files: 2,
                bytes: 105
            }
        );
        assert_eq!(
            counts(&entries, false).dirs,
            2,
            "sin fila padre, `..` es un dir más"
        );
        assert_eq!(
            counts(&[], true),
            Counts::default(),
            "vacío con fila padre: nada"
        );
    }

    /// Las dos lenguas redactan, y el pie es corto: cabe en un borde.
    #[test]
    fn el_pie_redacta_en_las_dos_lenguas() {
        let c = Counts {
            dirs: 12,
            files: 84,
            bytes: 1 << 30,
        };
        for lang in [Lang::En, Lang::Es] {
            let s = pane_footer(c, Marked::default(), Some(120 << 30), lang);
            assert!(
                s.contains("12") && s.contains("84") && s.contains("120"),
                "{s}"
            );
            assert!(!s.contains("pane-footer"), "clave sin traducir: {s}");
            assert!(crate::display::cells(&s) <= 56, "{s}");
        }
    }
}
