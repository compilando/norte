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

/// Las claves de `[effects]` que la ventana interpreta (spec 2026-09-11,
/// V6). Todo lo demás se enseña en la vista del tema como «sin soporte».
pub const EFECTOS_DE_LA_VENTANA: &[&str] = &["backdrop"];

/// La CORRESPONDENCIA: nombre de variable CSS, rol que lo llena, y si toma
/// el fondo (`true`) o el frente (`false`) de ese rol.
///
/// Es una tabla y no una secuencia de llamadas porque hacen falta las dos
/// preguntas por separado, y antes se respondían a la vez: **qué nombres
/// existen** (independiente de todo tema, ver [`nombres_de_tema`]) y **qué
/// colores tiene ESTE tema** (ver [`roles_de_tema`], que omite lo que el tema
/// calla). Mezcladas, un tema que no define un rol hacía desaparecer su
/// nombre de la lista, y el guardián de variables huérfanas del renderer leía
/// esa ausencia como «nadie alimenta esa variable».
///
/// Un rol puede aparecer DOS veces, una por lado: `Role::Selection` llena
/// `selection-bg` y `selection-fg`, y el terminal lo usa como estilo entero.
const CORRESPONDENCIA: &[(&str, norte_theme::Role, bool)] = {
    use norte_theme::Role;
    &[
        ("bg", Role::Background, true),
        ("fg", Role::Regular, false),
        ("panel-bg", Role::PaneBackground, true),
        ("panel-focus-bg", Role::PaneFocusBackground, true),
        ("border", Role::BorderUnfocused, false),
        ("border-focus", Role::BorderFocus, false),
        ("selection-bg", Role::Selection, true),
        ("selection-fg", Role::Selection, false),
        // El cursor del panel SIN foco y los botones de diálogo (spec
        // 2026-09-10): dos roles nuevos, dos parejas nuevas.
        ("selection-unfocused-bg", Role::SelectionUnfocused, true),
        ("selection-unfocused-fg", Role::SelectionUnfocused, false),
        ("button-bg", Role::Button, true),
        ("button-fg", Role::Button, false),
        ("mark-bg", Role::Mark, true),
        ("hostile-fg", Role::HostileBadge, false),
        ("status-bg", Role::StatusBar, true),
        // Y su primer plano. Faltaba, y `Role::StatusBar` es una PAREJA: el
        // terminal lo usa como estilo entero. Mandando solo el fondo, todo lo
        // que la ventana pinte encima tiene que adivinar el texto — la
        // cabecera del visor adivinaba `title-fg`, y con un tema cuya barra de
        // estado es clara eso es claro sobre claro: la ruta, el encoding, el
        // EOL y las pérdidas salían INVISIBLES. Un visor que no dice qué mira
        // miente por omisión.
        ("status-fg", Role::StatusBar, false),
        ("title-fg", Role::Title, false),
        ("error-fg", Role::Error, false),
        // Los dos roles que una DECORACIÓN de plugin puede pedir además de
        // `error`. Sin ellos, una insignia `warning` caía al color del título
        // y era indistinguible de una `info`: el rol es vocabulario cerrado
        // justamente para que signifique algo en pantalla.
        ("warning-fg", Role::Warning, false),
        ("info-fg", Role::Info, false),
        // El cromo de la ventana (spec 2026-09-11, F2). Los diez NO están en
        // `Role::CORE`, así que un tema puede callarlos —y los ocho presets
        // de siempre los callan— y entonces la hoja de estilos los DERIVA de
        // un color que el tema sí tiene: `var(--hover, var(--panel-focus-bg))`.
        // Por eso su nombre existe aquí aunque su color no llegue.
        ("hover", Role::Hover, true),
        ("input-bg", Role::InputBackground, true),
        ("input-border", Role::InputBorder, false),
        ("widget-bg", Role::WidgetBackground, true),
        ("widget-shadow", Role::WidgetShadow, false),
        ("badge-bg", Role::Badge, true),
        ("badge-fg", Role::Badge, false),
        ("scrollbar-slider", Role::ScrollbarSlider, true),
        ("separator", Role::Separator, false),
        ("focus-border", Role::FocusBorder, false),
        ("muted", Role::Muted, false),
    ]
};

/// Los nombres de variable CSS que la ventana conoce, existan o no en un tema
/// concreto. Es el ACUERDO con `style.css`, y lo comprueba el guardián de
/// variables huérfanas del renderer (`tests/variables_de_tema.rs`).
#[must_use]
pub fn nombres_de_tema() -> Vec<&'static str> {
    CORRESPONDENCIA.iter().map(|(n, _, _)| *n).collect()
}

/// Los roles del tema con su color, en el orden en que se nombran. Un rol que
/// el tema NO define se omite: la hoja de estilos lo deriva (ver la tabla
/// `CORRESPONDENCIA` de este módulo), y mandar un color inventado desde aquí
/// le quitaría esa posibilidad.
///
/// La correspondencia es EXPLÍCITA y no automática: una variable de la hoja
/// de estilos que nadie alimenta se ve (queda el valor por defecto), pero un
/// volcado automático de `Role` convertiría cada rol nuevo en una variable que
/// nadie usa y cada rename en un color que desaparece sin ruido.
///
/// Vive AQUÍ y no en quien hospeda, aunque los nombres sean los de sus
/// variables CSS, por una razón concreta: desde que el selector de tema elige,
/// el host tiene que resolver por nombre un tema que nadie le ha pasado, y
/// dos listas —una para pintar y otra para enseñar— es exactamente lo que el
/// comentario original decía que no podía pasar. Quien hospeda la consume.
#[must_use]
pub fn roles_de_tema(theme: &norte_theme::Theme) -> Vec<(String, String)> {
    CORRESPONDENCIA
        .iter()
        .filter_map(|&(nombre, role, fondo)| {
            let style = theme.style(role);
            let color = if fondo { style.bg } else { style.fg };
            color.map(|c| (nombre.to_owned(), c.to_hex()))
        })
        .collect()
}

/// El tema que esta ventana tiene puesto.
///
/// La correspondencia rol → color es [`roles_de_tema`], la misma que alimenta
/// las variables CSS de quien hospeda: lo que se ve en esta pantalla es lo que
/// pinta.
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
    /// El tema que se llama así, resuelto.
    ///
    /// El nombre que se guarda es el PEDIDO, y los colores los del tema que
    /// de verdad se resolvió: con los presets de fábrica son siempre el
    /// mismo, y quien la llama ya comprobó que existe.
    #[must_use]
    pub fn de(nombre: &str, theme: &norte_theme::Theme) -> Self {
        Self {
            name: nombre.to_owned(),
            roles: roles_de_tema(theme),
            // Los efectos que la ventana SÍ interpreta no se enseñan como
            // «sin soporte»: `backdrop` (spec 2026-09-11, V6) lo traduce el
            // catálogo de la ventana a una variable CSS.
            effects: theme
                .effect_names()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| !EFECTOS_DE_LA_VENTANA.contains(&e.as_str()))
                .collect(),
        }
    }

    /// La proyección.
    #[must_use]
    pub(crate) fn vista(&self) -> ThemeView {
        ThemeView {
            name: clamp_display(self.name.clone()),
            // La lista y el cursor los pone quien tiene el SELECTOR: este
            // tipo es el tema puesto, no la elección en curso.
            choices: Vec::new(),
            cursor: 0,
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
    /// Es el de FAVORITOS, la única lista de esta ventana que se EDITA (#309).
    ///
    /// Un bool y no un `kind` con cinco variantes: lo que se pregunta aquí no
    /// es qué lista es sino si `dialog.add`/`dialog.remove` significan algo
    /// sobre ella, y hoy solo hay una de la que sea cierto.
    hotlist: bool,
}

/// Una fila con lo que hace falta para ACTUAR, además de para pintar.
struct Fila {
    vista: PickerRowView,
    /// A dónde navega.
    destino: Option<VPath>,
    /// El nombre CRUDO, cuando la fila se puede editar (#309): es la clave con
    /// la que un favorito se quita del `norte.toml`, y no puede salir de la
    /// etiqueta, que va saneada y recortada para pintarse.
    nombre: Option<String>,
}

impl Selector {
    /// El selector de volúmenes, todavía sin la lista: se pide y llega.
    pub(crate) fn volumenes(slot: u32) -> Self {
        Self::volumenes_con_titulo(slot, "picker-volumes-title")
    }

    /// El selector de volúmenes de un LADO de la pantalla.
    ///
    /// El título lo dice, porque nada más puede decirlo: el slot no cruza el
    /// puente y los dos lados abren la misma lista. En Total Commander lo
    /// dice la posición de la ventana; aquí, con el foco en el otro panel,
    /// sin el título no hay forma de saber dónde se va a montar hasta que se
    /// monta (ADR 0058 D9, #293).
    pub(crate) fn volumenes_de_lado(slot: u32, derecha: bool) -> Self {
        Self::volumenes_con_titulo(
            slot,
            if derecha {
                "picker-volumes-title-right"
            } else {
                "picker-volumes-title-left"
            },
        )
    }

    fn volumenes_con_titulo(slot: u32, titulo: &'static str) -> Self {
        Self {
            filas: Vec::new(),
            cursor: 0,
            vacio: "picker-volumes-loading",
            titulo,
            slot,
            hotlist: false,
        }
    }

    /// El selector de CONEXIONES, todavía sin la lista: se pide al daemon y
    /// llega (#264).
    ///
    /// Vacío al abrir, como el de volúmenes y con la misma carrera: la lista
    /// viene de una respuesta, así que su `generation` es lo que impide que un
    /// click pintado sobre una lista se atienda sobre otra.
    pub(crate) fn conexiones(slot: u32) -> Self {
        Self {
            filas: Vec::new(),
            cursor: 0,
            vacio: "picker-connections-loading",
            titulo: "picker-connections-title",
            slot,
            hotlist: false,
        }
    }

    /// Rellena el selector de conexiones con lo que contestó el daemon.
    ///
    /// **La URL se enmascara como una autoridad y no como una ruta**: un host
    /// puede llamarse `banco.example@malo.example` sin llevar un solo carácter
    /// que se enmascare, y eso se lee como userinfo de un host legítimo. Es el
    /// mismo cuidado que el aviso de sesión degradada, y por el mismo motivo:
    /// aquí «¿a qué máquina me estoy conectando?» es la única pregunta.
    ///
    /// Lo que se navega es la URL: ir ahí ESTABLECE la sesión por el camino de
    /// siempre. Una que no parsea como `VPath` se enseña sin destino — se ve
    /// que está configurada y que no se puede abrir, que es más honesto que
    /// esconderla.
    pub(crate) fn con_conexiones(
        &mut self,
        conexiones: Vec<norte_proto::methods::ConnectionEntry>,
    ) {
        self.filas = conexiones
            .into_iter()
            .map(|c| {
                let (nombre, nombre_hostil) = norte_frontend::display_name(c.name.as_bytes());
                let (url, url_hostil) = norte_frontend::display_name(c.url.as_bytes());
                Fila {
                    vista: PickerRowView {
                        label: clamp_display(nombre),
                        hostile: nombre_hostil || url_hostil,
                        detail: clamp_display(url),
                    },
                    destino: VPath::parse(&c.url).ok(),
                    nombre: None,
                }
            })
            .collect();
        self.cursor = 0;
        self.vacio = if self.filas.is_empty() {
            "picker-connections-empty"
        } else {
            ""
        };
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
                    nombre: None,
                }
            })
            .collect();
        Self {
            filas,
            cursor: 0,
            vacio: "picker-history-empty",
            titulo: "picker-history-title",
            slot,
            hotlist: false,
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
                    // El nombre CRUDO viaja con la fila: es con lo que se
                    // quita el favorito del `norte.toml` (#309).
                    nombre: Some(nombre.clone()),
                }
            })
            .collect();
        Self {
            filas,
            cursor: 0,
            vacio: "picker-hotlist-empty",
            titulo: "picker-hotlist-title",
            slot,
            hotlist: true,
        }
    }

    /// A qué hueco navega lo que se elija aquí.
    pub(crate) fn slot(&self) -> u32 {
        self.slot
    }

    /// ¿Es el selector de FAVORITOS? (#309)
    ///
    /// Lo pregunta quien atiende `dialog.add`/`dialog.remove`: los favoritos
    /// son la única lista de esta ventana que se edita —los volúmenes los
    /// monta el sistema y las disposiciones se guardan por otro camino—, así
    /// que esos dos verbos solo significan algo aquí.
    pub(crate) fn es_hotlist(&self) -> bool {
        self.hotlist
    }

    /// El NOMBRE de la fila del cursor, sin pintar.
    ///
    /// Crudo y no la etiqueta de la vista: lo que se pinta va saneado y
    /// recortado, y quitar un favorito por su etiqueta borraría el que no era
    /// —o ninguno— en cuanto el nombre llevara bidi o midiera de más.
    pub(crate) fn nombre_crudo(&self) -> Option<&str> {
        self.filas.get(self.cursor)?.nombre.as_deref()
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
                    nombre: None,
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
    // El espacio y el solo-lectura, por el crate COMPARTIDO: aquí y en la
    // barra lateral estaban escritos aparte y ya diferían.
    trozos.push(norte_frontend::places::PlacesState::volume_detail(
        v.free_bytes,
        v.total_bytes,
        v.read_only,
        false,
        lang,
    ));
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

#[cfg(test)]
mod tests {
    use super::{nombres_de_tema, roles_de_tema};

    /// **Un rol de PAREJA cruza con sus dos mitades.**
    ///
    /// `Role::StatusBar` es fondo Y texto: el terminal lo aplica como estilo
    /// entero. Aquí solo viajaba el fondo, así que todo lo que la ventana
    /// pintase encima tenía que ADIVINAR el color del texto — la cabecera del
    /// visor adivinaba `title-fg`, y con un tema cuya barra de estado es clara
    /// eso es claro sobre claro: la ruta, el encoding, el EOL y las pérdidas
    /// salían invisibles. Se vio pintando la ventana de verdad, no en un test.
    ///
    /// La lista se comprueba entera y a mano, por lo que dice el rustdoc de
    /// `roles_de_tema`: es un acuerdo con una hoja de estilos que no comparte
    /// tipos, así que quitar una clave tiene que ponerse rojo aquí en vez de
    /// descubrirse mirando la pantalla.
    #[test]
    fn el_tema_cruza_las_dos_mitades_de_la_barra_de_estado() {
        let theme = norte_theme::Theme::preset_default();
        let roles = roles_de_tema(&theme);
        let nombres: Vec<&str> = roles.iter().map(|(n, _)| n.as_str()).collect();

        for mitad in ["status-bg", "status-fg"] {
            assert!(
                nombres.contains(&mitad),
                "falta `{mitad}`: sin las dos, quien pinte encima adivina — \
                 y adivinó claro sobre claro ({nombres:?})"
            );
        }

        assert_eq!(
            nombres,
            [
                "bg",
                "fg",
                "panel-bg",
                "panel-focus-bg",
                "border",
                "border-focus",
                "selection-bg",
                "selection-fg",
                "selection-unfocused-bg",
                "selection-unfocused-fg",
                "button-bg",
                "button-fg",
                "mark-bg",
                "hostile-fg",
                "status-bg",
                "status-fg",
                "title-fg",
                "error-fg",
                "warning-fg",
                "info-fg",
            ],
            "lo que el preset por defecto PROYECTA: calla los diez de cromo, \
             que la hoja deriva"
        );

        // El acuerdo con `style.css` es `nombres_de_tema`, no lo de arriba:
        // los nombres existen aunque el tema no los llene, y confundir las
        // dos cosas es lo que hacía que el guardián de huérfanas del renderer
        // leyera «nadie alimenta esto» donde en realidad ponía «este tema no
        // lo dice».
        let nombres_todos = nombres_de_tema();
        for n in &nombres {
            assert!(
                nombres_todos.contains(n),
                "`{n}` se proyecta y no está en el acuerdo"
            );
        }
        for cromo in [
            "hover",
            "input-bg",
            "input-border",
            "widget-bg",
            "widget-shadow",
            "badge-bg",
            "badge-fg",
            "scrollbar-slider",
            "separator",
            "focus-border",
            "muted",
        ] {
            assert!(
                nombres_todos.contains(&cromo),
                "falta `{cromo}` en el acuerdo con la hoja"
            );
            assert!(
                !nombres.contains(&cromo),
                "`{cromo}` no debería proyectarse: el preset por defecto no \
                 lo define, y la hoja lo deriva"
            );
        }

        // Y cada uno lleva un color de verdad, no una cadena vacía que la
        // hoja aceptaría en silencio.
        for (nombre, color) in &roles {
            assert!(
                color.starts_with('#') && color.len() == 7,
                "`{nombre}` no es un color: {color:?}"
            );
        }
    }
}
