//! Qué DOS ficheros compara `pane.compare-files` (#312).
//!
//! Comparar dos árboles ya existe (`pane.compare-dirs`, con su panel de
//! diferencias). Lo que faltaba es la pareja, que es lo que tienen Total
//! Commander y Krusader, y que aquí se delega en un programa externo — el
//! escalón 1 de la issue.
//!
//! La regla de QUÉ dos vive aquí, en el crate compartido, porque es la misma
//! decisión en la terminal y en la ventana, y una decisión duplicada entre
//! frontends diverge en silencio (ADR 0077).
//!
//! **Dos ficheros o nada.** No se adivina: con tres marcados, con uno solo, o
//! con una carpeta de por medio, el comando lo DICE. Comparar «lo que sea que
//! haya» es la clase de conveniencia que acaba enseñando la diferencia entre
//! dos ficheros que el lector no eligió.

use norte_proto::{Entry, EntryKind, VPath};

/// Por qué no hay pareja que comparar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    /// No son exactamente dos: ni dos marcados, ni uno en cada panel.
    NotTwo,
    /// Son dos, y alguno no es un fichero (una carpeta es `compare-dirs`).
    NotFiles,
}

impl PairError {
    /// La clave Fluent con la que se dice.
    #[must_use]
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::NotTwo => "msg-compare-files-need-two",
            Self::NotFiles => "msg-compare-files-not-files",
        }
    }
}

/// La pareja a comparar, en el orden en que se enseña: primero la del panel con
/// el foco (o la primera marcada), después la otra.
///
/// Dos fuentes, en este orden:
///
/// 1. **Lo marcado en el panel con el foco**, si hay marcas — el operando de
///    siempre. Tienen que ser exactamente dos.
/// 2. **Un fichero en cada panel**, que es como se compara en los dos gestores
///    de referencia: el de aquí contra el de enfrente.
///
/// ```
/// use norte_frontend::diffpair::{pair, PairError};
/// # use norte_proto::{Entry, EntryKind, VPath, Segment};
/// # fn f(n: &str) -> Entry {
/// #     Entry {
/// #         attrs: std::collections::BTreeMap::new(),
/// #         path: VPath::parse("file:///d").unwrap()
/// #             .join(Segment::new(n.as_bytes().to_vec()).unwrap()),
/// #         kind: EntryKind::File, size: Some(1), mtime_ms: None,
/// #     }
/// # }
/// let (a, b) = (f("a"), f("b"));
/// assert!(pair(&[&a, &b], None, None).is_ok());
/// assert_eq!(pair(&[&a], Some(&a), None), Err(PairError::NotTwo));
/// ```
///
/// # Errors
///
/// [`PairError`] cuando no son exactamente dos, o cuando alguno no es un
/// fichero.
pub fn pair(
    marked: &[&Entry],
    here: Option<&Entry>,
    there: Option<&Entry>,
) -> Result<(VPath, VPath), PairError> {
    let (a, b) = if marked.is_empty() {
        // Sin marcas, el de aquí contra el de enfrente. Si no hay dos paneles
        // —una disposición de un solo listado— tampoco hay pareja, y eso es un
        // `NotTwo` honesto: no hay nada contra lo que comparar.
        match (here, there) {
            (Some(a), Some(b)) => (a, b),
            _ => return Err(PairError::NotTwo),
        }
    } else if let [a, b] = marked {
        (*a, *b)
    } else {
        return Err(PairError::NotTwo);
    };
    // Un enlace vale: lo que hay al otro lado es un fichero, y quien lo marcó
    // sabe lo que marcó. Un directorio no, y ahí `pane.compare-dirs` es la
    // respuesta — decirlo es más útil que comparar dos listados a mano.
    if a.kind == EntryKind::Dir || b.kind == EntryKind::Dir {
        return Err(PairError::NotFiles);
    }
    // La misma ruta dos veces no es una comparación: es un diff vacío que se
    // lee como «son iguales» cuando lo que pasó es que se marcó una sola cosa.
    if a.path == b.path {
        return Err(PairError::NotTwo);
    }
    Ok((a.path.clone(), b.path.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: VPath::parse("file:///d")
                .expect("wire")
                .join(norte_proto::Segment::new(name.as_bytes().to_vec()).expect("segmento")),
            kind,
            size: Some(1),
            mtime_ms: None,
        }
    }

    #[test]
    fn dos_marcados_son_la_pareja() {
        let (a, b) = (entry("a", EntryKind::File), entry("b", EntryKind::File));
        let (x, y) = pair(&[&a, &b], None, None).expect("dos ficheros marcados");
        assert_eq!((x, y), (a.path, b.path));
    }

    #[test]
    fn sin_marcas_es_uno_de_cada_panel() {
        let (a, b) = (entry("a", EntryKind::File), entry("b", EntryKind::File));
        assert!(pair(&[], Some(&a), Some(&b)).is_ok());
    }

    /// Tres marcados no son una pareja, y adivinar cuáles dos sería enseñar la
    /// diferencia entre dos ficheros que nadie eligió.
    #[test]
    fn ni_uno_ni_tres() {
        let (a, b, c) = (
            entry("a", EntryKind::File),
            entry("b", EntryKind::File),
            entry("c", EntryKind::File),
        );
        assert_eq!(pair(&[&a, &b, &c], None, None), Err(PairError::NotTwo));
        assert_eq!(pair(&[&a], Some(&a), Some(&b)), Err(PairError::NotTwo));
        assert_eq!(pair(&[], Some(&a), None), Err(PairError::NotTwo));
    }

    #[test]
    fn una_carpeta_manda_a_comparar_directorios() {
        let (a, d) = (entry("a", EntryKind::File), entry("d", EntryKind::Dir));
        assert_eq!(pair(&[&a, &d], None, None), Err(PairError::NotFiles));
    }

    /// El mismo fichero en los dos paneles: el diff saldría vacío y se leería
    /// como «son iguales», que es una respuesta equivocada a una pregunta que
    /// nadie hizo.
    #[test]
    fn el_mismo_fichero_dos_veces_no_es_una_comparacion() {
        let a = entry("a", EntryKind::File);
        assert_eq!(pair(&[], Some(&a), Some(&a)), Err(PairError::NotTwo));
    }
}
