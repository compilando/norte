//! De un índice y un `stat` a una celda de columna.
//!
//! El orden importa y es el de git: comparar `stat` primero —eso contesta la
//! inmensa mayoría de los casos sin abrir nada—, y solo cuando la comparación
//! es ambigua (el caso «racy»: el fichero tiene el MISMO mtime que el índice,
//! así que pudo cambiar dentro del mismo segundo) leer el contenido y comparar
//! el id de objeto.
//!
//! Vocabulario de celda: vacío = limpio, `M` modificado, `D` borrado,
//! `?` no rastreado, `!` ignorado.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::ignore::Ignores;
use crate::index::GitIndex;

/// Metadatos de una entrada tal y como los da el host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    /// `true` si es un directorio.
    pub is_dir: bool,
    /// Tamaño en bytes.
    pub size: u64,
    /// mtime en segundos.
    pub mtime_sec: i64,
    /// mtime en nanosegundos.
    pub mtime_nsec: u32,
}

/// Lo que el guest sabe pedirle a la ubicación. Un trait para que la lógica se
/// pruebe en el host sin un runtime wasm por medio: lo que se prueba es la
/// decisión, no la ABI.
pub trait Location {
    /// Bytes de un fichero bajo la raíz.
    ///
    /// # Errors
    /// Cualquier cadena que el host devuelva.
    fn read(&self, rel: &[u8]) -> Result<Vec<u8>, String>;

    /// Metadatos de una entrada bajo la raíz.
    ///
    /// # Errors
    /// Cualquier cadena que el host devuelva.
    fn stat(&self, rel: &[u8]) -> Result<Meta, String>;
}

/// El estado de UNA entrada visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Rastreada y sin cambios.
    Clean,
    /// Rastreada y distinta de lo que dice el índice.
    Modified,
    /// Rastreada y ya no está en el disco.
    Deleted,
    /// No rastreada.
    Untracked,
    /// No rastreada y tapada por un `.gitignore`.
    Ignored,
}

/// Con qué se pinta cada estado (`[config] glyphs`, spec 2026-09-11 V4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Glyphs {
    /// Las letras de `git status --short`: `M`, `D`, `?`, `!`.
    #[default]
    Letters,
    /// Símbolos de una celda: `●` modificado, `✖` borrado, `+` nuevo, `·`
    /// ignorado. Se leen de un vistazo y no se confunden con un nombre.
    Symbols,
}

/// Cómo se pinta la columna: los glifos y si los ignorados se marcan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub glyphs: Glyphs,
    /// `false` = un fichero ignorado se pinta como limpio. Con `.gitignore`
    /// grandes la marca se repite en media pantalla y deja de decir nada.
    pub hide_ignored: bool,
}

impl Style {
    /// Desde los dos valores de `[config]` tal como los da el host.
    #[must_use]
    pub fn parse(glyphs: Option<&str>, ignored: Option<&str>) -> Self {
        Self {
            glyphs: match glyphs {
                Some("symbols") => Glyphs::Symbols,
                _ => Glyphs::Letters,
            },
            hide_ignored: matches!(ignored, Some("false")),
        }
    }
}

impl State {
    /// La celda que ve el usuario, con las letras de siempre. `Clean` no
    /// pinta nada: una columna llena de marcas iguales no dice nada.
    #[must_use]
    pub fn cell(self) -> Option<String> {
        self.cell_with(Style::default())
    }

    /// La celda que ve el usuario, con el estilo configurado.
    #[must_use]
    pub fn cell_with(self, style: Style) -> Option<String> {
        let glyph = match (self, style.glyphs) {
            (Self::Clean, _) => return None,
            (Self::Ignored, _) if style.hide_ignored => return None,
            (Self::Modified, Glyphs::Letters) => "M",
            (Self::Deleted, Glyphs::Letters) => "D",
            (Self::Untracked, Glyphs::Letters) => "?",
            (Self::Ignored, Glyphs::Letters) => "!",
            (Self::Modified, Glyphs::Symbols) => "●",
            (Self::Deleted, Glyphs::Symbols) => "✖",
            (Self::Untracked, Glyphs::Symbols) => "+",
            (Self::Ignored, Glyphs::Symbols) => "·",
        };
        Some(glyph.to_string())
    }

    /// Cuál de dos estados manda al agregar un directorio. Lo más fuerte gana:
    /// un directorio con algo modificado dentro está modificado, y da igual
    /// cuántos ficheros limpios lo acompañen.
    fn rank(self) -> u8 {
        match self {
            Self::Clean => 0,
            Self::Ignored => 1,
            Self::Untracked => 2,
            Self::Deleted => 3,
            Self::Modified => 4,
        }
    }

    fn strongest(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

/// El estado de cada entrada visible de un directorio.
///
/// `prefix` es el camino del directorio visible relativo a la raíz del
/// repositorio (vacío = la raíz), y `names` son los nombres de la página.
pub fn status_for(
    index: &GitIndex,
    ignores: &Ignores,
    loc: &dyn Location,
    prefix: &[u8],
    names: &[Vec<u8>],
    index_mtime_sec: i64,
) -> Vec<Option<String>> {
    status_for_with(
        index,
        ignores,
        loc,
        prefix,
        names,
        index_mtime_sec,
        Style::default(),
    )
}

/// [`status_for`] con el estilo configurado (`[config] glyphs`/`ignored`).
pub fn status_for_with(
    index: &GitIndex,
    ignores: &Ignores,
    loc: &dyn Location,
    prefix: &[u8],
    names: &[Vec<u8>],
    index_mtime_sec: i64,
    style: Style,
) -> Vec<Option<String>> {
    names
        .iter()
        .map(|name| {
            let rel = join(prefix, name);
            state_of(index, ignores, loc, &rel, index_mtime_sec).cell_with(style)
        })
        .collect()
}

#[cfg(test)]
mod style_tests {
    use super::*;

    #[test]
    fn el_estilo_cambia_el_glifo_y_puede_callar_los_ignorados() {
        assert_eq!(State::Modified.cell(), Some("M".to_string()));
        let simbolos = Style::parse(Some("symbols"), Some("true"));
        assert_eq!(State::Modified.cell_with(simbolos), Some("●".to_string()));
        assert_eq!(State::Deleted.cell_with(simbolos), Some("✖".to_string()));
        assert_eq!(State::Untracked.cell_with(simbolos), Some("+".to_string()));
        assert_eq!(State::Ignored.cell_with(simbolos), Some("·".to_string()));
        assert_eq!(State::Clean.cell_with(simbolos), None, "limpio no pinta");
        let sin_ignorados = Style::parse(Some("letters"), Some("false"));
        assert_eq!(State::Ignored.cell_with(sin_ignorados), None);
        assert_eq!(
            State::Untracked.cell_with(sin_ignorados),
            Some("?".to_string())
        );
        assert_eq!(
            Style::parse(None, None),
            Style::default(),
            "sin config, lo de siempre"
        );
    }
}

fn join(prefix: &[u8], name: &[u8]) -> Vec<u8> {
    if prefix.is_empty() {
        return name.to_vec();
    }
    let mut out = prefix.to_vec();
    out.push(b'/');
    out.extend_from_slice(name);
    out
}

/// El estado de UNA ruta relativa a la raíz del repositorio.
fn state_of(
    index: &GitIndex,
    ignores: &Ignores,
    loc: &dyn Location,
    rel: &[u8],
    index_mtime_sec: i64,
) -> State {
    let meta = loc.stat(rel).ok();
    if let Some(entry) = index.get(rel) {
        let Some(meta) = meta else {
            return State::Deleted;
        };
        return compare(entry, &meta, loc, rel, index_mtime_sec);
    }
    let is_dir = meta.is_some_and(|m| m.is_dir);
    if is_dir {
        // Un directorio agrega lo más fuerte que haya debajo. El índice está
        // ORDENADO, así que las entradas rastreadas bajo el prefijo son un
        // tramo contiguo y no hay que recorrerlo entero.
        let mut dir_prefix = rel.to_vec();
        dir_prefix.push(b'/');
        let mut peor = State::Clean;
        let mut rastreado = false;
        for entry in index.under_prefix(&dir_prefix) {
            rastreado = true;
            let hijo = match loc.stat(&entry.path) {
                Ok(meta) => compare(entry, &meta, loc, &entry.path, index_mtime_sec),
                Err(_) => State::Deleted,
            };
            peor = peor.strongest(hijo);
            if peor == State::Modified {
                break; // ya no hay nada más fuerte que encontrar
            }
        }
        if rastreado {
            return peor;
        }
    }
    if ignores.is_ignored(rel, is_dir) {
        State::Ignored
    } else {
        State::Untracked
    }
}

/// Compara una entrada del índice con el `stat` de ahora, y solo si eso no
/// decide, lee el fichero.
fn compare(
    entry: &crate::index::IndexEntry,
    meta: &Meta,
    loc: &dyn Location,
    rel: &[u8],
    index_mtime_sec: i64,
) -> State {
    // Un SUBMÓDULO no es un fichero (#225). Su entrada del índice es un
    // «gitlink» —modo `0o160000`— cuya ruta es el directorio, así que la
    // búsqueda exacta la encuentra y la comparación de abajo veía un
    // directorio donde el índice decía fichero: daba `D`, o sea por borrado un
    // submódulo perfectamente sano.
    //
    // Lo que se contesta es NADA, y es deliberado: saber si tiene cambios
    // exige abrir el repositorio de dentro —otro `.git`, otro índice, otro
    // árbol de objetos—, que es la misma frontera que deja fuera el estado
    // «staged». Callar es honesto; poner una marca sería afirmar algo que no
    // se ha mirado.
    if entry.mode & 0o170_000 == 0o160_000 {
        return State::Clean;
    }
    if meta.is_dir {
        // Era un fichero rastreado y ahora hay un directorio: para git eso es
        // el fichero borrado.
        return State::Deleted;
    }
    if u64::from(entry.size) != meta.size {
        return State::Modified;
    }
    let mtime_igual =
        i64::from(entry.mtime_sec) == meta.mtime_sec && entry.mtime_nsec == meta.mtime_nsec;
    if !mtime_igual {
        // Mismo tamaño, otro mtime: puede ser un `touch` sin cambios, así que
        // decide el contenido y no la marca de tiempo.
        return by_content(entry, loc, rel);
    }
    // El `stat` casa. Aun así puede mentir: si la entrada se guardó en el
    // MISMO segundo en que se escribió el índice —el caso «racy git»—, un
    // cambio posterior dentro de ese segundo es indistinguible. git resuelve
    // esto exactamente así, comparando contra el mtime del PROPIO índice, y
    // por eso `read` existe en la interfaz.
    if i64::from(entry.mtime_sec) >= index_mtime_sec {
        return by_content(entry, loc, rel);
    }
    State::Clean
}

/// El desempate por contenido: el id de objeto que git le daría al fichero.
/// Si no se puede leer —presupuesto agotado, permisos— la respuesta es limpio,
/// nunca una marca inventada.
fn by_content(entry: &crate::index::IndexEntry, loc: &dyn Location, rel: &[u8]) -> State {
    let Ok(bytes) = loc.read(rel) else {
        return State::Clean;
    };
    if u64::from(entry.size) != bytes.len() as u64 {
        return State::Modified;
    }
    if entry.oid == [0u8; 20] {
        // El índice no trajo id (una forja de test): con el tamaño igual, no
        // hay nada más que comparar.
        return State::Clean;
    }
    if crate::sha1::blob_oid(&bytes) == entry.oid {
        State::Clean
    } else {
        State::Modified
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use core::cell::RefCell;

    /// Una ubicación de mentira que CUENTA las lecturas: el test de que el
    /// `stat` basta es un test sobre cuántas veces se abrió un fichero.
    #[derive(Default)]
    struct FakeLocation {
        files: BTreeMap<Vec<u8>, (Vec<u8>, Meta)>,
        reads: RefCell<usize>,
    }

    impl FakeLocation {
        fn with(mut self, rel: &[u8], content: &[u8], meta: Meta) -> Self {
            self.files.insert(rel.to_vec(), (content.to_vec(), meta));
            self
        }

        fn with_dir(mut self, rel: &[u8]) -> Self {
            self.files.insert(
                rel.to_vec(),
                (
                    Vec::new(),
                    Meta {
                        is_dir: true,
                        size: 0,
                        mtime_sec: 0,
                        mtime_nsec: 0,
                    },
                ),
            );
            self
        }

        fn reads(&self) -> usize {
            *self.reads.borrow()
        }
    }

    impl Location for FakeLocation {
        fn read(&self, rel: &[u8]) -> Result<Vec<u8>, String> {
            *self.reads.borrow_mut() += 1;
            self.files
                .get(rel)
                .map(|(c, _)| c.clone())
                .ok_or_else(|| "no existe".to_string())
        }

        fn stat(&self, rel: &[u8]) -> Result<Meta, String> {
            self.files
                .get(rel)
                .map(|(_, m)| *m)
                .ok_or_else(|| "no existe".to_string())
        }
    }

    /// El índice se escribió DESPUÉS que las entradas: nada es racy.
    const INDICE_NUEVO: i64 = 100;
    /// El índice se escribió a la vez que la entrada: el caso racy.
    const INDICE_VIEJO: i64 = 0;

    fn meta(size: u64, mtime_sec: i64, mtime_nsec: u32) -> Meta {
        Meta {
            is_dir: false,
            size,
            mtime_sec,
            mtime_nsec,
        }
    }

    /// Un índice con las entradas dadas: `(ruta, tamaño, mtime, oid)`.
    fn index_con(entradas: &[(&[u8], u32, u32, [u8; 20])]) -> GitIndex {
        GitIndex::parse(&crate::index::tests_support::forja(entradas)).expect("índice forjado")
    }

    #[test]
    fn stat_igual_al_indice_es_limpio_y_no_lee_el_fichero() {
        let idx = index_con(&[(b"a.txt", 4, 11, [0u8; 20])]);
        let fs = FakeLocation::default().with(b"a.txt", b"hola", meta(4, 11, 0));
        let cells = status_for(
            &idx,
            &Ignores::default(),
            &fs,
            b"",
            &[b"a.txt".to_vec()],
            INDICE_NUEVO,
        );
        assert_eq!(cells, vec![None], "celda vacía = limpio");
        assert_eq!(fs.reads(), 0, "el stat basta: no se lee el contenido");
    }

    #[test]
    fn mtime_igual_pero_tamano_distinto_es_modificado() {
        let idx = index_con(&[(b"a.txt", 4, 11, [0u8; 20])]);
        let fs = FakeLocation::default().with(b"a.txt", b"holaaa", meta(6, 11, 0));
        let cells = status_for(
            &idx,
            &Ignores::default(),
            &fs,
            b"",
            &[b"a.txt".to_vec()],
            INDICE_NUEVO,
        );
        assert_eq!(cells, vec![Some("M".to_string())]);
        assert_eq!(fs.reads(), 0, "el tamaño ya lo decidió");
    }

    /// El caso racy: mismo mtime, mismo tamaño y el índice sin nanosegundos.
    /// El `stat` NO decide, así que se lee y se compara el id de objeto —
    /// exactamente lo que hace git, y por eso `read` existe en la interfaz.
    #[test]
    fn el_caso_racy_lee_y_compara_el_oid() {
        let oid = crate::sha1::blob_oid(b"hola");
        let idx = index_con(&[(b"a.txt", 4, 11, oid)]);
        let fs = FakeLocation::default().with(b"a.txt", b"otro", meta(4, 11, 0));
        // El índice se escribió en el mismo segundo: el stat casa y aun así
        // no decide nada.
        let cells = status_for(
            &idx,
            &Ignores::default(),
            &fs,
            b"",
            &[b"a.txt".to_vec()],
            INDICE_VIEJO,
        );
        assert_eq!(cells, vec![Some("M".to_string())]);
        assert_eq!(fs.reads(), 1);

        let limpio = FakeLocation::default().with(b"a.txt", b"hola", meta(4, 11, 0));
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &limpio,
                b"",
                &[b"a.txt".to_vec()],
                INDICE_VIEJO
            ),
            vec![None]
        );
    }

    #[test]
    fn una_entrada_rastreada_que_ya_no_esta_es_borrada() {
        let idx = index_con(&[(b"a.txt", 4, 11, [0u8; 20])]);
        let fs = FakeLocation::default();
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"",
                &[b"a.txt".to_vec()],
                INDICE_NUEVO
            ),
            vec![Some("D".to_string())]
        );
    }

    #[test]
    fn lo_no_rastreado_es_interrogante_y_lo_ignorado_es_cierre_de_admiracion() {
        let idx = index_con(&[(b"seguido.txt", 1, 11, [0u8; 20])]);
        let mut ign = Ignores::default();
        ign.add_file(b"", b"target/\n*.tmp\n");
        let fs = FakeLocation::default()
            .with_dir(b"target")
            .with(b"nuevo.rs", b"", meta(0, 1, 1))
            .with(b"basura.tmp", b"", meta(0, 1, 1));
        let cells = status_for(
            &idx,
            &ign,
            &fs,
            b"",
            &[
                b"target".to_vec(),
                b"nuevo.rs".to_vec(),
                b"basura.tmp".to_vec(),
            ],
            INDICE_NUEVO,
        );
        assert_eq!(
            cells,
            vec![
                Some("!".to_string()),
                Some("?".to_string()),
                Some("!".to_string())
            ]
        );
    }

    /// **Un submódulo no está borrado** (#225).
    ///
    /// Su entrada del índice es un «gitlink» (modo `0o160000`) cuya ruta es el
    /// DIRECTORIO, así que `index.get` la encuentra y `compare` veía un
    /// directorio donde el índice decía fichero: `D`. O sea, la columna daba
    /// por borrado un submódulo perfectamente sano, que es una falsa alarma
    /// sobre lo que más asusta.
    ///
    /// Lo que dice ahora es NADA: sin abrir el repositorio de dentro no se
    /// puede saber si tiene cambios, y callar es lo honesto. Decir «limpio»
    /// con una marca sería afirmarlo.
    #[test]
    fn un_submodulo_no_sale_como_borrado() {
        let idx = GitIndex::parse(&crate::index::tests_support::forja_con_modo(&[(
            b"vendor/lib",
            0,
            11,
            [0u8; 20],
            0o160_000,
        )]))
        .expect("índice forjado");
        let fs = FakeLocation::default().with_dir(b"vendor/lib");
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"vendor",
                &[b"lib".to_vec()],
                INDICE_NUEVO
            ),
            vec![None],
            "un submódulo sano no es ni «borrado» ni «sin rastrear»"
        );
    }

    #[test]
    fn un_directorio_agrega_lo_mas_fuerte_que_hay_debajo() {
        let idx = index_con(&[
            (b"src/deep/x.rs", 4, 11, [0u8; 20]),
            (b"src/limpio.rs", 4, 11, [0u8; 20]),
        ]);
        let fs = FakeLocation::default()
            .with_dir(b"src")
            .with(b"src/deep/x.rs", b"otro", meta(9, 11, 0))
            .with(b"src/limpio.rs", b"hola", meta(4, 11, 0));
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"",
                &[b"src".to_vec()],
                INDICE_NUEVO
            ),
            vec![Some("M".to_string())]
        );
    }

    #[test]
    fn un_directorio_con_todo_limpio_no_pinta_nada() {
        let idx = index_con(&[(b"src/a.rs", 4, 11, [0u8; 20])]);
        let fs =
            FakeLocation::default()
                .with_dir(b"src")
                .with(b"src/a.rs", b"hola", meta(4, 11, 0));
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"",
                &[b"src".to_vec()],
                INDICE_NUEVO
            ),
            vec![None]
        );
    }

    /// El prefijo es lo que hace que esto funcione fuera de la raíz: la raíz
    /// abierta es el repositorio, y el panel puede estar tres niveles dentro.
    #[test]
    fn el_prefijo_situa_la_pagina_dentro_del_repositorio() {
        let idx = index_con(&[(b"src/deep/x.rs", 4, 11, [0u8; 20])]);
        let fs = FakeLocation::default().with(b"src/deep/x.rs", b"OTRO", meta(9, 11, 0));
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"src/deep",
                &[b"x.rs".to_vec()],
                INDICE_NUEVO
            ),
            vec![Some("M".to_string())]
        );
    }

    #[test]
    fn sin_indice_todas_las_celdas_son_no_rastreadas() {
        let idx = GitIndex::default();
        let fs = FakeLocation::default().with(b"a", b"", meta(0, 1, 1));
        assert_eq!(
            status_for(
                &idx,
                &Ignores::default(),
                &fs,
                b"",
                &[b"a".to_vec()],
                INDICE_NUEVO
            ),
            vec![Some("?".to_string())]
        );
    }
}
