//! El sidebar de sitios: discos y favoritos, en un panel que se queda.
//!
//! Estado PURO, sin `Backend` y sin render: se le entregan los volúmenes que
//! contestó `host.volumes` y los favoritos que ya trae la config, y devuelve
//! filas, cursor y destino. Lo mismo que hace [`crate::help`] con la ayuda, y
//! por el mismo motivo: así se prueba entero sin daemon y sin terminal.
//!
//! # Lo que NO decide este módulo
//!
//! CUÁNDO se piden los volúmenes. Un sidebar que sondea sería la regla de
//! suspensión del ADR 0058 rota desde el primer frame, así que quien lo pinta
//! los pide al abrirlo y al refrescar, y nunca por reloj.
//!
//! # Dos secciones, no tres
//!
//! No hay «Remotos»: norte no tiene lista de conexiones todavía (#140), y un
//! `sftp://` guardado como favorito ya sale bajo Favoritos. Inventar la
//! sección sin la fuente sería una caja vacía prometiendo algo.

use norte_proto::VPath;
use norte_proto::methods::Volume;

/// Las secciones del sidebar, en el orden en que se pintan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Los volúmenes del host, con su espacio.
    Drives,
    /// La hotlist del usuario.
    Favorites,
}

impl Section {
    /// La clave Fluent de su cabecera.
    ///
    /// ```
    /// use norte_frontend::places::Section;
    /// assert_eq!(Section::Drives.label_key(), "places-section-drives");
    /// ```
    #[must_use]
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Drives => "places-section-drives",
            Self::Favorites => "places-section-favorites",
        }
    }
}

/// Una fila pintable del sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceRow {
    /// La cabecera de una sección. No navega.
    Header {
        /// De qué sección.
        section: Section,
        /// ¿Está plegada?
        folded: bool,
    },
    /// Un volumen del host.
    Drive {
        /// Lo que el sistema lo llama, en BYTES: ninguna plataforma promete
        /// que la etiqueta de un volumen sea UTF-8 (regla 1). Vacío = sin
        /// etiqueta, y entonces se pinta el punto de montaje.
        label: Vec<u8>,
        /// Dónde está montado.
        mount: VPath,
        /// Espacio libre, o `None` si el filesystem no contestó.
        ///
        /// `None` NO es cero: un cero aquí se leería como «lleno». Es la misma
        /// regla que [`crate::space`] aplica al aviso previo a una copia.
        free: Option<u64>,
        /// Espacio total, con la misma advertencia que [`Self::Drive::free`].
        total: Option<u64>,
        /// ¿Está montado de solo lectura?
        read_only: bool,
    },
    /// Un favorito de la hotlist.
    Favorite {
        /// El nombre que le puso el usuario.
        name: String,
        /// Su destino, o la clave Fluent del error si la ruta no parsea.
        ///
        /// Un favorito roto se PINTA, con su motivo: uno que desaparece en
        /// silencio es un fallo de configuración que nadie puede ver.
        target: Result<VPath, String>,
    },
}

/// El sidebar entero: sus dos fuentes, qué está plegado y dónde está el
/// cursor.
///
/// ```
/// use norte_frontend::places::{PlaceRow, PlacesState};
/// use norte_proto::VPath;
///
/// let mut s = PlacesState::new();
/// s.set_favorites(&[(
///     "casa".to_owned(),
///     Ok(VPath::parse("file:///home").expect("wire")),
/// )]);
/// // Las DOS cabeceras están siempre, aunque una sección esté vacía: sin
/// // volúmenes todavía, la lista no da un brinco cuando lleguen. El cursor
/// // arranca en la primera cabecera, que no navega a ningún sitio.
/// assert_eq!(s.rows().len(), 3);
/// assert!(s.activate().is_none());
/// s.down();
/// s.down();
/// assert!(matches!(s.rows()[s.cursor()], PlaceRow::Favorite { .. }));
/// assert!(s.activate().is_some());
/// ```
#[derive(Debug, Clone)]
pub struct PlacesState {
    drives: Vec<PlaceRow>,
    favorites: Vec<PlaceRow>,
    drives_folded: bool,
    favorites_folded: bool,
    rows: Vec<PlaceRow>,
    cursor: usize,
}

impl Default for PlacesState {
    fn default() -> Self {
        Self::new()
    }
}

impl PlacesState {
    /// Un sidebar vacío: sin volúmenes y sin favoritos todavía.
    ///
    /// Vacío de CONTENIDO, no de filas: las dos cabeceras existen desde el
    /// primer frame. Sin ellas, el panel recién abierto sería una caja en
    /// blanco mientras `host.volumes` contesta, y plegar no querría decir nada
    /// porque el cursor no estaría en ninguna sección.
    #[must_use]
    pub fn new() -> Self {
        let mut s = Self {
            drives: Vec::new(),
            favorites: Vec::new(),
            drives_folded: false,
            favorites_folded: false,
            rows: Vec::new(),
            cursor: 0,
        };
        s.rebuild();
        s
    }

    /// Sustituye los volúmenes por los que acaba de contestar el host.
    ///
    /// Sustituye, no fusiona: la lista de montajes es una FOTO, y conservar
    /// uno que ya no está sería ofrecer un sitio al que no se puede ir.
    pub fn set_drives(&mut self, volumes: &[Volume]) {
        self.drives = volumes
            .iter()
            .map(|v| PlaceRow::Drive {
                label: v.label.clone().unwrap_or_default(),
                mount: v.mount.clone(),
                free: v.free_bytes,
                total: v.total_bytes,
                read_only: v.read_only,
            })
            .collect();
        self.rebuild();
    }

    /// Sustituye los favoritos.
    ///
    /// Recibe el par ya desmenuzado y no el tipo de la config: este crate no
    /// tiene por qué depender de `norte-config` para una struct de dos
    /// campos, y el frontend que la tiene delante la traduce en el sitio de
    /// llamada.
    pub fn set_favorites(&mut self, items: &[(String, Result<VPath, String>)]) {
        self.favorites = items
            .iter()
            .map(|(name, target)| PlaceRow::Favorite {
                name: name.clone(),
                target: target.clone(),
            })
            .collect();
        self.rebuild();
    }

    /// Las filas VISIBLES, cabeceras incluidas y sin lo que esté plegado.
    #[must_use]
    pub fn rows(&self) -> &[PlaceRow] {
        &self.rows
    }

    /// Dónde está el cursor dentro de [`Self::rows`].
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// Pone el cursor en la fila `i`, acotado a las que hay.
    ///
    /// Lo pide el RATÓN (#226): un click nombra una fila por su POSICIÓN, y
    /// llegar a ella a base de `up`/`down` sería reimplementar la aritmética
    /// del cursor en el frontend. Fuera de rango se acota en vez de no hacer
    /// nada: una lista que encogió entre el frame y el click no debe dejar el
    /// cursor donde estaba.
    pub fn set_cursor(&mut self, i: usize) {
        self.cursor = i.min(self.rows.len().saturating_sub(1));
    }

    /// Sube una fila. En la primera se queda.
    pub const fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja una fila. En la última se queda.
    pub fn down(&mut self) {
        let ultimo = self.rows.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add(1).min(ultimo);
    }

    /// Pliega o despliega la sección donde está el cursor.
    ///
    /// Sobre una fila cualquiera vale la sección a la que pertenece, así que
    /// plegar no obliga a subir hasta la cabecera primero.
    pub fn toggle_fold(&mut self) {
        match self.section_at(self.cursor) {
            Some(Section::Drives) => self.drives_folded = !self.drives_folded,
            Some(Section::Favorites) => self.favorites_folded = !self.favorites_folded,
            None => return,
        }
        self.rebuild();
    }

    /// A dónde lleva la fila del cursor, o `None`.
    ///
    /// `None` en una cabecera y en un favorito roto: los dos se pintan, y
    /// ninguno de los dos es un sitio.
    #[must_use]
    pub fn activate(&self) -> Option<&VPath> {
        match self.rows.get(self.cursor)? {
            PlaceRow::Header { .. } => None,
            PlaceRow::Drive { mount, .. } => Some(mount),
            PlaceRow::Favorite { target, .. } => target.as_ref().ok(),
        }
    }

    /// ¿Está plegada esa sección?
    ///
    /// Lo pregunta quien tiene el `Backend` delante: desplegar las unidades es
    /// el momento de volver a pedirlas, y plegarlas es el momento de NO
    /// pedirlas.
    #[must_use]
    pub const fn is_folded(&self, section: Section) -> bool {
        match section {
            Section::Drives => self.drives_folded,
            Section::Favorites => self.favorites_folded,
        }
    }

    /// A qué sección pertenece la fila `i`.
    fn section_at(&self, i: usize) -> Option<Section> {
        let mut actual = None;
        for (j, fila) in self.rows.iter().enumerate() {
            if let PlaceRow::Header { section, .. } = fila {
                actual = Some(*section);
            }
            if j == i {
                return actual;
            }
        }
        None
    }

    /// Rehace las filas visibles y recoloca el cursor dentro de ellas.
    ///
    /// Lo segundo es la mitad que se olvida: plegar una sección con el cursor
    /// dentro lo dejaría apuntando a una fila que ya no existe.
    fn rebuild(&mut self) {
        let mut out = Vec::with_capacity(self.rows.len() + 2);
        out.push(PlaceRow::Header {
            section: Section::Drives,
            folded: self.drives_folded,
        });
        if !self.drives_folded {
            out.extend(self.drives.iter().cloned());
        }
        out.push(PlaceRow::Header {
            section: Section::Favorites,
            folded: self.favorites_folded,
        });
        if !self.favorites_folded {
            out.extend(self.favorites.iter().cloned());
        }
        self.rows = out;
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::methods::{Volume, VolumeKind};

    fn vp(wire: &str) -> VPath {
        VPath::parse(wire).expect("wire válido")
    }

    fn volumen(mount: &str, free: Option<u64>, total: Option<u64>) -> Volume {
        Volume {
            mount: vp(mount),
            label: None,
            fs_type: "ext4".to_owned(),
            kind: VolumeKind::Fixed,
            total_bytes: total,
            free_bytes: free,
            read_only: false,
        }
    }

    fn con_label(label: Vec<u8>) -> Volume {
        Volume {
            label: Some(label),
            ..volumen("file:///", Some(1), Some(2))
        }
    }

    /// Un favorito roto se PINTA, con su motivo. Uno que desaparece en
    /// silencio es un fallo de config que no puedes ver.
    #[test]
    fn un_favorito_roto_sale_en_la_lista_y_no_navega() {
        let mut s = PlacesState::new();
        s.set_favorites(&[
            ("bueno".to_owned(), Ok(vp("file:///casa"))),
            ("roto".to_owned(), Err("err-invalid-path".to_owned())),
        ]);
        // Cabecera de discos (vacía), cabecera de favoritos, y los dos.
        assert_eq!(s.rows().len(), 4);
        assert!(matches!(
            s.rows()[1],
            PlaceRow::Header {
                section: Section::Favorites,
                ..
            }
        ));
        s.down();
        s.down();
        s.down();
        assert!(matches!(s.rows()[s.cursor()], PlaceRow::Favorite { .. }));
        assert!(s.activate().is_none(), "el roto no lleva a ningún sitio");
    }

    /// `free_bytes` ausente NO es cero: es «no contestó». La regla vive en
    /// `space.rs` y aquí se conserva el `Option` tal cual, sin sustituirlo por
    /// un número que se leería como «lleno».
    #[test]
    fn un_volumen_sin_espacio_conserva_el_none() {
        let mut s = PlacesState::new();
        s.set_drives(&[volumen("file:///mnt", None, None)]);
        let PlaceRow::Drive { free, total, .. } = &s.rows()[1] else {
            panic!("la fila 1 es el volumen");
        };
        assert!(free.is_none() && total.is_none());
    }

    /// Plegar esconde las filas de la sección y deja el cursor dentro de lo
    /// que queda.
    #[test]
    fn plegar_una_seccion_esconde_sus_filas_y_recoloca_el_cursor() {
        let mut s = PlacesState::new();
        s.set_drives(&[volumen("file:///", Some(1000), Some(4000))]);
        s.set_favorites(&[("casa".to_owned(), Ok(vp("file:///casa")))]);
        assert_eq!(s.rows().len(), 4);
        // El cursor al final del todo, que es donde plegar duele.
        for _ in 0..10 {
            s.down();
        }
        assert_eq!(s.cursor(), 3);
        s.toggle_fold();
        assert_eq!(s.rows().len(), 3);
        assert!(s.cursor() < s.rows().len());
    }

    /// `is_folded` dice lo mismo que la cabecera pinta: es lo que mira quien
    /// decide si toca volver a pedir los volúmenes.
    #[test]
    fn is_folded_sigue_al_toggle() {
        let mut s = PlacesState::new();
        assert!(!s.is_folded(Section::Drives));
        s.toggle_fold();
        assert!(s.is_folded(Section::Drives));
        assert!(!s.is_folded(Section::Favorites));
    }

    /// Plegar desde una fila cualquiera pliega SU sección, no la primera.
    #[test]
    fn plegar_desde_una_fila_pliega_su_propia_seccion() {
        let mut s = PlacesState::new();
        s.set_drives(&[volumen("file:///", Some(1), Some(2))]);
        s.set_favorites(&[("casa".to_owned(), Ok(vp("file:///casa")))]);
        s.down(); // sobre el volumen
        s.toggle_fold();
        assert!(matches!(
            s.rows()[0],
            PlaceRow::Header {
                section: Section::Drives,
                folded: true
            }
        ));
        assert!(matches!(
            s.rows()[1],
            PlaceRow::Header {
                section: Section::Favorites,
                folded: false
            }
        ));
    }

    /// La etiqueta de un volumen son BYTES (regla 1): un nombre que no es
    /// UTF-8 no revienta ni se pierde por el camino.
    #[test]
    fn una_etiqueta_no_utf8_sobrevive_como_bytes() {
        let mut s = PlacesState::new();
        s.set_drives(&[con_label(b"\xffdisco".to_vec())]);
        let PlaceRow::Drive { label, .. } = &s.rows()[1] else {
            panic!("volumen")
        };
        assert_eq!(label, b"\xffdisco");
    }

    /// Un volumen que desaparece del host desaparece de la lista: ofrecer un
    /// montaje que ya no está es ofrecer un sitio al que no se puede ir.
    #[test]
    fn set_drives_sustituye_no_fusiona() {
        let mut s = PlacesState::new();
        s.set_drives(&[volumen("file:///", Some(1), Some(2))]);
        s.set_drives(&[volumen("file:///mnt", Some(1), Some(2))]);
        assert_eq!(s.rows().len(), 3);
        let PlaceRow::Drive { mount, .. } = &s.rows()[1] else {
            panic!("volumen")
        };
        assert_eq!(*mount, vp("file:///mnt"));
    }
}
