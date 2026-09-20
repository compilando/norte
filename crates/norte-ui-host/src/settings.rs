//! Los ajustes (F11 / `app.settings`) vistos desde el host: qué hay
//! configurado, de dónde sale, y cambiarlo.
//!
//! Nada de esto lo decide este módulo. El registro de ajustes, el valor
//! efectivo de cada entrada, su texto localizado y la MÁQUINA de edición
//! —girar un booleano, validar un entero— son `norte_frontend::settings`: el
//! mismo catálogo y el mismo editor que el terminal, con los mismos ids
//! estables. Lo que se aporta aquí es la PROYECCIÓN al vocabulario del
//! bridge, una sección que no es configuración sino diagnóstico —dónde vive
//! cada cosa—, y la forma de pedir un valor: el terminal teclea en línea, y
//! la ventana abre el diálogo de un campo, que es su forma de preguntar.

use std::path::PathBuf;

use norte_frontend::settings::{
    PendingWrite, Row, Section, SettingsEditError, SettingsState, build_rows_in,
};
use norte_i18n::Lang;

use crate::bridge::clamp_display;
use crate::dto::{
    PathRowView, SectionIndexView, SettingRowView, SettingsSectionView, SettingsView,
};

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
    /// `<config>/profiles/<nombre>`, la capa que el lector ELIGE por nombre
    /// (spec 2026-08-26, D1).
    Profile,
    /// `./.norte`, solo tras trust (ADR 0026).
    Project,
}

impl ConfigLayer {
    /// La clave Fluent de su nombre.
    fn label_id(self) -> &'static str {
        match self {
            Self::System => "settings-path-config-system",
            Self::User => "settings-path-config-user",
            Self::Profile => "settings-path-config-profile",
            Self::Project => "settings-path-config-project",
        }
    }
}

/// Una ubicación que la ventana puede enseñar, con su existencia YA resuelta.
///
/// El `missing` viene de fuera a propósito. Saber si un directorio está es
/// `std::fs::metadata`, o sea I/O bloqueante, y esta proyección corre en el
/// bucle del ÚNICO ESCRITOR: con una capa de configuración en un NFS colgado,
/// abrir los ajustes congelaba la ventana entera —ni teclas, ni listados
/// aterrizando, ni progreso de tasks— hasta que expirase el montaje. Es la
/// regla 2, y el arranque ya tiene un `spawn_blocking` donde hacerlo bien.
#[derive(Debug, Clone)]
pub struct HostPath {
    /// La ruta, en bytes nativos. Se pinta con `display_os_name`.
    pub path: PathBuf,
    /// No existe. Una capa que nadie ha creado se DICE, en vez de pintar una
    /// ruta que parece estar ahí.
    pub missing: bool,
}

/// Dónde vive cada cosa, tal como lo resolvió quien arrancó el host.
///
/// Se recibe ya resuelto a propósito. El host no lee ficheros ni consulta el
/// entorno: si lo hiciera, una ventana podría acabar diciendo que su
/// configuración está en un sitio distinto de donde la leyó de verdad — y lo
/// haría bloqueando el actor.
#[derive(Debug, Clone, Default)]
pub struct HostPaths {
    /// Las capas de configuración, en precedencia ASCENDENTE.
    pub config_layers: Vec<(ConfigLayer, HostPath)>,
    /// El directorio de estado (sesión, historial).
    pub state_dir: Option<HostPath>,
    /// Dónde escribe sus logs esta ventana.
    pub logs_dir: Option<HostPath>,
    /// El socket del daemon con el que habla.
    pub socket: Option<HostPath>,
}

/// Los ajustes abiertos: el modelo mínimo, que es un cursor.
///
/// No hay más estado porque no hay edición. Cuando la fase 5 la traiga, lo
/// Lo que hace Enter (o el doble clic) sobre la fila del cursor.
#[derive(Debug)]
pub(crate) enum Activacion {
    /// Nada que activar: una ruta, o ninguna fila.
    Nada,
    /// La fila giró sola —booleano, enumerado, tema, preset— y esto es lo que
    /// hay que escribir. En caja: un `PendingWrite` lleva un `toml_edit::Value`
    /// y es grande al lado de las otras variantes.
    Escribir(Box<PendingWrite>),
    /// La fila quiere un valor tecleado: `nombre` y `actual` para el diálogo
    /// que lo pide.
    PedirTexto {
        /// Cómo se llama la entrada, ya traducido.
        nombre: String,
        /// Qué dice ahora.
        actual: String,
        /// Sobre qué fila PLANA se preguntó, para volver a ella al confirmar.
        fila: usize,
    },
}

/// Los ajustes abiertos: el editor compartido más las ubicaciones.
///
/// El cursor es sobre la lista PLANA —las entradas del registro y después
/// las rutas—, y se proyecta sobre el del editor solo cuando toca activar
/// una entrada: el editor no sabe de rutas, y no tiene por qué.
pub(crate) struct Ajustes {
    /// El editor compartido con el terminal, sobre las filas del registro.
    ///
    /// Sin filtro: el terminal filtra tecleando porque su overlay se come
    /// todo imprimible, y aquí las teclas imprimibles no llegan al host. Lo
    /// que cuenta es que girar, validar y proponer un valor sea UNA máquina.
    estado: SettingsState,
    /// Las ubicaciones, ya saneadas.
    rutas: Vec<PathRowView>,
    /// Qué fila tiene el cursor, sobre la lista PLANA.
    cursor: usize,
}

impl Ajustes {
    /// Abre la vista con la configuración que el host tiene puesta.
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
        Self {
            estado: SettingsState::new(filas_de(cfg, lang)),
            rutas: rutas_de(paths, lang),
            cursor: 0,
        }
    }

    /// Las filas se vuelven a construir sobre la configuración RECARGADA,
    /// con el cursor donde estaba.
    ///
    /// Es lo que le pasa al terminal en cada recarga en caliente, y por lo
    /// mismo: la fila que se acaba de girar ya enseña el valor nuevo
    /// (optimista), y esto la deja diciendo lo que el fichero dice de verdad.
    pub(crate) fn refrescar(&mut self, cfg: &norte_frontend::config::FrontendConfig, lang: Lang) {
        self.estado.refresh(filas_de(cfg, lang));
    }

    /// Cuántas filas elegibles hay AHORA: las que el filtro deja ver, más
    /// las ubicaciones, que no se filtran (son diagnóstico, no ajustes).
    fn total(&self) -> usize {
        self.estado.shown() + self.rutas.len()
    }

    /// La fila plana `fila` como posición dentro de las VISIBLES del editor,
    /// o `None` si cae en las ubicaciones (o fuera).
    ///
    /// Es la traducción que el `debug_assert` de antes decía que haría falta
    /// el día que esta ventana filtrara: con filtro puesto, la fila plana
    /// tercera no es la tercera del registro.
    fn fila_visible(&self, fila: usize) -> Option<usize> {
        (fila < self.estado.shown()).then_some(fila)
    }

    /// Pone la consulta del buscador.
    ///
    /// El cursor se re-encaja: filtrando, la fila a la que apuntaba puede
    /// haberse ido, y un cursor fuera de la lista es un Enter que activa
    /// otra cosa.
    pub(crate) fn consultar(&mut self, texto: &str) {
        self.estado.set_query(texto);
        let total = self.total();
        self.cursor = if total == 0 {
            0
        } else {
            self.cursor.min(total - 1)
        };
    }

    /// Lleva el cursor a la primera fila de una sección, nombrada por su
    /// clave estable. Una que no existe, o que el filtro vació, no mueve
    /// nada.
    pub(crate) fn saltar(&mut self, clave: &str) {
        let Some(seccion) = Section::ORDER
            .iter()
            .copied()
            .find(|s| s.stable_key() == clave)
        else {
            return;
        };
        if seccion == Section::Paths {
            // Las ubicaciones van detrás de todo y no las lleva el editor.
            if !self.rutas.is_empty() {
                self.cursor = self.estado.shown();
            }
            return;
        }
        let antes = self.estado.cursor();
        self.estado.jump_to(seccion);
        let ahora = self.estado.cursor();
        if ahora != antes || self.estado.shown() > 0 {
            self.cursor = ahora;
        }
    }

    /// ¿La fila de este id sigue diciendo que no es de fábrica?
    ///
    /// Se pregunta DESPUÉS de releer, y es lo que distingue «restablecido»
    /// de «lo fija otra capa» sin construir procedencia de capas.
    pub(crate) fn sigue_modificada(&self, id: &str) -> bool {
        self.estado
            .rows()
            .iter()
            .find(|r| r.id() == Some(id))
            .is_some_and(|r| r.modified)
    }

    /// Restablecer la fila `fila`: la clave que hay que quitar, o `None`.
    ///
    /// Una ubicación no se restablece —no es un ajuste— y una fila que ya
    /// está en su valor de fábrica tampoco.
    pub(crate) fn restablecer(
        &mut self,
        fila: usize,
    ) -> Option<norte_frontend::settings::PendingReset> {
        let visible = self.fila_visible(fila)?;
        self.estado.set_cursor(visible);
        self.estado.reset()
    }

    /// Enter sobre la fila del cursor.
    ///
    /// Las listas de temas y presets llegan de fuera y VIVAS, como en el
    /// terminal: el tema efectivo puede haber cambiado en caliente.
    pub(crate) fn activar(&mut self, temas: &[String], presets: &[&str]) -> Activacion {
        // De fila PLANA a fila VISIBLE: con el buscador puesto, la tercera
        // fila de la pantalla no es la tercera del registro.
        let Some(fila) = self.fila_visible(self.cursor) else {
            return Activacion::Nada;
        };
        self.estado.set_cursor(fila);
        if let Some(write) = self.estado.activate(temas, presets) {
            return Activacion::Escribir(Box::new(write));
        }
        if !self.estado.is_editing() {
            return Activacion::Nada;
        }
        // La ventana no teclea en línea: pregunta con un diálogo, y el valor
        // vuelve ENTERO al confirmar. Hasta entonces el editor no queda a
        // medias — `confirmar_texto` vuelve a abrir la edición sobre la
        // misma fila, y un diálogo cancelado no deja nada que cerrar.
        let actual = self.estado.edit_buffer().unwrap_or_default().to_owned();
        self.estado.edit_cancel();
        let nombre = self.estado.rows()[fila].name.clone();
        Activacion::PedirTexto {
            nombre,
            actual,
            fila,
        }
    }

    /// El valor que el diálogo trajo para la fila `fila`.
    ///
    /// Vuelve a entrar en la edición de esa fila, pone el texto entero y
    /// confirma: la validación —rango de un entero, forma de una línea de
    /// órdenes— es la del editor compartido, no una copia.
    ///
    /// # Errors
    /// Lo que el editor rechaza, sin escribir nada. Una fila que ya no pide
    /// texto —el registro cambió bajo el diálogo— se rechaza como un entero
    /// inválido: es el fallo inerte del editor, y no hay nada que escribir.
    pub(crate) fn confirmar_texto(
        &mut self,
        fila: usize,
        texto: &str,
    ) -> Result<PendingWrite, SettingsEditError> {
        // La fila viaja PLANA en el diálogo, como salió: se vuelve a
        // traducir a visible aquí, porque el filtro puede haber cambiado
        // mientras el diálogo estaba abierto.
        let Some(visible) = self.fila_visible(fila) else {
            return Err(SettingsEditError::NotAnInt);
        };
        self.estado.set_cursor(visible);
        // Sin listas: una fila de texto no las mira, y una que las mirara
        // giraría en vez de editar, que es justo lo que el guard de abajo
        // rechaza. Con la lista vacía `cycle` devuelve el valor que había, así
        // que el `PendingWrite` que se descarta aquí era además un no-op.
        if self.estado.activate(&[], &[]).is_some() || !self.estado.is_editing() {
            self.estado.edit_cancel();
            return Err(SettingsEditError::NotAnInt);
        }
        self.estado.edit_set(texto);
        let salida = self.estado.edit_commit();
        // Un rechazo deja el buffer abierto en el editor (el terminal lo
        // conserva para corregirlo); aquí el diálogo ya se cerró, y una
        // edición colgada haría que el siguiente Enter no girase.
        self.estado.edit_cancel();
        salida
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

    /// La proyección: una sección por cada una que tenga filas, en el orden
    /// de la pantalla, más el índice y las dos cifras del buscador.
    ///
    /// Una sección que el FILTRO vació sigue en el índice, apagada; una que
    /// esta superficie no tiene no aparece. Las ubicaciones van al final y
    /// no las toca el filtro: son diagnóstico, no ajustes.
    pub(crate) fn vista(&self, lang: Lang) -> SettingsView {
        let indice = self.estado.sections();
        let mut sections = Vec::new();
        for v in &indice {
            if v.section == Section::Paths || v.total == 0 {
                continue;
            }
            let filas: Vec<_> = self
                .estado
                .visible()
                .iter()
                .map(|&i| &self.estado.rows()[i])
                .filter(|r| r.section == v.section)
                .map(proyectar_fila)
                .collect();
            if filas.is_empty() {
                continue;
            }
            sections.push(SettingsSectionView::Settings {
                title: clamp_display(norte_i18n::t_in(lang, v.section.label_key())),
                rows: filas,
            });
        }
        if !self.rutas.is_empty() {
            sections.push(SettingsSectionView::Paths {
                title: clamp_display(norte_i18n::t_in(lang, "settings-section-paths")),
                rows: self.rutas.clone(),
            });
        }
        let mut index: Vec<SectionIndexView> = indice
            .iter()
            .filter(|v| v.section != Section::Paths && v.total > 0)
            .map(|v| SectionIndexView {
                key: v.section.stable_key().to_owned(),
                title: clamp_display(norte_i18n::t_in(lang, v.section.label_key())),
                visible: v.visible as u64,
            })
            .collect();
        if !self.rutas.is_empty() {
            index.push(SectionIndexView {
                key: Section::Paths.stable_key().to_owned(),
                title: clamp_display(norte_i18n::t_in(lang, "settings-section-paths")),
                visible: self.rutas.len() as u64,
            });
        }
        SettingsView {
            sections,
            index,
            cursor: self.cursor as u64,
            query: clamp_display(self.estado.query_display()),
            shown: self.estado.shown() as u64,
            total: self.estado.total() as u64,
        }
    }
}

/// Las filas del registro, en el idioma del HOST.
///
/// Los títulos de sección ya iban con él y el nombre y la descripción de
/// cada opción con el del proceso: media pantalla en cada idioma es peor que
/// ninguna traducción.
fn filas_de(cfg: &norte_frontend::config::FrontendConfig, lang: Lang) -> Vec<Row> {
    build_rows_in(cfg, &[], lang)
        .into_iter()
        .filter(|r| !r.is_plugins_note())
        .collect()
}

/// Lo que la ventana NO puede aplicar sin reiniciar, por id del catálogo.
///
/// El catálogo compartido dice qué se aplica en caliente desde el punto de
/// vista del terminal, que recarga todo. La ventana relee la configuración
/// ENTERA al escribir un ajuste (`aplicar_config`), y casi todo se lee en el
/// momento de usarse —las barras al proyectar cada foto, el editor y el
/// comparador al lanzarlos, el modo de búsqueda al buscar, si pregunta al
/// salir al salir—, así que cambia al instante. Lo que no: lo que quien
/// hospeda resuelve una vez al arrancar —el idioma, las fuentes, el
/// movimiento reducido, que es lo que `fuera_de_alcance_en_caliente`
/// nombra—, y lo que se fija al crear cada hueco —los ocultos y la fila
/// `..`—, que los huecos ya abiertos no releen. Marcar TODO lo demás como
/// «requiere reinicio» era mentir dieciséis veces en una pantalla.
fn pide_reinicio(id: &str) -> bool {
    matches!(
        id,
        "ui.lang"
            | "ui.font"
            | "ui.mono-font"
            | "ui.font-size"
            | "ui.reduce-motion"
            | "ui.show-hidden"
            | "ui.parent-entry"
    )
}

/// Una fila del registro, proyectada.
fn proyectar_fila(r: &Row) -> SettingRowView {
    let (valor, hostile) = norte_frontend::display_name(r.value.as_bytes());
    SettingRowView {
        // El id es una IDENTIDAD del catálogo compartido, no prosa: viaja
        // entero, sin recorte, y el renderer no lo pinta.
        id: r.id().unwrap_or_default().to_owned(),
        name: clamp_display(r.name.clone()),
        desc: clamp_display(r.desc.clone()),
        // El VALOR sale de `norte.toml` tal cual —`ui.font`, `ui.theme`,
        // `keymap.preset` son cadenas que escribe el usuario, y la capa de
        // PROYECTO es «he abierto este repositorio», no «doy fe de esta
        // cadena» (ADR 0026)—. Era el único sitio de esta ventana donde texto
        // de fuera llegaba al DOM sin pasar por la máscara.
        value: clamp_display(valor),
        hostile,
        // Por id y no por `SettingDef::applies_live`: ese campo está escrito
        // desde el punto de vista del terminal, que recarga todo en caliente,
        // y esta ventana solo recarga lo que el cambio de perfil sabe aplicar.
        // Decir que una entrada se aplica sola cuando no lo hace es la clase
        // de mentira que manda al usuario a buscar un bug que no existe.
        restart_required: r.id().is_none_or(pide_reinicio),
        modified: r.modified,
    }
}

/// Las ubicaciones, saneadas para pintar.
///
/// CERO I/O: la existencia de cada sitio la trae [`HostPath`] ya resuelta por
/// el arranque. Es lo que hace verdad que «el host no lee ficheros», que este
/// módulo decía tres veces mientras llamaba a `exists()`.
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
fn fila_de_ruta(label: String, dir: &HostPath) -> PathRowView {
    let (pintable, hostile) = norte_frontend::display::display_os_name(dir.path.as_os_str());
    PathRowView {
        label: clamp_display(label),
        display: clamp_display(pintable),
        hostile,
        missing: dir.missing,
    }
}
