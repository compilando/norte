//! El árbol en RAM de UN `.rar`: entradas validadas como segmentos `VPath`,
//! omitidas contadas, y la pregunta que decide si un nombre se le puede pedir
//! al delegado sin ambigüedad.

use std::collections::{BTreeSet, HashMap};

use norte_proto::{Entry, EntryKind, Error, Segment, VPath};

use crate::RarLimits;
use crate::delegate::RarError;
use crate::listing::RawEntry;

/// Clave del árbol: los componentes del path interior, en bytes.
pub(crate) type InnerPath = Vec<Vec<u8>>;

/// Un nodo del árbol virtual.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub mtime_ms: Option<i64>,
    /// La entrada está cifrada: se LISTA, pero leerla pediría una contraseña
    /// que nadie va a teclear (el hijo tiene `stdin` a null).
    pub encrypted: bool,
}

impl Node {
    fn dir(mtime_ms: Option<i64>) -> Self {
        Self {
            kind: EntryKind::Dir,
            size: None,
            mtime_ms,
            encrypted: false,
        }
    }
}

/// Índice completo de UN archivo, ligado a una generación del contenedor.
pub struct ArchiveIndex {
    pub(crate) nodes: HashMap<InnerPath, Node>,
    pub(crate) children: HashMap<InnerPath, BTreeSet<Vec<u8>>>,
    skipped: u64,
    /// Los nombres COMPLETOS tal cual se le pedirían al delegado. Se guardan
    /// aparte porque la prueba de ambigüedad es contra ellos, no contra el
    /// árbol.
    full_names: Vec<Vec<u8>>,
    /// `(mtime_ms, size)` del contenedor al indexar — la invalidación.
    pub(crate) generation: (Option<i64>, Option<u64>),
}

impl ArchiveIndex {
    /// Construye el índice desde lo que imprimió el delegado, con los límites
    /// por defecto. Para los tests y para quien no configura nada.
    #[must_use]
    pub fn from_raw(raw: Vec<RawEntry>) -> Self {
        Self::build(raw, &RarLimits::default(), (None, None), 0)
    }

    /// Como [`from_raw`](Self::from_raw), diciendo límites, generación y
    /// cuántas entradas descartó ya el PARSER (que también cuentan).
    #[must_use]
    pub(crate) fn build(
        raw: Vec<RawEntry>,
        limits: &RarLimits,
        generation: (Option<i64>, Option<u64>),
        skipped_by_parser: u64,
    ) -> Self {
        let mut idx = Self {
            nodes: HashMap::new(),
            children: HashMap::new(),
            skipped: skipped_by_parser,
            full_names: Vec::new(),
            generation,
        };
        for entry in raw {
            idx.insert(&entry, limits);
        }
        if idx.skipped > 0 {
            tracing::warn!(
                skipped = idx.skipped,
                indexed = idx.nodes.len(),
                "entradas del rar omitidas por nombre no representable"
            );
        }
        idx
    }

    /// Cuántas entradas del archivo NO están en el árbol.
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Cuántos nodos tiene el árbol (dirs implícitos incluidos).
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// `true` si el archivo no trajo ninguna entrada representable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// ¿Se le puede pedir este nombre al delegado sin que saque OTRA cosa?
    ///
    /// MEDIDO: los dos delegados tratan el nombre de la entrada como un
    /// **patrón**, y ninguno tiene un interruptor de «esto es literal». Una
    /// entrada llamada `star?name.txt` extrae también `starXname.txt`, y el
    /// flujo parece perfectamente sano. Como el índice entero ya está en
    /// memoria, la ambigüedad se decide AQUÍ, contra nuestros propios
    /// nombres, antes de arrancar ningún proceso.
    ///
    /// # Errors
    ///
    /// [`RarError::AmbiguousForDelegate`] si el nombre, tratado como patrón,
    /// alcanza a alguna otra entrada del archivo.
    pub fn addressable(&self, name: &[u8]) -> Result<(), RarError> {
        if !name.iter().any(|b| matches!(b, b'*' | b'?' | b'[' | b']')) {
            return Ok(()); // sin metacaracteres no hay nada que confundir
        }
        let colisiones = self
            .full_names
            .iter()
            .filter(|other| other.as_slice() != name && glob_matches(name, other))
            .count();
        if colisiones == 0 {
            Ok(())
        } else {
            tracing::warn!(
                name = ?String::from_utf8_lossy(name),
                colisiones,
                "nombre indireccionable: el delegado lo trataría como patrón"
            );
            Err(RarError::AmbiguousForDelegate)
        }
    }

    /// Valida y trocea el nombre crudo. `None` = no representable, ya contado.
    fn split_name(&mut self, raw: &[u8], limits: &RarLimits) -> Option<(InnerPath, bool)> {
        let mut hostile = |why: &str| {
            tracing::debug!(name = ?String::from_utf8_lossy(raw), why, "entrada omitida");
            self.skipped += 1;
            None
        };
        if raw.is_empty() {
            return hostile("nombre vacío");
        }
        if raw.len() > limits.max_name_bytes {
            return hostile("nombre demasiado largo");
        }
        if raw.first() == Some(&b'/') {
            return hostile("path absoluto");
        }
        // RAR guarda `\` como separador cuando el archivo se hizo en Windows;
        // aquí NO se traduce: un nombre con `\` es un nombre con `\`, y
        // convertirlo inventaría una jerarquía que el archivo no declara.
        let is_dir = raw.last() == Some(&b'/');
        let body = if is_dir { &raw[..raw.len() - 1] } else { raw };
        let mut parts: InnerPath = Vec::new();
        for comp in body.split(|&b| b == b'/') {
            if comp.is_empty() || comp == b"." || comp == b".." {
                return hostile("componente `.`/`..`/vacío (traversal)");
            }
            if comp == b"!" {
                return hostile("componente `!` (marcador ADR 0018, indireccionable)");
            }
            if Segment::new(comp.to_vec()).is_err() {
                return hostile("componente inválido como segmento VPath");
            }
            parts.push(comp.to_vec());
        }
        if parts.is_empty() {
            return hostile("nombre sin componentes");
        }
        if parts.len() > limits.max_depth {
            return hostile("profundidad excesiva");
        }
        Some((parts, is_dir))
    }

    fn add_child(&mut self, parent: InnerPath, name: Vec<u8>) {
        self.children.entry(parent).or_default().insert(name);
    }

    /// Materializa los ancestros como dirs implícitos, con el mismo criterio
    /// «gana el dir» que ADR 0018: sin esto, un `a` fichero seguido de
    /// `a/hijo` dejaría el subárbol invisible.
    fn ensure_parents(&mut self, path: &[Vec<u8>]) {
        for depth in 0..path.len().saturating_sub(1) {
            let dir: InnerPath = path[..=depth].to_vec();
            let node = self.nodes.entry(dir).or_insert_with(|| Node::dir(None));
            if node.kind != EntryKind::Dir {
                *node = Node::dir(node.mtime_ms);
                self.skipped += 1;
            }
            self.add_child(path[..depth].to_vec(), path[depth].clone());
        }
    }

    fn insert(&mut self, entry: &RawEntry, limits: &RarLimits) {
        if self.nodes.len() >= limits.max_entries {
            self.skipped += 1;
            return;
        }
        let Some((path, trailing_slash)) = self.split_name(&entry.name, limits) else {
            return;
        };
        let is_dir = entry.is_dir || trailing_slash;
        let node = Node {
            kind: if is_dir {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            size: if is_dir { None } else { Some(entry.size) },
            mtime_ms: entry.mtime.map(|s| s * 1_000),
            encrypted: entry.encrypted,
        };
        self.ensure_parents(&path);
        // Un dir jamás degrada a file: se perdería el subárbol.
        if self
            .nodes
            .get(&path)
            .is_some_and(|prev| prev.kind == EntryKind::Dir && node.kind != EntryKind::Dir)
        {
            self.skipped += 1;
            return;
        }
        // El nombre que se le pedirá al delegado es el del ÁRBOL, no el
        // crudo: si el crudo traía `dir/` final, pedirlo con la barra no
        // extrae nada.
        let full = path.join(&b'/');
        if !self.full_names.contains(&full) {
            self.full_names.push(full);
        }
        self.nodes.insert(path.clone(), node);
        let (parent, name) = (
            path[..path.len() - 1].to_vec(),
            path.last().expect("path no vacío").clone(),
        );
        self.add_child(parent, name);
    }

    /// El `Entry` de un nodo, o de la raíz sintética del contenedor.
    pub(crate) fn entry_for(&self, at: &VPath, inner: &[Vec<u8>]) -> Result<Entry, Error> {
        if inner.is_empty() {
            return Ok(Entry {
                attrs: std::collections::BTreeMap::new(),
                path: at.clone(),
                kind: EntryKind::Dir,
                size: None,
                mtime_ms: self.generation.0,
            });
        }
        let node = self.nodes.get(inner).ok_or(Error::NotFound)?;
        Ok(Entry {
            attrs: std::collections::BTreeMap::new(),
            path: at.clone(),
            kind: node.kind,
            size: node.size,
            mtime_ms: node.mtime_ms,
        })
    }

    pub(crate) fn node(&self, inner: &[Vec<u8>]) -> Option<&Node> {
        self.nodes.get(inner)
    }
}

/// ¿`candidate` casa con `pattern` entendido como el glob que el delegado
/// aplicaría?
///
/// Deliberadamente GENEROSO: `*` cruza barras y una clase `[...]` mal cerrada
/// se trata como literal. Equivocarse de más aquí produce una negativa
/// («no puedo darte esa entrada sin ambigüedad»); equivocarse de menos
/// produce el contenido de OTRO fichero.
fn glob_matches(pattern: &[u8], candidate: &[u8]) -> bool {
    match pattern.first() {
        None => candidate.is_empty(),
        Some(b'*') => {
            (0..=candidate.len()).any(|skip| glob_matches(&pattern[1..], &candidate[skip..]))
        }
        Some(b'?') => !candidate.is_empty() && glob_matches(&pattern[1..], &candidate[1..]),
        Some(b'[') => match class_end(pattern) {
            Some(end) => {
                !candidate.is_empty()
                    && class_matches(&pattern[1..end], candidate[0])
                    && glob_matches(&pattern[end + 1..], &candidate[1..])
            }
            // Clase sin cerrar: literal, como hace un shell.
            None => literal_head(pattern, candidate),
        },
        Some(_) => literal_head(pattern, candidate),
    }
}

fn literal_head(pattern: &[u8], candidate: &[u8]) -> bool {
    match (pattern.first(), candidate.first()) {
        (Some(p), Some(c)) if p == c => glob_matches(&pattern[1..], &candidate[1..]),
        _ => false,
    }
}

fn class_end(pattern: &[u8]) -> Option<usize> {
    // `[]abc]` es una clase que contiene `]`: el primer `]` pegado al
    // corchete no cierra.
    let start = if pattern.get(1) == Some(&b'!') { 2 } else { 1 };
    let start = if pattern.get(start) == Some(&b']') {
        start + 1
    } else {
        start
    };
    pattern[start..]
        .iter()
        .position(|b| *b == b']')
        .map(|at| at + start)
}

fn class_matches(class: &[u8], byte: u8) -> bool {
    let (negate, body) = match class.first() {
        Some(b'!') => (true, &class[1..]),
        _ => (false, class),
    };
    let mut hit = false;
    let mut i = 0;
    while i < body.len() {
        if i + 2 < body.len() && body[i + 1] == b'-' {
            if (body[i]..=body[i + 2]).contains(&byte) {
                hit = true;
            }
            i += 3;
        } else {
            if body[i] == byte {
                hit = true;
            }
            i += 1;
        }
    }
    hit != negate
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(name: &[u8], size: u64) -> RawEntry {
        RawEntry {
            name: name.to_vec(),
            size,
            is_dir: false,
            mtime: None,
            encrypted: false,
            solid: false,
        }
    }

    /// MEDIDO: `star?name.txt` saca DOS entradas de los dos delegados.
    #[test]
    fn un_nombre_que_es_glob_de_otro_se_rechaza() {
        let idx = ArchiveIndex::from_raw(vec![raw(b"star?name.txt", 7), raw(b"starXname.txt", 7)]);
        assert!(matches!(
            idx.addressable(b"star?name.txt"),
            Err(RarError::AmbiguousForDelegate)
        ));
        // El gemelo literal NO es ambiguo: no contiene metacaracteres.
        assert!(idx.addressable(b"starXname.txt").is_ok());
    }

    /// Un patrón que solo se alcanza a sí mismo SÍ se puede pedir: negarlo
    /// escondería un fichero que se puede servir bien.
    #[test]
    fn un_glob_que_solo_se_alcanza_a_si_mismo_se_permite() {
        let idx = ArchiveIndex::from_raw(vec![raw(b"solo*.txt", 1), raw(b"otro.bin", 1)]);
        assert!(idx.addressable(b"solo*.txt").is_ok());
    }

    #[test]
    fn una_clase_de_corchetes_tambien_cuenta_como_patron() {
        let idx = ArchiveIndex::from_raw(vec![raw(b"a[bc]d.txt", 1), raw(b"abd.txt", 1)]);
        assert!(matches!(
            idx.addressable(b"a[bc]d.txt"),
            Err(RarError::AmbiguousForDelegate)
        ));
    }

    #[test]
    fn un_nombre_inseguro_se_salta_y_se_cuenta() {
        let idx = ArchiveIndex::from_raw(vec![
            raw(b"ok.txt", 1),
            raw(b"../fuera.txt", 1),
            raw(b"/abs.txt", 1),
            raw(b"con\0nul", 1),
        ]);
        assert_eq!(idx.len(), 1);
        assert_eq!(
            idx.skipped(),
            3,
            "ADR 0018: saltadas y contadas, jamás fatales"
        );
    }

    #[test]
    fn los_directorios_intermedios_se_materializan() {
        let idx = ArchiveIndex::from_raw(vec![raw(b"docs/sub/hoja.txt", 3)]);
        assert_eq!(idx.len(), 3, "docs, docs/sub y la hoja");
        assert_eq!(idx.children[&vec![]].len(), 1);
    }

    #[test]
    fn el_marcador_de_archivo_no_es_direccionable_y_se_omite() {
        let idx = ArchiveIndex::from_raw(vec![raw(b"a/!/b.txt", 1)]);
        assert!(idx.is_empty());
        assert_eq!(idx.skipped(), 1);
    }

    #[test]
    fn el_tope_de_entradas_no_revienta_el_indice_entero() {
        let limits = RarLimits {
            max_entries: 2,
            ..RarLimits::default()
        };
        let idx = ArchiveIndex::build(
            (0..5)
                .map(|i| raw(format!("f{i}.txt").as_bytes(), 1))
                .collect(),
            &limits,
            (None, None),
            0,
        );
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.skipped(), 3, "las que no caben se cuentan");
    }

    #[test]
    fn el_glob_de_asterisco_cruza_barras() {
        assert!(glob_matches(b"a*z", b"a/b/z"));
        assert!(!glob_matches(b"a?z", b"a/bz"));
        assert!(glob_matches(b"a[!x]z", b"abz"));
        assert!(!glob_matches(b"a[!x]z", b"axz"));
        assert!(glob_matches(b"a[b-d]z", b"acz"));
    }
}
