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
                    color: clamp_display(color.clone()),
                })
                .collect(),
            unsupported_effects: self
                .effects
                .iter()
                .map(|e| clamp_display(norte_frontend::display_name(e.as_bytes()).0))
                .collect(),
        }
    }
}

/// Un selector abierto.
pub(crate) struct Selector {
    filas: Vec<Fila>,
    cursor: usize,
    /// La lista está vacía y esta es la clave Fluent que lo explica.
    vacio: &'static str,
}

/// Una fila con lo que hace falta para ACTUAR, además de para pintar.
struct Fila {
    vista: PickerRowView,
    /// A dónde navega.
    destino: Option<VPath>,
}

impl Selector {
    /// El selector de volúmenes, todavía sin la lista: se pide y llega.
    pub(crate) fn volumenes() -> Self {
        Self {
            filas: Vec::new(),
            cursor: 0,
            vacio: "picker-volumes-loading",
        }
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
                Fila {
                    vista: PickerRowView {
                        label: clamp_display(pintable),
                        hostile,
                        detail: clamp_display(detalle_de(v, lang)),
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

    /// A dónde navega la fila del cursor, si hay alguna.
    pub(crate) fn elegir(&self) -> Option<VPath> {
        self.filas.get(self.cursor)?.destino.clone()
    }

    /// La proyección.
    pub(crate) fn vista(&self, lang: Lang) -> PickerView {
        PickerView {
            title: clamp_display(norte_i18n::t_in(lang, "picker-volumes-title")),
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
fn detalle_de(v: &norte_proto::methods::Volume, lang: Lang) -> String {
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
    if let Some(label) = &v.label {
        trozos.push(norte_frontend::display_name(label).0);
    }
    trozos.join(" · ")
}
