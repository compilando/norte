//! El tema y el selector de volúmenes.
//!
//! Dos superficies pequeñas y una sola idea: enseñar lo que hay sin poder
//! tocarlo todavía.
//!
//! - El **tema** se ve por dentro: qué color tiene cada ROL, que es lo que un
//!   tema de norte nombra de verdad, y qué efectos declara que esta ventana
//!   no sabe pintar. Decirlo es la mitad del contrato: un tema retro que no
//!   se ve distinto es un tema que el usuario cree roto.
//! - Los **volúmenes** se eligen y se navega a ellos, que es lectura.
//!
//! El selector de CONEXIONES que la tarea 4.5 nombra a su lado no está, y la
//! ausencia es una decisión: leer `connections.toml` obliga a meter
//! `norte-connect` —con russh, opendal, suppaftp, age y el llavero— en esta
//! ventana, para una lista que todavía no puede abrir ninguna conexión.
//! Llega con la fase 5, que necesita ese crate de todas formas; hasta
//! entonces `pane.connect` contesta «aquí no», que es verdad.

use norte_i18n::Lang;
use norte_proto::VPath;

use crate::bridge::clamp_display;
use crate::dto::{PickerRowView, PickerView, ThemeRoleView, ThemeView};

/// El tema tal como lo resolvió quien arrancó el host.
///
/// Llega ya resuelto por el mismo motivo que las rutas: el host no lee
/// ficheros. La correspondencia rol → color es EXPLÍCITA en quien la
/// construye (`norte_gui_tauri::catalog::variables`), no un volcado
/// automático, y esta vista enseña exactamente esa lista — la misma que
/// alimenta las variables CSS, así que lo que se ve aquí es lo que pinta.
#[derive(Debug, Clone, Default)]
pub struct HostTheme {
    /// Cómo se llama.
    pub name: String,
    /// Cada rol con su color `#rrggbb`, en el orden en que se declaran.
    pub roles: Vec<(String, String)>,
    /// Los efectos que el tema declara. TODOS son «no soportados» hoy: este
    /// renderer es una webview y no interpreta ninguno.
    pub effects: Vec<String>,
}

impl HostTheme {
    /// La proyección.
    #[must_use]
    pub(crate) fn vista(&self) -> ThemeView {
        ThemeView {
            name: clamp_display(self.name.clone()),
            roles: self
                .roles
                .iter()
                .map(|(role, color)| ThemeRoleView {
                    role: clamp_display(role.clone()),
                    color: color_valido(color),
                })
                .collect(),
            unsupported_effects: self
                .effects
                .iter()
                .map(|e| {
                    // La clave sale del fichero de tema: se enmascara, y se
                    // DICE que se enmascaró (#266).
                    let (pintable, hostil) = norte_frontend::display_name(e.as_bytes());
                    crate::dto::ThemeEffectView {
                        key: clamp_display(pintable),
                        hostile: hostil,
                    }
                })
                .collect(),
        }
    }
}

/// Un color `#rrggbb`, o vacío.
///
/// El renderer lo mete en `style.setProperty("background-color", …)`. Hoy
/// llega siempre de `Theme::to_hex()`, así que es seguro — pero el invariante
/// lo sostenía UN llamante y nada lo decía en el tipo. El CSSOM tira un valor
/// que no parsea en vez de partirlo por `;`, o sea que esto no es un agujero
/// de inyección; es que la garantía no estaba escrita en ninguna parte.
///
/// Uno que no case se manda VACÍO: la muestra sin pintar dice que el tema
/// tiene un color que no vale, y una cadena arbitraria en una propiedad CSS
/// no dice nada.
fn color_valido(color: &str) -> String {
    let bien = color.len() == 7
        && color.starts_with('#')
        && color[1..].bytes().all(|b| b.is_ascii_hexdigit());
    if bien {
        color.to_owned()
    } else {
        String::new()
    }
}

/// Un selector abierto.
pub(crate) struct Selector {
    filas: Vec<Fila>,
    cursor: usize,
    /// La lista está vacía y esta es la clave Fluent que lo explica.
    vacio: &'static str,
    /// Cómo se llama, en clave Fluent. Estaba CLAVADO en el de volúmenes, que
    /// era el único; con tres, un título fijo miente en dos de ellos.
    titulo: &'static str,
    /// A qué hueco navega lo elegido.
    ///
    /// Explícito y no «el activo»: `pane.select-drive-left` nombra un LADO de
    /// la pantalla, y el lado se resuelve al ABRIR. Leerlo al elegir haría
    /// que mover el foco mientras la lista está puesta cambiara el panel que
    /// acaba montando el volumen.
    slot: u32,
}

/// Una fila con lo que hace falta para ACTUAR, además de para pintar.
struct Fila {
    vista: PickerRowView,
    /// A dónde navega.
    destino: Option<VPath>,
}

impl Selector {
    /// El selector de volúmenes, todavía sin la lista: se pide y llega.
    pub(crate) fn volumenes(slot: u32) -> Self {
        Self {
            filas: Vec::new(),
            cursor: 0,
            vacio: "picker-volumes-loading",
            titulo: "picker-volumes-title",
            slot,
        }
    }

    /// El rastro de navegación de un hueco, más reciente primero.
    ///
    /// Las filas son las de `History::entries` —el MRU compartido— y no una
    /// segunda lista de aquí: qué recuerda un panel y en qué orden no puede
    /// depender de quién lo pinta.
    pub(crate) fn historial(slot: u32, rastro: &std::collections::VecDeque<VPath>) -> Self {
        let filas = rastro
            .iter()
            .map(|p| {
                let (pintable, hostile) = norte_frontend::display::path_display(p);
                Fila {
                    vista: PickerRowView {
                        label: clamp_display(pintable),
                        hostile,
                        detail: String::new(),
                    },
                    destino: Some(p.clone()),
                }
            })
            .collect();
        Self {
            filas,
            cursor: 0,
            vacio: "picker-history-empty",
            titulo: "picker-history-title",
            slot,
        }
    }

    /// Los favoritos de la configuración.
    ///
    /// Un favorito cuya ruta no parsea SE QUEDA, con su aviso y sin destino:
    /// la hotlist es data del usuario, y uno que desaparece en silencio es un
    /// fallo que nadie puede ver (mismo criterio que la barra lateral).
    pub(crate) fn hotlist(
        slot: u32,
        favoritos: &[(String, Result<VPath, String>)],
        lang: Lang,
    ) -> Self {
        let filas = favoritos
            .iter()
            .map(|(nombre, destino)| {
                // El nombre de un favorito son BYTES tanto como una ruta: lo
                // escribió una persona en un fichero y puede llevar bidi.
                let (nombre_pintable, nombre_hostil) =
                    norte_frontend::display_name(nombre.as_bytes());
                let (detalle, detalle_hostil, destino) = match destino {
                    Ok(p) => {
                        let (pintable, hostile) = norte_frontend::display::path_display(p);
                        (pintable, hostile, Some(p.clone()))
                    }
                    Err(_) => (norte_i18n::t_in(lang, "hotlist-invalid"), false, None),
                };
                Fila {
                    vista: PickerRowView {
                        label: clamp_display(nombre_pintable),
                        hostile: nombre_hostil || detalle_hostil,
                        detail: clamp_display(detalle),
                    },
                    destino,
                }
            })
            .collect();
        Self {
            filas,
            cursor: 0,
            vacio: "picker-hotlist-empty",
            titulo: "picker-hotlist-title",
            slot,
        }
    }

    /// A qué hueco navega lo que se elija aquí.
    pub(crate) fn slot(&self) -> u32 {
        self.slot
    }

    /// Mete los volúmenes que contestó el host.
    pub(crate) fn set_volumenes(&mut self, vols: &[norte_proto::methods::Volume], lang: Lang) {
        self.vacio = "picker-volumes-empty";
        self.filas = vols
            .iter()
            .map(|v| {
                // Un punto de montaje es un `VPath`, o sea BYTES: se pinta
                // por el camino compartido y viaja con su marca.
                let (pintable, hostile) = norte_frontend::display::path_display(&v.mount);
                let (detail, detail_hostil) = detalle_de(v, lang);
                Fila {
                    vista: PickerRowView {
                        label: clamp_display(pintable),
                        // El punto de montaje O la ETIQUETA. La etiqueta es
                        // `Option<Vec<u8>>` y en Windows cruza como WTF-8: un
                        // surrogate suelto que una etiqueta FAT/NTFS puede
                        // llevar legalmente sobrevive en vez de convertirse
                        // en U+FFFD. Se enmascaraba y la marca se TIRABA,
                        // mientras la MISMA etiqueta en la barra lateral sí
                        // se marcaba: dos superficies, dos respuestas, los
                        // mismos bytes.
                        hostile: hostile || detail_hostil,
                        detail: clamp_display(detail),
                    },
                    destino: Some(v.mount.clone()),
                }
            })
            .collect();
        self.cursor = self.cursor.min(self.filas.len().saturating_sub(1));
    }

    /// Mueve el cursor sin salirse.
    pub(crate) fn mover(&mut self, delta: i64) {
        if self.filas.is_empty() {
            return;
        }
        let destino = i64::try_from(self.cursor)
            .unwrap_or(0)
            .saturating_add(delta);
        self.cursor = usize::try_from(destino.max(0))
            .unwrap_or(0)
            .min(self.filas.len() - 1);
    }

    /// Pone el cursor en una fila (un click). Fuera de rango no hace nada.
    pub(crate) fn senalar(&mut self, fila: usize) {
        if fila < self.filas.len() {
            self.cursor = fila;
        }
    }

    /// Hay una fila bajo el cursor, tenga destino o no.
    ///
    /// Distingue «la lista está vacía» de «esta fila no lleva a ninguna
    /// parte» —un favorito cuya ruta no parsea—, que son dos respuestas
    /// distintas y sin esto se contestaban igual: con silencio.
    pub(crate) fn hay_fila(&self) -> bool {
        self.filas.get(self.cursor).is_some()
    }

    /// A dónde navega la fila del cursor, si hay alguna.
    pub(crate) fn elegir(&self) -> Option<VPath> {
        self.filas.get(self.cursor)?.destino.clone()
    }

    /// La proyección.
    pub(crate) fn vista(&self, lang: Lang) -> PickerView {
        PickerView {
            title: clamp_display(norte_i18n::t_in(lang, self.titulo)),
            rows: self.filas.iter().map(|f| f.vista.clone()).collect(),
            cursor: (!self.filas.is_empty()).then_some(self.cursor as u64),
            empty: if self.filas.is_empty() {
                clamp_display(norte_i18n::t_in(lang, self.vacio))
            } else {
                String::new()
            },
            // La pone el controlador, que es quien sabe cuántas veces ha
            // cambiado el conjunto: el selector no se entera de sus propias
            // reaperturas.
            generation: 0,
        }
    }
}

/// El detalle de un volumen: su sistema de ficheros, el espacio y si es de
/// solo lectura.
///
/// El espacio que el sistema no contestó se DICE, jamás se pinta un `0`: cero
/// libre se lee como «lleno», que es lo contrario de «no lo sé».
///
/// Devuelve TAMBIÉN si lo pintado difiere de lo real: la etiqueta la da el
/// sistema y son bytes, así que la marca la produce esta función y quien la
/// llama tiene que llevarla a la fila. Antes se calculaba y se tiraba.
fn detalle_de(v: &norte_proto::methods::Volume, lang: Lang) -> (String, bool) {
    let mut trozos: Vec<String> = Vec::new();
    if !v.fs_type.is_empty() {
        trozos.push(norte_frontend::display_name(v.fs_type.as_bytes()).0);
    }
    match (v.free_bytes, v.total_bytes) {
        (Some(free), Some(total)) => trozos.push(norte_i18n::ta_in(
            lang,
            "picker-volume-space",
            &[
                ("free", &norte_frontend::human_bytes(free)),
                ("total", &norte_frontend::human_bytes(total)),
            ],
        )),
        _ => trozos.push(norte_i18n::t_in(lang, "volumes-size-unknown")),
    }
    if v.read_only {
        trozos.push(norte_i18n::t_in(lang, "picker-volume-read-only"));
    }
    // La etiqueta que da el sistema son BYTES —ninguna plataforma promete
    // UTF-8— así que entra por el mismo camino que un nombre de fichero.
    let mut hostil = false;
    if let Some(label) = &v.label {
        let (pintable, h) = norte_frontend::display_name(label);
        hostil = h;
        trozos.push(pintable);
    }
    (trozos.join(" · "), hostil)
}
