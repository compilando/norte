//! Los ajustes (F9 / `app.settings`) vistos desde el host: qué hay
//! configurado y de dónde sale.
//!
//! Nada de esto lo decide este módulo. El registro de ajustes, el valor
//! efectivo de cada entrada y su texto localizado son
//! `norte_frontend::settings` — el mismo catálogo, con los mismos ids
//! estables, que pinta el TUI. Lo que se aporta aquí es la PROYECCIÓN al
//! vocabulario del bridge, más una sección que no es configuración sino
//! diagnóstico: dónde vive cada cosa.
//!
//! **Solo lectura, y se dice.** Esta ventana no escribe ajustes hasta que la
//! fase 5 le dé el camino seguro, así que la vista lo ANUNCIA en vez de
//! ofrecer un `enter` que se negaría. El estado editor compartido
//! (`settings::SettingsState`) no se usa: es una máquina de edición, y aquí no
//! se edita.

use std::path::PathBuf;

use norte_frontend::settings::{Row, build_rows};
use norte_i18n::Lang;

use crate::bridge::clamp_display;
use crate::dto::{PathRowView, SettingRowView, SettingsSectionView, SettingsView};

/// Una capa de configuración, nombrada como la nombra el usuario.
///
/// Espejo de `norte_config::Layer` sin depender de ese crate: el host no
/// descubre ficheros —quien lo arranca ya resolvió las capas— y arrastrar el
/// buscador de directorios aquí sería darle una segunda idea de dónde vive la
/// configuración (ADR 0066, decisión D14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigLayer {
    /// `/etc/norte` (o `%ProgramData%\norte`).
    System,
    /// `$XDG_CONFIG_HOME/norte`.
    User,
    /// `./.norte`, solo tras trust (ADR 0026).
    Project,
}

impl ConfigLayer {
    /// La clave Fluent de su nombre.
    fn label_id(self) -> &'static str {
        match self {
            Self::System => "settings-path-config-system",
            Self::User => "settings-path-config-user",
            Self::Project => "settings-path-config-project",
        }
    }
}

/// Dónde vive cada cosa, tal como lo resolvió quien arrancó el host.
///
/// Se recibe ya resuelto a propósito. El host no lee ficheros ni consulta el
/// entorno: si lo hiciera, una ventana podría acabar diciendo que su
/// configuración está en un sitio distinto de donde la leyó de verdad.
#[derive(Debug, Clone, Default)]
pub struct HostPaths {
    /// Las capas de configuración, en precedencia ASCENDENTE.
    pub config_layers: Vec<(ConfigLayer, PathBuf)>,
    /// El directorio de estado (sesión, historial).
    pub state_dir: Option<PathBuf>,
    /// Dónde escribe sus logs esta ventana.
    pub logs_dir: Option<PathBuf>,
    /// El socket del daemon con el que habla.
    pub socket: Option<PathBuf>,
}

/// Los ajustes abiertos: el modelo mínimo, que es un cursor.
///
/// No hay más estado porque no hay edición. Cuando la fase 5 la traiga, lo
/// que entra aquí es `settings::SettingsState`, que ya existe y ya está
/// probado — no una segunda máquina.
pub(crate) struct Ajustes {
    /// Las filas del registro, congeladas al abrir. Se construyen una vez,
    /// como las de la paleta y por el mismo motivo: `build_rows` resuelve el
    /// valor efectivo de cada entrada y formatea dos cadenas Fluent por fila.
    filas: Vec<Row>,
    /// Las ubicaciones, ya saneadas.
    rutas: Vec<PathRowView>,
    /// Qué fila tiene el cursor, sobre la lista PLANA.
    cursor: usize,
}

impl Ajustes {
    /// Abre la vista con la configuración que el host recibió al arrancar.
    ///
    /// La sección de plugins del modelo compartido se deja fuera: sus filas
    /// necesitan el esquema `[config]` de cada extensión, que llega con la
    /// siguiente rebanada. Enseñar su fila de «ninguna extensión declara
    /// ajustes» sin haber preguntado sería afirmar algo que no se ha mirado.
    pub(crate) fn abrir(
        cfg: &norte_frontend::config::FrontendConfig,
        paths: &HostPaths,
        lang: Lang,
    ) -> Self {
        let filas = build_rows(cfg, &[])
            .into_iter()
            .filter(|r| !r.is_plugins_note())
            .collect();
        Self {
            filas,
            rutas: rutas_de(paths, lang),
            cursor: 0,
        }
    }

    /// Cuántas filas elegibles hay en total.
    fn total(&self) -> usize {
        self.filas.len() + self.rutas.len()
    }

    /// Mueve el cursor `delta` filas, sin salirse.
    pub(crate) fn mover(&mut self, delta: i64) {
        let total = self.total();
        if total == 0 {
            return;
        }
        let destino = i64::try_from(self.cursor)
            .unwrap_or(0)
            .saturating_add(delta);
        self.cursor = usize::try_from(destino.max(0)).unwrap_or(0).min(total - 1);
    }

    /// Pone el cursor en una fila concreta (un click). Fuera de rango no hace
    /// nada: quien pinta puede ir un frame por detrás.
    pub(crate) fn senalar(&mut self, fila: usize) {
        if fila < self.total() {
            self.cursor = fila;
        }
    }

    /// La proyección.
    pub(crate) fn vista(&self, lang: Lang) -> SettingsView {
        let general = SettingsSectionView::Settings {
            title: clamp_display(norte_i18n::t_in(lang, "settings-section-general")),
            rows: self.filas.iter().map(proyectar_fila).collect(),
        };
        let rutas = SettingsSectionView::Paths {
            title: clamp_display(norte_i18n::t_in(lang, "settings-section-paths")),
            rows: self.rutas.clone(),
        };
        SettingsView {
            sections: vec![general, rutas],
            cursor: self.cursor as u64,
            read_only: true,
        }
    }
}

/// Una fila del registro, proyectada.
fn proyectar_fila(r: &Row) -> SettingRowView {
    SettingRowView {
        // El id es una IDENTIDAD del catálogo compartido, no prosa: viaja
        // entero, sin recorte, y el renderer no lo pinta.
        id: r.id().unwrap_or_default().to_owned(),
        name: clamp_display(r.name.clone()),
        desc: clamp_display(r.desc.clone()),
        value: clamp_display(r.value.clone()),
        // TODAS, hoy. `SettingDef::applies_live` está escrito desde el punto
        // de vista del TUI, que recarga en caliente; esta ventana resuelve
        // catálogo, tema y keymaps UNA vez al arrancar y no tiene camino de
        // recarga, así que cualquier cambio pide reiniciarla. Decir que una
        // entrada se aplica sola cuando no lo hace es la clase de mentira que
        // manda al usuario a buscar un bug que no existe.
        restart_required: true,
    }
}

/// Las ubicaciones, saneadas para pintar.
///
/// Se comprueba si CADA sitio existe: una capa que nadie ha creado se dice
/// que falta en vez de pintar una ruta que parece estar ahí. Es la única I/O
/// de este módulo y es un `exists()` sobre rutas que el arranque ya nombró.
fn rutas_de(paths: &HostPaths, lang: Lang) -> Vec<PathRowView> {
    let mut out = Vec::new();
    for (capa, dir) in &paths.config_layers {
        out.push(fila_de_ruta(norte_i18n::t_in(lang, capa.label_id()), dir));
    }
    for (clave, dir) in [
        ("settings-path-state", paths.state_dir.as_ref()),
        ("settings-path-logs", paths.logs_dir.as_ref()),
        ("settings-path-socket", paths.socket.as_ref()),
    ] {
        if let Some(d) = dir {
            out.push(fila_de_ruta(norte_i18n::t_in(lang, clave), d));
        }
    }
    out
}

/// Una ubicación: el texto ya enmascarado, si difiere del real, y si está.
///
/// Un path es BYTES y no una cadena (regla 1), así que se pinta por el mismo
/// camino que un nombre de fichero del listado — `display_name` sobre los
/// bytes nativos — y NUNCA por `to_string_lossy`, que se come la diferencia
/// entre un nombre raro y uno hostil sin decirlo.
fn fila_de_ruta(label: String, dir: &std::path::Path) -> PathRowView {
    let (pintable, hostile) = norte_frontend::display::display_os_name(dir.as_os_str());
    PathRowView {
        label: clamp_display(label),
        display: clamp_display(pintable),
        hostile,
        missing: !dir.exists(),
    }
}
