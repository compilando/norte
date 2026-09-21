//! Settings registry (S2) + the shared pure editor state machine (S3/S4
//! hoist): a CURATED, Fluent-localized catalog of general settings, shared
//! by the TUI overlay (S3) and the GUI full-view swap (S4) — NOT parsed from
//! the JSON schema at runtime (the schema's descriptions are English
//! rustdoc; the UI must localize).
//!
//! Every entry resolves two Fluent keys ([`fluent_name_id`]/
//! [`fluent_desc_id`]) that MUST exist in both locales — pinned by a
//! coverage test below (`fluent_keys_existen_en_ambos_locales_para_cada_entrada`),
//! the same "coverage over every catalog entry" discipline as the rest of
//! norte's Fluent-backed UI surfaces.
//!
//! [`build_rows`]/[`Row`]/[`SettingsState`]/[`PendingWrite`]/
//! [`SettingsEditError`] landed in the TUI first (S3, `norte-tui/src/{app,
//! settings}.rs`) with ZERO TUI-specific coupling (no ratatui/crossterm
//! types, only [`crate::nav::fold`] and [`FrontendConfig`], both already
//! shared) — S4 hoists them here rather than duplicating the same pure state
//! machine in the GUI (CLAUDE.md rule 7: business logic belongs in the core
//! or a shared frontend crate). The TUI now re-exports these names from its
//! own `app`/`settings` modules for source compatibility.

use crate::config::FrontendConfig;
use norte_i18n::t;

/// Bajo qué grupo de la pantalla de ajustes se pinta una entrada.
///
/// Nació con dos variantes —`General` y `Plugins`— y una de las dos ni
/// siquiera aparecía en [`catalog`]: las 33 entradas eran `General`, así que
/// la pantalla se leía como una lista plana con un rótulo encima. El orden
/// de [`Self::ORDER`] es el de la pantalla, y es deliberado: lo que se toca
/// el primer día arriba, lo que es diagnóstico abajo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Tema, fuentes y lo que se ve.
    Appearance,
    /// Qué enseña un panel y qué cromo lo rodea.
    Panes,
    /// Con qué programa se abre un fichero.
    OpenWith,
    /// Teclado y ratón.
    Input,
    /// Lo que norte hace sin que se lo pidan.
    Behavior,
    /// Built from an approved plugin's manifest (S3/S4) — no entries of this
    /// kind live in [`catalog`] itself.
    Plugins,
    /// Las ubicaciones (configuración, estado, logs, socket). Tampoco sale
    /// del catálogo: la proyecta quien hospeda. Es una sección del MODELO
    /// para que el índice la liste como una más y para que la terminal la
    /// gane sin copiar la proyección de la ventana.
    Paths,
}

impl Section {
    /// Las secciones en el orden en el que se pintan.
    pub const ORDER: &'static [Section] = &[
        Section::Appearance,
        Section::Panes,
        Section::OpenWith,
        Section::Input,
        Section::Behavior,
        Section::Plugins,
        Section::Paths,
    ];

    /// Su nombre ESTABLE, sin traducir.
    ///
    /// Lo acepta `@section:` en cualquier idioma, y es lo que viaja por el
    /// puente hacia la ventana: un fichero de traducción a medias no puede
    /// volver una sección inencontrable ni romper un salto.
    ///
    /// ```
    /// use norte_frontend::settings::Section;
    /// assert_eq!(Section::OpenWith.stable_key(), "open-with");
    /// ```
    #[must_use]
    pub fn stable_key(self) -> &'static str {
        match self {
            Section::Appearance => "appearance",
            Section::Panes => "panes",
            Section::OpenWith => "open-with",
            Section::Input => "input",
            Section::Behavior => "behavior",
            Section::Plugins => "plugins",
            Section::Paths => "paths",
        }
    }

    /// La clave Fluent de su rótulo, derivada de [`Self::stable_key`]: dos
    /// listas de nombres es una lista que se desincroniza.
    ///
    /// ```
    /// use norte_frontend::settings::Section;
    /// assert_eq!(Section::Appearance.label_key(), "settings-section-appearance");
    /// ```
    #[must_use]
    pub fn label_key(self) -> &'static str {
        match self {
            Section::Appearance => "settings-section-appearance",
            Section::Panes => "settings-section-panes",
            Section::OpenWith => "settings-section-open-with",
            Section::Input => "settings-section-input",
            Section::Behavior => "settings-section-behavior",
            Section::Plugins => "settings-section-plugins",
            Section::Paths => "settings-section-paths",
        }
    }

    /// La sección anterior/siguiente en [`Self::ORDER`], sin dar la vuelta.
    #[must_use]
    pub fn step(self, delta: i32) -> Option<Section> {
        let pos = Section::ORDER.iter().position(|s| *s == self)?;
        let destino = i32::try_from(pos).ok()?.checked_add(delta)?;
        let destino = usize::try_from(destino).ok()?;
        Section::ORDER.get(destino).copied()
    }
}

/// The editing widget a setting needs, and (for [`Self::Enum`]) its valid
/// values.
#[derive(Debug, Clone, Copy)]
pub enum SettingKind {
    /// Toggle.
    Bool,
    /// One of a fixed, small set of string values.
    Enum(&'static [&'static str]),
    /// Free text.
    Text,
    /// Una LÍNEA DE ÓRDENES: se teclea como texto y se guarda como ARRAY de
    /// tokens (`zed %f` → `["zed", "%f"]`).
    ///
    /// Existe porque `[ui] editor` no es una cadena en el fichero: es un argv,
    /// y guardarlo como cadena haría que la siguiente carga lo rechazara. El
    /// troceo es por espacios ASCII, la misma convención con la que `$EDITOR`
    /// admite `code -w` — el precio es un programa cuyo binario lleve un
    /// espacio, que hay que escribir en el fichero a mano.
    Args,
    /// A NUMBER in `[min, max]` — despite the name, the buffer parses as
    /// `f64` and accepts a fractional part (revisión S, M4): `ui.font-size`
    /// is the only entry using this kind, and its underlying config field
    /// (`CommonConfig::ui_font_size`) is `f32`, not an integer — a
    /// hand-edited `font_size = 14.5` was previously un-editable from this
    /// UI (the old strict `i64` parse rejected it outright). `min`/`max`
    /// stay `i64` (every bound in the catalog today is a whole number;
    /// widening them to `f64` for one entry wasn't worth the churn). The
    /// written [`toml_edit::Value`] is an Integer when the parsed number has
    /// no fractional part (keeps `norte.toml` looking the same as before
    /// for the common whole-number case) and a Float otherwise — see
    /// [`SettingsState::edit_commit`].
    Int {
        /// Inclusive lower bound.
        min: i64,
        /// Inclusive upper bound.
        max: i64,
    },
    /// A theme preset name or a path to a custom theme file (ADR 0020) —
    /// like [`Self::Enum`], but its value set comes from
    /// `norte_theme::preset_names` at render time, not a `&'static` slice.
    ThemeName,
    /// A keymap preset name — like [`Self::Enum`], but its value set comes
    /// from [`crate::keymap::presets::NAMES`] at render time.
    PresetName,
}

/// One entry of the settings registry: a stable id, which section it
/// renders under, its editing widget, and whether a live edit takes effect
/// without restarting. `applies_live` is written from the TUI's point of
/// view (S3: every entry here hot-reloads there); the GUI (S4) interprets
/// it per-frontend, and that split is documented per-entry below where it
/// applies.
///
/// The fonts used to be the example here — "they resolve once at GUI startup,
/// so the GUI marks them restart-required". They did not resolve at all: no
/// frontend read them. They now cross in the window's startup catalogue and
/// re-apply whenever it is rebuilt, which is the same path the theme takes.
/// A terminal still applies none of the four, and says so.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    /// Stable id (`section.key`, dashed — e.g. `ui.confirm-quit`), stable
    /// across releases: it is also the seed for the Fluent key pair via
    /// [`fluent_name_id`]/[`fluent_desc_id`].
    pub id: &'static str,
    /// The editing widget.
    pub kind: SettingKind,
    /// Whether a live edit applies without a restart, from the TUI's point
    /// of view (see the struct doc for the GUI's per-entry split).
    pub applies_live: bool,
}

impl SettingDef {
    /// La sección bajo la que se pinta.
    ///
    /// Sale de [`section_of`] y no de un campo por entrada: escrito 33
    /// veces al lado de cada `id`, el reparto no se puede leer de un
    /// vistazo ni auditar de una vez — que es exactamente cómo las 33
    /// entradas acabaron diciendo `General`.
    ///
    /// ```
    /// use norte_frontend::settings::{catalog, Section};
    /// let tema = catalog().iter().find(|d| d.id == "ui.theme").expect("ui.theme");
    /// assert_eq!(tema.section(), Section::Appearance);
    /// ```
    #[must_use]
    pub fn section(&self) -> Section {
        // El invariante lo fija `cada_entrada_del_catalogo_tiene_seccion`:
        // ningún id del catálogo cae aquí. Un id nuevo sin sección aterriza
        // en «Comportamiento» —visible, no escondido— y el test lo caza.
        section_of(self.id).unwrap_or(Section::Behavior)
    }
}

/// El filtro de la pantalla de ajustes, ya interpretado.
///
/// El texto libre busca donde siempre —id, nombre y descripción, plegados—,
/// y encima hay dos operadores, copiados de donde el lector ya los conoce:
/// `@modified` (solo lo que no es de fábrica) y `@section:<x>`.
///
/// `@section:` casa contra la clave ESTABLE de la sección y contra su
/// rótulo en **los dos** idiomas, no solo en el activo: un fichero de
/// traducción no puede ser la diferencia entre encontrar algo y no
/// encontrarlo.
///
/// Una arroba que no abre operador conocido es texto normal. Nadie tiene
/// que escapar nada para buscar una arroba, y un filtro que se come lo que
/// no entiende deja al lector mirando una lista vacía sin saber por qué.
#[derive(Debug, Default)]
struct Query {
    /// El texto libre, ya plegado. Vacío = no filtra por texto.
    text: String,
    /// `@modified` estaba en la consulta.
    only_modified: bool,
    /// Las secciones nombradas con `@section:`. Vacío = todas.
    sections: Vec<Section>,
    /// `@section:` nombró algo que no existe. No filtra a «todas»: filtra a
    /// NADA, que es la respuesta honesta a «enséñame los ajustes de algo que
    /// no hay». Ignorar el operador enseñaría la lista entera y el lector
    /// leería eso como «aquí está todo lo que pediste».
    imposible: bool,
}

impl Query {
    /// Interpreta la consulta cruda (bytes, como se teclean).
    fn parse(raw: &[u8]) -> Self {
        let mut q = Query::default();
        // El texto libre se conserva TAL CUAL mientras no haya operadores, y
        // eso no es pereza: `ui.font ` con el espacio final aísla una fila
        // que `ui.font` no aísla, porque el heno lleva el id seguido del
        // nombre. Trocear y volver a juntar por un espacio se come esa
        // precisión, y un filtro que enseña dos filas donde antes enseñaba
        // una es una regresión silenciosa.
        if !raw.contains(&b'@') {
            q.text = crate::nav::fold(raw);
            return q;
        }
        // Con operadores por medio: se sacan sus tokens y el resto se pliega
        // junto, ya sin la precisión del espacio de los bordes — combinar
        // `@modified` con un fragmento que dependa de un espacio final no es
        // una consulta que nadie escriba.
        let mut resto: Vec<&[u8]> = Vec::new();
        // Por bytes y separando por espacio ASCII: la consulta es entrada de
        // usuario cruda (pegado incluido) y no tiene por qué ser UTF-8
        // válido. `from_utf8_lossy` para mirar un token no lo escribe en
        // ningún sitio.
        for token in raw.split(|b| *b == b' ').filter(|t| !t.is_empty()) {
            let texto = String::from_utf8_lossy(token);
            // Plegado, como todo lo demás de esta pantalla: `@Modified` y
            // `@MODIFIED` son lo mismo, y dos operadores con dos reglas de
            // comparación es una trampa.
            if crate::nav::fold(token) == "@modified" {
                q.only_modified = true;
            } else if let Some(nombre) = texto.strip_prefix("@section:") {
                match section_by_name(nombre) {
                    Some(s) => q.sections.push(s),
                    None => q.imposible = true,
                }
            } else {
                resto.push(token);
            }
        }
        q.text = crate::nav::fold(resto.join(&b' ').as_slice());
        q
    }

    /// ¿Esta fila pasa el filtro? `fold` es su heno ya plegado.
    fn matches(&self, row: &Row, fold: &str) -> bool {
        if self.imposible {
            return false;
        }
        if self.only_modified && !row.modified {
            return false;
        }
        if !self.sections.is_empty() && !self.sections.contains(&row.section) {
            return false;
        }
        self.text.is_empty() || fold.contains(&self.text)
    }
}

/// La sección cuyo nombre estable, o cuyo rótulo en CUALQUIERA de los dos
/// idiomas, EMPIEZA por `nombre` (plegando acentos y mayúsculas).
///
/// Por prefijo y no por igualdad, por dos motivos que son el mismo: la
/// consulta se trocea por espacios, así que `@section:abrir con` solo trae
/// `abrir` —y cinco de las siete secciones tienen el rótulo de dos palabras,
/// o sea que con igualdad eran inalcanzables—, y quien teclea espera ver el
/// efecto según escribe, no al poner la última letra.
///
/// Ambigüedad: gana la primera de [`Section::ORDER`], que es el orden de la
/// pantalla. Ninguna pareja de rótulos comparte prefijo hoy en ninguno de
/// los dos idiomas.
fn section_by_name(nombre: &str) -> Option<Section> {
    let buscado = crate::nav::fold(nombre.as_bytes());
    if buscado.is_empty() {
        return None;
    }
    Section::ORDER.iter().copied().find(|s| {
        if crate::nav::fold(s.stable_key().as_bytes()).starts_with(&buscado) {
            return true;
        }
        [norte_i18n::Lang::Es, norte_i18n::Lang::En]
            .into_iter()
            .any(|l| {
                crate::nav::fold(norte_i18n::t_in(l, s.label_key()).as_bytes())
                    .starts_with(&buscado)
            })
    })
}

/// Qué mitad de la pantalla de ajustes tiene el teclado.
///
/// El mismo vocabulario que [`crate::help::Focus`], y por el mismo motivo:
/// dos listas a la vez piden decir cuál manda, y las dos pantallas que lo
/// hacen tienen que decirlo igual.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    /// La lista de ajustes: arriba/abajo recorren filas, Enter edita.
    #[default]
    List,
    /// El índice: arriba/abajo cambian de SECCIÓN, y la lista sigue.
    Index,
}

/// Una sección tal y como la pinta el índice: su rótulo ya traducido,
/// cuántas filas visibles tiene con el filtro puesto, y en cuál empieza.
///
/// Se proyecta, no se guarda: el estado es el filtro, y un índice guardado
/// al lado sería una segunda copia que se queda vieja en cuanto alguien
/// teclea una letra.
#[derive(Debug, Clone)]
pub struct SectionView {
    /// Qué sección es.
    pub section: Section,
    /// Su rótulo, traducido al idioma activo.
    pub title: String,
    /// Cuántas de sus filas se ven con el filtro puesto. Cero con
    /// [`Self::total`] mayor que cero = apagada en el índice, nunca ausente.
    pub visible: usize,
    /// Cuántas filas tiene en total, filtre lo que filtre.
    ///
    /// Distingue «la tapó el filtro» de «esta superficie no la tiene»: la
    /// terminal no proyecta ubicaciones, y un índice que anuncia una sección
    /// que nunca va a tener nada promete algo que no va a cumplir.
    pub total: usize,
    /// Posición de su primera fila visible dentro de
    /// [`SettingsState::visible`] — la unidad del cursor. `None` si el
    /// filtro la dejó vacía.
    pub first_row: Option<usize>,
}

/// La configuración DE FÁBRICA: la que sale de cero capas.
///
/// Es contra esto contra lo que se decide si una fila está «modificada», y
/// se calcula con [`current_value`], la misma función que pinta el valor —
/// una tabla de defectos escrita a mano se desincroniza del esquema en
/// cuanto alguien cambia uno.
///
/// Cacheada porque las 33 entradas se comparan contra la misma y
/// [`crate::config::load`] con cero capas no toca el disco (recorre una
/// lista vacía). Si alguna vez fallara, `None` degrada a «nada está
/// modificado»: un punto de menos es un fallo inerte, y uno de más señala
/// como tocado algo que nadie tocó.
fn factory_config() -> Option<&'static FrontendConfig> {
    static FABRICA: std::sync::OnceLock<Option<FrontendConfig>> = std::sync::OnceLock::new();
    FABRICA
        .get_or_init(|| crate::config::load(&norte_config::Layers { dirs: vec![] }).ok())
        .as_ref()
}

/// Qué clase de control pide un ajuste, y con qué valores.
///
/// Es lo que un frontend GRÁFICO necesita para pintar un interruptor en vez
/// de la palabra `true`: el terminal se apaña con el ciclo de
/// [`SettingsState::activate`], pero una ventana tiene controles de verdad y
/// no puede adivinar de qué clase es cada fila mirando su texto.
///
/// Las listas vivas —temas y presets— NO salen de aquí: las resuelve quien
/// llama, como en [`SettingsState::activate`], porque cambian en caliente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// Un interruptor.
    Toggle,
    /// Una lista cerrada, con sus valores.
    Choice(&'static [&'static str]),
    /// Una lista cuyos valores resuelve quien llama: los temas.
    ThemeChoice,
    /// Lo mismo con los presets de teclado.
    PresetChoice,
    /// Un número entre dos topes, los dos incluidos.
    Number {
        /// Tope inferior.
        min: i64,
        /// Tope superior.
        max: i64,
    },
    /// Texto libre.
    Text,
    /// Una línea de órdenes: se teclea como texto y se guarda troceada.
    Args,
}

/// El control que pide el ajuste `id`, o `None` si no es del catálogo.
///
/// ```
/// use norte_frontend::settings::{control_of, Control};
/// assert_eq!(control_of("ui.mouse"), Some(Control::Toggle));
/// assert!(matches!(control_of("ui.font-size"), Some(Control::Number { .. })));
/// assert_eq!(control_of("ni.idea"), None);
/// ```
#[must_use]
pub fn control_of(id: &str) -> Option<Control> {
    let def = catalog().iter().find(|d| d.id == id)?;
    Some(match def.kind {
        SettingKind::Bool => Control::Toggle,
        SettingKind::Enum(v) => Control::Choice(v),
        SettingKind::ThemeName => Control::ThemeChoice,
        SettingKind::PresetName => Control::PresetChoice,
        SettingKind::Int { min, max } => Control::Number { min, max },
        SettingKind::Text => Control::Text,
        SettingKind::Args => Control::Args,
    })
}

/// El valor DE FÁBRICA de una entrada, como texto de pantalla.
///
/// Es [`current_value`] sobre la configuración de cero capas: la misma
/// función que pinta el valor, no una segunda tabla de defectos que se
/// desincronice. Vacío si la configuración de fábrica no se pudo cargar,
/// que es lo mismo que enseña una fila sin valor.
///
/// Lo pinta la ventana como marcador de un campo vacío: «vacío» no es un
/// hueco, es este valor, y decir cuál informa — decirlo con una frase ocupa
/// el sitio del dato sin darlo.
///
/// ```
/// use norte_frontend::settings::{catalog, default_value};
/// let tema = catalog().iter().find(|d| d.id == "ui.theme").expect("ui.theme");
/// assert_eq!(default_value(tema), "default");
/// ```
#[must_use]
pub fn default_value(def: &SettingDef) -> String {
    factory_config()
        .map(|f| current_value(def, f))
        .unwrap_or_default()
}

/// El reparto del catálogo en secciones, en UN sitio.
///
/// `None` para un id que no es del catálogo. Un id del catálogo que
/// devuelva `None` es un bug que caza el test de cobertura: la alternativa
/// —un `_ =>` que le dé una sección cualquiera— archiva mal en silencio.
#[must_use]
pub fn section_of(id: &str) -> Option<Section> {
    let s = match id {
        "ui.theme" | "ui.theme-light" | "ui.theme-dark" | "ui.font" | "ui.mono-font"
        | "ui.font-size" | "ui.reduce-motion" | "ui.row-stripes" | "ui.images" | "ui.titlebar" => {
            Section::Appearance
        }
        "ui.show-hidden"
        | "ui.parent-entry"
        | "ui.dir-indicator"
        | "ui.pane-footer"
        | "ui.date-format"
        | "ui.panel-bar"
        | "ui.panel-bar-style"
        | "ui.panel-bar-position"
        | "ui.status-items"
        | "ui.menu-bar"
        | "ui.key-bar"
        | "ui.splash"
        | "ui.processes-panel" => Section::Panes,
        "ui.editor" | "ui.editor-detached" | "ui.diff" | "ui.diff-detached" => Section::OpenWith,
        "keymap.preset" | "ui.mouse" | "ui.alt-menu" | "ui.quick-search" => Section::Input,
        "ui.confirm-quit" | "ui.dialog-buttons" | "ui.notice-seconds" | "ui.history-size"
        | "ui.lang" => Section::Behavior,
        _ => return None,
    };
    Some(s)
}

/// The curated GENERAL settings (v1). Order is DISPLAY order (S3/S4 render
/// top to bottom before a search filter narrows it) — grouped by `norte.toml`
/// section (`[ui]` first, then `[keymap]`), not alphabetically.
const CATALOG: &[SettingDef] = &[
    SettingDef {
        id: "ui.theme",
        kind: SettingKind::ThemeName,
        applies_live: true,
    },
    SettingDef {
        id: "ui.lang",
        kind: SettingKind::Enum(&["es", "en"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.font",
        kind: SettingKind::Text,
        applies_live: true,
    },
    SettingDef {
        id: "ui.mono-font",
        kind: SettingKind::Text,
        applies_live: true,
    },
    SettingDef {
        id: "ui.font-size",
        kind: SettingKind::Int { min: 8, max: 32 },
        applies_live: true,
    },
    SettingDef {
        id: "ui.quick-search",
        kind: SettingKind::Enum(&["filter", "jump"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.reduce-motion",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // TUI only: the GUI has no terminal to share the pointer with, so
        // there is nothing there for this to turn off. It is in the CURATED
        // catalog anyway because it is the one key a user needs to find
        // when the terminal stops selecting text (see the `mouse` help
        // topic) — and a setting you only learn about from a config file
        // you did not know existed is not discoverable.
        id: "ui.mouse",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // Solo TUI, como `ui.mouse`. En el catálogo porque apagada por
        // defecto nadie la encontraría, y quien la busca es quien acaba de
        // pulsar Alt en el terminal y no ha pasado nada.
        id: "ui.alt-menu",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // La barra de menú fijada. Está en el catálogo por lo mismo que
        // `ui.mouse`: es la clave que alguien va a buscar en cuanto quiera
        // recuperar esa fila, y un ajuste del que solo te enteras leyendo un
        // fichero de config que no sabías que existía no es descubrible.
        id: "ui.menu-bar",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // La barra de paneles (#324), y aquí el argumento es el de la propia
        // feature: existe porque un panel que no se ve no lo encuentra nadie.
        // Dejar su interruptor solo en un fichero de config sería cometer el
        // mismo error una capa más arriba.
        id: "ui.panel-bar",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // La fila `..`. Mismo criterio que `ui.mouse` y `ui.menu-bar`: no
        // tiene comando ni tecla, así que el fichero era el ÚNICO sitio desde
        // el que se podía apagar o encender.
        id: "ui.parent-entry",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // El default de arranque de los ocultos. `pane.toggle-hidden` alterna
        // la SESIÓN y no persiste nada, así que sin esta fila el valor con el
        // que norte abre solo se podía cambiar escribiendo el fichero.
        id: "ui.show-hidden",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // El editor de `pane.edit`. Va aquí y no solo en el fichero por lo
        // mismo que el resto: es lo primero que alguien quiere cambiar, y
        // hasta ahora se elegía por variable de entorno, que es el sitio donde
        // menos se busca la configuración de un programa.
        id: "ui.editor",
        kind: SettingKind::Args,
        applies_live: true,
    },
    SettingDef {
        // Y si ese editor abre ventana propia. Sin esta fila, poner un editor
        // gráfico deja la terminal en blanco y no hay nada en pantalla que
        // explique por qué.
        id: "ui.editor-detached",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        // El comparador de `pane.compare-files` (#312), por lo mismo que el
        // editor: sin fila, el que compara dos ficheros solo se elige
        // escribiendo el fichero de configuración.
        id: "ui.diff",
        kind: SettingKind::Args,
        applies_live: true,
    },
    SettingDef {
        // Y si ese comparador abre ventana propia (Meld, Kompare).
        id: "ui.diff-detached",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        id: "ui.confirm-quit",
        kind: SettingKind::Enum(&["auto", "always", "never"]),
        applies_live: true,
    },
    // ─── El cromo (spec 2026-09-10): cada uno existe porque un lector lo
    //     echa de menos en la primera hora, y un interruptor que solo vive en
    //     el fichero es un interruptor que no encuentra nadie.
    SettingDef {
        id: "ui.key-bar",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        id: "ui.panel-bar-style",
        kind: SettingKind::Enum(&["names", "letters"]),
        applies_live: true,
    },
    SettingDef {
        // Arriba en el terminal y a la izquierda en la ventana (`auto`), o
        // la misma en los dos (spec 2026-09-21).
        id: "ui.panel-bar-position",
        kind: SettingKind::Enum(&["auto", "top", "left"]),
        applies_live: true,
    },
    SettingDef {
        // La barra de título de la ventana (ADR 0136). De ARRANQUE: la
        // decoración se quita al crear la ventana.
        id: "ui.titlebar",
        kind: SettingKind::Enum(&["native", "custom"]),
        applies_live: false,
    },
    SettingDef {
        // La mitad derecha de la barra de estado (ADR 0132): ids separados
        // por espacios, en el orden en que se pintan.
        id: "ui.status-items",
        kind: SettingKind::Args,
        applies_live: true,
    },
    SettingDef {
        id: "ui.pane-footer",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        id: "ui.row-stripes",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    SettingDef {
        id: "ui.date-format",
        kind: SettingKind::Enum(&["smart", "relative", "iso"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.notice-seconds",
        kind: SettingKind::Int { min: 0, max: 600 },
        applies_live: true,
    },
    SettingDef {
        id: "ui.history-size",
        kind: SettingKind::Int { min: 5, max: 64 },
        applies_live: true,
    },
    // Spec 2026-09-15, fase 2: la pantalla de arranque, el panel de procesos
    // que se abre solo y la `/` de las carpetas.
    SettingDef {
        id: "ui.splash",
        kind: SettingKind::Enum(&["brief", "off", "home"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.processes-panel",
        kind: SettingKind::Enum(&["auto", "manual"]),
        applies_live: true,
    },
    // Fase 5, tarea 2: cómo el visor de la TUI pinta una imagen. Clave de
    // TERMINAL — la ventana pinta imágenes por su propia webview y no la lee.
    SettingDef {
        id: "ui.images",
        kind: SettingKind::Enum(&["auto", "kitty", "blocks", "off"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.dir-indicator",
        kind: SettingKind::Enum(&["auto", "slash", "none"]),
        applies_live: true,
    },
    SettingDef {
        id: "ui.dialog-buttons",
        kind: SettingKind::Bool,
        applies_live: true,
    },
    // ─── El tema por esquema del escritorio (spec 2026-09-11, V6): solo la
    //     ventana lo lee, pero el fichero es uno y la pantalla de ajustes
    //     es la misma en los dos frontends.
    SettingDef {
        id: "ui.theme-light",
        kind: SettingKind::Text,
        applies_live: true,
    },
    SettingDef {
        id: "ui.theme-dark",
        kind: SettingKind::Text,
        applies_live: true,
    },
    SettingDef {
        id: "keymap.preset",
        kind: SettingKind::PresetName,
        applies_live: true,
    },
];

/// The curated GENERAL settings list (v1). Plugin entries are NOT here —
/// they are built separately at open time from each approved plugin's
/// manifest (S3/S4).
#[must_use]
pub fn catalog() -> &'static [SettingDef] {
    CATALOG
}

/// The Fluent id for a setting's display NAME: `setting-<id-dashed>-name`,
/// where `id-dashed` replaces `.` with `-` (`ui.confirm-quit` →
/// `setting-ui-confirm-quit-name`). Both `id` and the resulting key are
/// `norte`-authored constants (never user data) — no sanitization needed.
#[must_use]
pub fn fluent_name_id(id: &str) -> String {
    format!("setting-{}-name", id.replace('.', "-"))
}

/// The Fluent id for a setting's DESCRIPTION: `setting-<id-dashed>-desc` —
/// see [`fluent_name_id`] for the dashing rule.
#[must_use]
pub fn fluent_desc_id(id: &str) -> String {
    format!("setting-{}-desc", id.replace('.', "-"))
}

/// Maps a curated id (`ui.confirm-quit`) to the `norte.toml` WIRE location it
/// persists to: `(section, key)`. `section` is the part of `id` before the
/// first `.` (an id always has one — pinned by the coverage test below, over
/// every entry in [`catalog`]); `key` swaps every `-` for `_` (ids are dashed
/// for the Fluent derivation above, but `norte.toml` keys are `snake_case` —
/// see `norte_config::CommonConfig`'s fields, e.g. `confirm_quit`). Shared
/// by the TUI overlay (S3) and the GUI view (S4): both write through
/// `norte_config::persist_set(dir, section, key, value)`, and must derive the
/// exact same wire location from the same id.
///
/// # Panics
/// Never for an id from [`catalog`] (pinned below); a hand-rolled id without
/// a `.` would panic — a bug in the caller, not reachable through this crate.
#[must_use]
pub fn wire_key(id: &str) -> (&str, String) {
    let (section, key) = id
        .split_once('.')
        .expect("un id de catalog() siempre tiene sección.clave");
    (section, key.replace('-', "_"))
}

/// The current value of `def` read from `cfg`, as DISPLAY text (S3/S4 render
/// it directly; editing widgets parse it back per `def.kind`). An absent
/// config value renders the same string the frontend would actually use —
/// `"default"`/`"auto"` rather than empty, so the settings UI never shows a
/// blank row for something that resolves to a real behavior.
///
/// # Panics
/// Never — every arm is total; an id in [`catalog`] with no matching arm
/// here is a logic bug the catalog-coverage test below would catch (every
/// def must resolve without panicking).
#[must_use]
pub fn current_value(def: &SettingDef, cfg: &FrontendConfig) -> String {
    match def.id {
        "ui.theme" => cfg
            .common
            .ui_theme
            .clone()
            .unwrap_or_else(|| "default".to_owned()),
        "ui.lang" => cfg
            .common
            .ui_lang
            .clone()
            .unwrap_or_else(|| "auto".to_owned()),
        "ui.font" => cfg.common.ui_font.clone().unwrap_or_default(),
        "ui.mono-font" => cfg.common.ui_mono_font.clone().unwrap_or_default(),
        "ui.font-size" => cfg
            .common
            .ui_font_size
            .map(|f| f.to_string())
            .unwrap_or_default(),
        "ui.quick-search" => match cfg.common.quick_search {
            norte_config::QuickSearch::Filter => "filter",
            norte_config::QuickSearch::Jump => "jump",
        }
        .to_owned(),
        "ui.reduce-motion" => cfg.common.ui_reduce_motion.unwrap_or(false).to_string(),
        // Absent = captured: the row shows `true`, which is what the TUI
        // actually does, rather than an empty cell for a real behavior.
        "ui.mouse" => cfg.common.ui_mouse.unwrap_or(true).to_string(),
        "ui.alt-menu" => cfg.common.ui_alt_menu.unwrap_or(false).to_string(),
        // Ausente = FIJADA, igual que `ui.mouse`: la fila enseña lo que el
        // frontend hace de verdad. Faltaba, y la consecuencia no era cosmética
        // — con la celda vacía, alternar leía «no es true» y escribía `true`
        // siempre, así que la barra no se podía apagar desde aquí.
        "ui.menu-bar" => cfg.common.ui_menu_bar.unwrap_or(true).to_string(),
        "ui.panel-bar" => cfg.common.ui_panel_bar.unwrap_or(true).to_string(),
        "ui.parent-entry" => cfg.common.ui_parent_entry.unwrap_or(true).to_string(),
        "ui.show-hidden" => cfg.common.ui_show_hidden.unwrap_or(false).to_string(),
        "ui.editor" => cfg.common.ui_editor.clone().unwrap_or_default().join(" "),
        "ui.editor-detached" => cfg.common.ui_editor_detached.unwrap_or(false).to_string(),
        // Ausente = `diff -u`, y la fila lo enseña: es lo que norte hace de
        // verdad, no una celda vacía sobre un comportamiento que existe.
        "ui.diff" => cfg
            .common
            .ui_diff
            .clone()
            .unwrap_or_else(|| vec!["diff".to_owned(), "-u".to_owned(), "%F".to_owned()])
            .join(" "),
        "ui.diff-detached" => cfg.common.ui_diff_detached.unwrap_or(false).to_string(),
        "ui.confirm-quit" => cfg.common.ui_confirm_quit.as_str().to_owned(),
        // Ausente = lo que el frontend hace de verdad, como `ui.menu-bar`.
        "ui.key-bar" => cfg.common.ui_chrome.key_bar().to_string(),
        "ui.panel-bar-style" => cfg.common.ui_chrome.panel_bar_style().as_str().to_owned(),
        "ui.panel-bar-position" => cfg
            .common
            .ui_chrome
            .panel_bar_position()
            .as_str()
            .to_owned(),
        "ui.titlebar" => cfg.common.ui_chrome.titlebar().as_str().to_owned(),
        "ui.status-items" => cfg.common.ui_chrome.status_items().to_ids().join(" "),
        "ui.pane-footer" => cfg.common.ui_chrome.pane_footer().to_string(),
        "ui.row-stripes" => cfg.common.ui_chrome.row_stripes().to_string(),
        "ui.date-format" => cfg.common.ui_chrome.date_format().as_str().to_owned(),
        "ui.notice-seconds" => cfg.common.ui_chrome.notice_seconds().to_string(),
        "ui.history-size" => cfg.common.ui_chrome.history_size().to_string(),
        "ui.splash" => cfg.common.ui_chrome.splash().as_str().to_owned(),
        "ui.processes-panel" => cfg.common.ui_chrome.processes_panel().as_str().to_owned(),
        "ui.images" => cfg.common.ui_chrome.images().as_str().to_owned(),
        "ui.dir-indicator" => cfg.common.ui_chrome.dir_indicator().as_str().to_owned(),
        "ui.dialog-buttons" => cfg.common.ui_chrome.dialog_buttons().to_string(),
        // Vacío = sin variante: la ventana pinta `theme` en los dos esquemas.
        "ui.theme-light" => cfg.common.ui_theme_light.clone().unwrap_or_default(),
        "ui.theme-dark" => cfg.common.ui_theme_dark.clone().unwrap_or_default(),
        "keymap.preset" => cfg.common.preset.clone(),
        // Unreachable for anything in `CATALOG` (pinned by the coverage
        // test below); an id typo'd into `current_value` but not `CATALOG`
        // — or vice versa — would only show up as a fallback, never panic.
        _ => String::new(),
    }
}

/// One approved+enabled plugin's `[config]` SUMMARY (G3c): built by the
/// caller from `plugins_list` + one `plugin.get_config` call per plugin
/// (async — [`build_rows`] stays pure/sync, the caller fetches these
/// FIRST). Drives one [`Row`] per plugin in the Plugins section; drilling
/// into it (caller-side: `Backend::plugin_get_config` again, then a
/// [`crate::plugin_config::PluginConfigState`]) is how the actual keys get
/// edited — this summary only carries enough to LIST the plugin.
#[derive(Debug, Clone)]
pub struct PluginConfigSummary {
    /// Stable plugin id (`org.norte.demo`) — safe to display as-is
    /// (reverse-DNS charset, core-validated) and to pass back to
    /// `Backend::plugin_get_config`/`plugin_set_config`.
    pub plugin_id: String,
    /// Plugin name, ALREADY masked ([`crate::display_name`] — plugin text,
    /// untrusted).
    pub name: String,
    /// How many `[config.<key>]` entries this plugin declares. A plugin
    /// with `0` is NOT expected here — the caller should already have
    /// filtered it out (nothing to show, nothing to drill into).
    pub key_count: usize,
}

/// One row of a settings view (TUI overlay, S3; GUI full-view swap, S4):
/// built, never computed by the editor ([`SettingsState`] only consumes it).
#[derive(Debug, Clone)]
pub struct Row {
    /// Index into [`catalog`]; `None` for a Plugins-section row (a
    /// per-plugin summary, or the informational "nothing configurable"
    /// fallback — see [`build_rows`]) — never editable through THIS state
    /// machine, [`SettingsState::activate`] recognizes it by this, not by
    /// text.
    def_index: Option<usize>,
    /// The plugin id this row summarizes (G3c), or `None` for a General
    /// row or the informational "nothing configurable" fallback. The
    /// caller checks this BEFORE calling [`SettingsState::activate`] — a
    /// `Some` here means Enter should drill into that plugin's own
    /// [`crate::plugin_config::PluginConfigState`], not call `activate`
    /// (which is a no-op for any row with `def_index: None`, plugin
    /// summary included).
    plugin_id: Option<String>,
    /// Localized (Fluent) name to paint.
    pub name: String,
    /// Localized description — footer/detail line of the selected row.
    pub desc: String,
    /// Current value as display text; empty for a row with nothing single
    /// to show (the informational fallback).
    pub value: String,
    /// La sección bajo la que se pinta: la del catálogo para una entrada
    /// curada, [`Section::Plugins`] para un resumen de plugin.
    ///
    /// Va en la fila y no se re-deriva del id en cada frontend: dos cuentas
    /// de «dónde va esto» son dos pantallas que se desordenan por separado.
    pub section: Section,
    /// El valor efectivo NO es el de fábrica.
    ///
    /// «No es el de fábrica», y no «lo has tocado tú»: una clave que solo
    /// fija la capa del sistema enciende el punto sin que el lector haya
    /// hecho nada, y una que escribió a mano con el valor que ya traía no lo
    /// enciende. La etiqueta de la pantalla dice lo primero, que es lo que
    /// esto mide.
    ///
    /// Falso siempre para una fila que no sale del catálogo: no hay valor
    /// de fábrica con el que compararla.
    pub modified: bool,
}

impl Row {
    /// `true` for ANY row in the Plugins section (summary or the
    /// informational fallback): never editable via [`SettingsState::activate`].
    #[must_use]
    pub fn is_plugins_note(&self) -> bool {
        self.def_index.is_none()
    }

    /// The catalog id this row renders (`ui.confirm-quit`, …), or `None` for
    /// a Plugins-section row. A frontend that needs to derive section/key
    /// ([`wire_key`]) or per-entry behavior from a rendered row (e.g. the
    /// GUI's S4 live-vs-restart-required split, since `Row` keeps
    /// `def_index` private) uses this instead of re-deriving the catalog
    /// index itself.
    #[must_use]
    pub fn id(&self) -> Option<&'static str> {
        self.def_index.map(|i| catalog()[i].id)
    }

    /// The plugin id this row summarizes (G3c), or `None` for a General row
    /// or the informational "nothing configurable" fallback. `Some` is the
    /// caller's signal to drill in on Enter (see the field's own doc).
    #[must_use]
    pub fn plugin_id(&self) -> Option<&str> {
        self.plugin_id.as_deref()
    }
}

/// The Plugins section's rows (G3c — replaces the old P2-era informational
/// note now that `plugin.get_config`/`plugin.set_config` put settings on
/// the wire): one row PER `summaries` entry (`name` = the plugin's masked
/// name, `desc` a localized "press Enter" hint, `value` a localized
/// `"N settings"` count) — never directly editable through THIS state
/// machine (`plugin_id().is_some()` is the caller's cue to drill into a
/// [`crate::plugin_config::PluginConfigState`] instead of calling
/// [`SettingsState::activate`]). An EMPTY `summaries` (no approved+enabled
/// plugin declares any `[config]` key) falls back to a single
/// informational row, same shape as before G3c.
fn plugin_summary_rows(summaries: &[PluginConfigSummary], lang: norte_i18n::Lang) -> Vec<Row> {
    if summaries.is_empty() {
        return vec![Row {
            def_index: None,
            plugin_id: None,
            name: norte_i18n::t_in(lang, "settings-plugins-name"),
            desc: norte_i18n::t_in(lang, "settings-plugins-note"),
            value: String::new(),
            section: Section::Plugins,
            modified: false,
        }];
    }
    summaries
        .iter()
        .map(|s| Row {
            def_index: None,
            plugin_id: Some(s.plugin_id.clone()),
            name: s.name.clone(),
            desc: norte_i18n::t_in(lang, "settings-plugins-open-hint"),
            value: norte_i18n::ta_in(
                lang,
                "settings-plugins-key-count",
                &[("count", &s.key_count.to_string())],
            ),
            section: Section::Plugins,
            modified: false,
        })
        .collect()
}

/// Builds the rows for a settings view: the GENERAL catalog (S2) × the
/// CURRENT value of `cfg` × localized name/description, plus the Plugins
/// section (G3c) built from `plugin_summaries` — the caller fetches those
/// via `plugins_list` + `plugin.get_config` BEFORE
/// calling this (this function stays pure/sync). Called on OPEN
/// (`app.settings`) and on every successful hot-reload with the current
/// `cfg` (TUI) — same criterion as `help_lines`/`palette_rows`: rebuilt
/// wholesale, never mutated row by row.
#[must_use]
pub fn build_rows(cfg: &FrontendConfig, plugin_summaries: &[PluginConfigSummary]) -> Vec<Row> {
    build_rows_in(cfg, plugin_summaries, norte_i18n::active())
}

/// [`build_rows`] en un idioma DADO.
///
/// La pantalla de ajustes traducía los títulos de sección con el idioma del
/// HOST y el nombre y la descripción de cada opción con el del PROCESO, así
/// que salía a medias en dos idiomas.
#[must_use]
pub fn build_rows_in(
    cfg: &FrontendConfig,
    plugin_summaries: &[PluginConfigSummary],
    lang: norte_i18n::Lang,
) -> Vec<Row> {
    let fabrica = factory_config();
    let mut rows: Vec<Row> = catalog()
        .iter()
        .enumerate()
        .map(|(i, def)| {
            let value = current_value(def, cfg);
            Row {
                def_index: Some(i),
                plugin_id: None,
                name: norte_i18n::t_in(lang, &fluent_name_id(def.id)),
                desc: norte_i18n::t_in(lang, &fluent_desc_id(def.id)),
                modified: fabrica.is_some_and(|f| value != current_value(def, f)),
                value,
                section: def.section(),
            }
        })
        .collect();
    // En orden de PANTALLA: por sección primero, y dentro de cada una el
    // orden del catálogo. El catálogo va agrupado por sección de
    // `norte.toml` (`[ui]` y luego `[keymap]`), que no es el mismo reparto:
    // `ui.lang` es Comportamiento y `ui.quick-search` es Teclado, y están a
    // dos filas la una de la otra. Sin ordenar aquí, una lista con cabeceras
    // pintaría la misma sección siete veces.
    //
    // `sort_by_key` es ESTABLE, que es lo que conserva el orden del catálogo
    // dentro de cada sección sin escribir un segundo criterio.
    rows.sort_by_key(|r| {
        Section::ORDER
            .iter()
            .position(|s| *s == r.section)
            .unwrap_or(usize::MAX)
    });
    rows.extend(plugin_summary_rows(plugin_summaries, lang));
    rows
}

/// A value pending persistence to `norte.toml`, PRODUCED by
/// [`SettingsState::activate`]/[`SettingsState::edit_commit`] — editing is
/// PURE (no [`SettingsState`] method does I/O); the caller (TUI
/// `main::on_settings_key`, GUI `settings_view`) calls
/// `norte_config::persist_set` off the UI thread (rule 2) and announces the
/// result. `section`/`key` already come in WIRE form ([`wire_key`]).
#[derive(Debug, Clone)]
pub struct PendingWrite {
    /// `[section]` of `norte.toml`.
    pub section: &'static str,
    /// Key within that section (`snake_case`, already converted).
    pub key: String,
    /// The TYPED value to write (native bool/int/string — `persist_set`
    /// serializes each in its own TOML shape, never everything as a string).
    pub value: toml_edit::Value,
    /// Localized name of the setting (for the confirmation message).
    pub name: String,
    /// New value as DISPLAY text (for the message + the optimistic row
    /// update [`SettingsState`] performs when it builds this value).
    pub display: String,
}

impl PendingWrite {
    /// A write of a STRING value (spec 2026-09-10, the first-run wizard):
    /// the typed `toml_edit::Value` is built here so a frontend that never
    /// depends on `toml_edit` can still hand a write to its settings path.
    ///
    /// ```
    /// use norte_frontend::settings::PendingWrite;
    /// let w = PendingWrite::text("ui", "theme", "nord", "Theme".to_owned());
    /// assert_eq!((w.section, w.key.as_str(), w.display.as_str()), ("ui", "theme", "nord"));
    /// assert_eq!(w.value.as_str(), Some("nord"));
    /// ```
    #[must_use]
    pub fn text(section: &'static str, key: &str, value: &str, name: String) -> Self {
        Self {
            section,
            key: key.to_owned(),
            value: toml_edit::Value::from(value),
            name,
            display: value.to_owned(),
        }
    }
}

/// Una clave que hay que QUITAR de la capa de escritura, producida por
/// [`SettingsState::reset`].
///
/// Como [`PendingWrite`], es pura: quien la recibe llama a
/// `norte_config::persist_unset` fuera del hilo de pintado (regla 2) y
/// anuncia el resultado. No lleva valor porque no hay ninguno que escribir
/// — restablecer es dejar de decir nada, no decir el defecto: escribir el
/// valor de fábrica en el fichero lo congelaría contra un cambio futuro del
/// defecto, que es justo lo contrario de lo que el lector pidió.
#[derive(Debug, Clone)]
pub struct PendingReset {
    /// `[section]` de `norte.toml`.
    pub section: &'static str,
    /// La clave dentro de esa sección (`snake_case`, ya convertida).
    pub key: String,
    /// El id del catálogo (`ui.theme`), para volver a encontrar la fila
    /// después de releer.
    ///
    /// Viaja porque la vuelta —de `(section, key)` al id— NO es una
    /// biyección: un id futuro con `_` volvería con `-` y no casaría con
    /// ninguna fila, y quien busca contestaría «vuelve al valor de fábrica»
    /// para todo, que es la respuesta equivocada y muda.
    pub id: &'static str,
    /// El nombre traducido del ajuste, para el aviso.
    pub name: String,
}

/// Why [`SettingsState::edit_commit`] rejected the buffer — WITHOUT
/// persisting (S3/S4: "invalid = status-bar/inline error, value untouched").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsEditError {
    /// The buffer does not parse as an integer ([`SettingKind::Int`]).
    NotAnInt,
    /// Parses, but falls outside `[min, max]`.
    OutOfRange {
        /// Inclusive lower bound.
        min: i64,
        /// Inclusive upper bound.
        max: i64,
    },
    /// The value does not fit what the setting admits; `key` is the Fluent
    /// id of the message that says what does (ADR 0132: an unknown or
    /// repeated `ui.status-items` id is refused HERE, before it reaches
    /// `norte.toml` and breaks the next load).
    Invalid {
        /// The Fluent id of the message.
        key: &'static str,
    },
}

/// Settings editor (`app.settings`, S3 TUI overlay / S4 GUI full-view swap):
/// free search ALWAYS active, cursor over the VISIBLE rows (same pattern as
/// the command palette) plus an inline EDIT mode for `Text`/`Int` rows (raw
/// buffer, Enter confirms, Esc cancels). Editing methods are PURE — they
/// return a [`PendingWrite`] or a [`SettingsEditError`], never do I/O — the
/// caller persists and announces. The Plugins section (a single
/// informational row, [`build_rows`]) is never editable: `def_index == None`
/// makes [`Self::activate`] a no-op over it.
#[derive(Debug, Clone)]
pub struct SettingsState {
    /// Rows ([`Row`]) — snapshot frozen on open, replaced WHOLESALE by
    /// [`Self::refresh`] on every successful hot-reload.
    rows: Vec<Row>,
    /// Folded haystack per row (id + name + description, [`crate::nav::fold`]).
    folds: Vec<String>,
    /// Bytes typed as-is into the filter (unsanitized; sanitizing happens
    /// only when painting, [`Self::query_display`]).
    query: Vec<u8>,
    /// REAL indices into `rows` that match (empty query = all).
    visible: Vec<usize>,
    /// Selection position WITHIN `visible`.
    cursor: usize,
    /// Inline edit buffer (`Text`/`Int`): `Some` = editing the row under the
    /// cursor; `None` = normal browsing/filtering. Raw, like a name-input
    /// popup — sanitizing happens on paint.
    edit: Option<String>,
    /// La primera LÍNEA visible de una lista que no cabe, en la unidad de
    /// quien pinta ([`Self::reconcile_viewport`]).
    ///
    /// No existía porque los ajustes cabían en una pantalla — lo decía el
    /// editor de atajos de la terminal, y era verdad cuando se escribió. Con
    /// ~30 ajustes dejó de serlo: bajar con el cursor pasado el borde lo
    /// dejaba fuera de la caja y la lista no se movía.
    viewport_offset: usize,
    /// Qué mitad tiene el teclado.
    ///
    /// No hay un cursor del índice aparte: con el foco en él, moverse CAMBIA
    /// de sección y la lista sigue, igual que la barra lateral de la ayuda
    /// abre el tema al recorrerla. Un segundo cursor que hubiera que
    /// sincronizar con el primero es la clase de estado que se desincroniza.
    focus: Focus,
}

impl SettingsState {
    /// Deja la ventana lista para pintar `rows` líneas con el cursor a la
    /// vista: la arrastra SÓLO si el cursor se salió, por la regla compartida
    /// de [`crate::viewport::sticky_offset`]. Se llama una vez por frame,
    /// antes de pintar.
    ///
    /// Recibe la línea del cursor y el total YA en líneas de pantalla, y no
    /// en filas, porque quien pinta intercala cabeceras de sección entre las
    /// filas: esa cuenta es suya, y hacerla aquí sería una segunda copia de
    /// cómo se pinta. La ventana, que es web, ni lo llama — el navegador ya
    /// desplaza la fila elegida hasta que se ve.
    ///
    /// `anchor_line` es la primera línea que tiene que verse CON el cursor:
    /// la cabecera de su sección cuando el cursor está en la primera fila de
    /// ella, y `cursor_line` en cualquier otro caso. Existe porque anclar
    /// solo al cursor esconde la cabecera para siempre: la primera fila vive
    /// en la línea 1 —la 0 es «General»—, así que al subir del todo el
    /// desplazamiento se quedaba en 1 y la cabecera no volvía nunca. El
    /// cursor manda en el borde de ABAJO (una cabecera no puede empujarlo
    /// fuera de la caja) y el ancla solo tira hacia ARRIBA.
    pub fn reconcile_viewport(
        &mut self,
        cursor_line: usize,
        anchor_line: usize,
        total_lines: usize,
        rows: usize,
    ) {
        let off =
            crate::viewport::sticky_offset(self.viewport_offset, cursor_line, total_lines, rows);
        self.viewport_offset = off.min(anchor_line.min(cursor_line));
    }

    /// La primera línea visible — ver [`Self::reconcile_viewport`].
    #[must_use]
    pub fn viewport_offset(&self) -> usize {
        self.viewport_offset
    }

    /// Opens the editor over `rows` (a [`build_rows`] snapshot): folds each
    /// row's haystack and starts with an empty query (everything visible),
    /// not editing.
    #[must_use]
    pub fn new(rows: Vec<Row>) -> Self {
        let folds = Self::fold_rows(&rows);
        let mut s = Self {
            rows,
            folds,
            query: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
            edit: None,
            viewport_offset: 0,
            focus: Focus::List,
        };
        s.recompute();
        s
    }

    fn fold_rows(rows: &[Row]) -> Vec<String> {
        let catalog = catalog();
        rows.iter()
            .map(|r| {
                let id: &str = match (r.def_index, r.plugin_id.as_deref()) {
                    (Some(i), _) => catalog[i].id,
                    (None, Some(pid)) => pid,
                    (None, None) => "plugins",
                };
                crate::nav::fold(format!("{id} {} {}", r.name, r.desc).as_bytes())
            })
            .collect()
    }

    /// Replaces the rows with a FRESH snapshot (hot-reload): recomputes the
    /// fold and re-filters with the CURRENT query (kept, unlike
    /// help/palette overlays, which CLOSE — a settings row is just
    /// `(name, description, value)` read from `cfg`, safe to recompute
    /// without invalidating what the user is doing). The edit buffer, if
    /// any, is ALSO kept raw — a reload must not throw away what the user
    /// already typed.
    pub fn refresh(&mut self, rows: Vec<Row>) {
        self.folds = Self::fold_rows(&rows);
        self.rows = rows;
        self.recompute();
    }

    fn recompute(&mut self) {
        let filtro = Query::parse(&self.query);
        self.visible = self
            .folds
            .iter()
            .enumerate()
            .filter(|(i, f)| filtro.matches(&self.rows[*i], f))
            .map(|(i, _)| i)
            .collect();
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len() - 1;
        }
    }

    /// Appends a character to the filter query and recomputes. No-op while
    /// editing ([`Self::is_editing`]) — the caller already branches on that,
    /// but the guard here makes it an invariant OF THE TYPE, not just of the
    /// call site.
    pub fn push_char(&mut self, c: char) {
        if self.edit.is_some() {
            return;
        }
        let mut buf = [0u8; 4];
        self.query
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.recompute();
    }

    /// Pone la consulta ENTERA de golpe y recomputa.
    ///
    /// La ventana la necesita: su buscador es un `<input>` del navegador y
    /// lo que cruza el puente es el texto completo, no la tecla. El terminal
    /// sigue con [`Self::push_char`] porque su overlay sí recibe teclas.
    ///
    /// El texto llega como bytes de una caja de texto: no se valida ni se
    /// recorta aquí — plegar y filtrar es todo lo que se hace con él, y
    /// pintarlo es de quien pinta ([`Self::query_display`] lo enmascara).
    pub fn set_query(&mut self, text: &str) {
        if self.edit.is_some() {
            return;
        }
        self.query = text.as_bytes().to_vec();
        self.recompute();
    }

    /// Removes the last complete UTF-8 char from the query. No-op editing.
    pub fn backspace(&mut self) {
        if self.edit.is_some() || self.query.is_empty() {
            return;
        }
        let mut cut = self.query.len() - 1;
        while cut > 0 && (self.query[cut] & 0b1100_0000) == 0b1000_0000 {
            cut -= 1;
        }
        self.query.truncate(cut);
        self.recompute();
    }

    /// Moves the selection up (clamped at the top). No-op editing.
    pub fn up(&mut self) {
        if self.edit.is_some() {
            return;
        }
        // Con el foco en el índice se recorren SECCIONES, no filas, y la
        // lista sigue: es lo que hace la barra lateral de la ayuda, que abre
        // el tema al pasar por él.
        if self.focus == Focus::Index {
            self.step_section(-1);
            return;
        }
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the selection down (clamped at the end). No-op editing.
    pub fn down(&mut self) {
        if self.edit.is_some() {
            return;
        }
        if self.focus == Focus::Index {
            self.step_section(1);
            return;
        }
        if self.cursor + 1 < self.visible.len() {
            self.cursor += 1;
        }
    }

    /// Sets the selection to `idx` WITHIN [`Self::visible`], clamped to the
    /// last visible row (or 0 with nothing visible) — the mouse hover/click
    /// selection primitive (GUI, S4; a future TUI mouse mode could reuse it
    /// too). No-op while editing, same guard as [`Self::up`]/[`Self::down`].
    pub fn set_cursor(&mut self, idx: usize) {
        if self.edit.is_none() {
            self.cursor = idx.min(self.visible.len().saturating_sub(1));
        }
    }

    /// Moves the selection up `n` positions (page-up). No-op editing.
    pub fn page_up(&mut self, n: usize) {
        if self.edit.is_none() {
            self.cursor = self.cursor.saturating_sub(n);
        }
    }

    /// Moves the selection down `n` positions, clamped at the end
    /// (page-down). No-op editing.
    pub fn page_down(&mut self, n: usize) {
        if self.edit.is_none() {
            self.cursor = (self.cursor + n).min(self.visible.len().saturating_sub(1));
        }
    }

    /// REAL indices into [`Self::rows`] visible under the current query.
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// All rows — `rows()[visible()[i]]` paints the `i`-th filtered row.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// El índice de secciones que pinta la pantalla: TODAS, en el orden de
    /// [`Section::ORDER`], con cuántas filas visibles tiene cada una y en
    /// cuál empieza.
    ///
    /// Una sección que el filtro deja a cero **sigue en la lista**, apagada:
    /// un índice que cambia de largo mientras escribes es un índice que no
    /// se puede usar como mapa.
    ///
    /// `first_row` es una posición dentro de [`Self::visible`] —la misma
    /// unidad que [`Self::cursor`]— y NO un índice dentro de [`Self::rows`].
    /// Mezclar las dos unidades es un cursor que apunta a otra fila.
    #[must_use]
    pub fn sections(&self) -> Vec<SectionView> {
        Section::ORDER
            .iter()
            .map(|s| {
                let mut visible = 0;
                let mut first_row = None;
                for (pos, &real) in self.visible.iter().enumerate() {
                    if self.rows[real].section == *s {
                        visible += 1;
                        if first_row.is_none() {
                            first_row = Some(pos);
                        }
                    }
                }
                SectionView {
                    section: *s,
                    title: t(s.label_key()),
                    visible,
                    total: self.rows.iter().filter(|r| r.section == *s).count(),
                    first_row,
                }
            })
            .collect()
    }

    /// Qué mitad tiene el teclado.
    #[must_use]
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// Cambia de lado. No-op mientras se edita: una edición abierta congela
    /// todo lo demás, como el resto de esta máquina.
    ///
    /// Del índice no se puede salir a un sitio que no existe, así que pasar
    /// a él con la lista vacía tampoco tiene sentido: sin filas visibles no
    /// hay sección a la que ir, y el foco se queda donde está.
    pub fn toggle_focus(&mut self) {
        if self.edit.is_some() {
            return;
        }
        self.focus = match self.focus {
            Focus::Index => Focus::List,
            Focus::List if self.visible.is_empty() => Focus::List,
            Focus::List => Focus::Index,
        };
    }

    /// Lleva el cursor a la sección anterior (`delta` negativo) o siguiente,
    /// SALTÁNDOSE las que el filtro dejó vacías. Devuelve a cuál fue, o
    /// `None` si no había ninguna con filas hacia ese lado.
    ///
    /// Vive aquí y no en cada frontend porque las dos pantallas tienen que
    /// moverse igual: con el recorrido escrito dos veces, la octava sección
    /// —o un cambio de orden— las separa en silencio y solo una tiene test.
    /// Saltarse las vacías es lo que hace que la tecla sirva con un filtro
    /// puesto: parar en una obligaría a pulsar dos veces sin que nada pase.
    pub fn step_section(&mut self, delta: i32) -> Option<Section> {
        let &real = self.visible.get(self.cursor)?;
        let mut actual = self.rows[real].section;
        let index = self.sections();
        while let Some(siguiente) = actual.step(delta) {
            if index
                .iter()
                .any(|v| v.section == siguiente && v.visible > 0)
            {
                self.jump_to(siguiente);
                return Some(siguiente);
            }
            actual = siguiente;
        }
        None
    }

    /// Lleva el cursor a la primera fila visible de `section`.
    ///
    /// Una sección sin filas visibles no mueve nada: un salto que aterriza
    /// en la fila de otra sección es peor que un salto que no ocurre.
    pub fn jump_to(&mut self, section: Section) {
        let destino = self
            .visible
            .iter()
            .position(|&real| self.rows[real].section == section);
        if let Some(pos) = destino {
            self.set_cursor(pos);
        }
    }

    /// Selection position WITHIN [`Self::visible`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Cuántas filas hay en total, filtre lo que filtre.
    #[must_use]
    pub fn total(&self) -> usize {
        self.rows.len()
    }

    /// Cuántas se ven con el filtro puesto.
    ///
    /// Va con [`Self::total`] a la pantalla («7 de 33») porque sin la
    /// segunda cifra «no hay nada» y «lo tapé con una letra» se leen igual.
    #[must_use]
    pub fn shown(&self) -> usize {
        self.visible.len()
    }

    /// The localized description of the row under the cursor, if any is
    /// visible — the detail/footer line of the overlay/view.
    #[must_use]
    pub fn selected_desc(&self) -> Option<&str> {
        self.visible
            .get(self.cursor)
            .map(|&i| self.rows[i].desc.as_str())
    }

    /// Query text ready to paint (lossy, masked — same contract as the
    /// command palette's query display).
    #[must_use]
    pub fn query_display(&self) -> String {
        String::from_utf8_lossy(&self.query)
            .chars()
            .map(|c| {
                if norte_encoding::is_terminal_hazard(c) {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    }

    /// `true` while the inline edit buffer (`Text`/`Int`) is active.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    /// The RAW edit buffer, for painting (sanitizing happens on paint, same
    /// contract as a raw name-input buffer).
    #[must_use]
    pub fn edit_buffer(&self) -> Option<&str> {
        self.edit.as_deref()
    }

    /// Appends a char to the edit buffer. No-op if not editing.
    pub fn edit_push_char(&mut self, c: char) {
        if let Some(buf) = &mut self.edit {
            buf.push(c);
        }
    }

    /// Removes the last char from the edit buffer. No-op if not editing.
    pub fn edit_backspace(&mut self) {
        if let Some(buf) = &mut self.edit {
            buf.pop();
        }
    }

    /// Replaces the edit buffer WHOLESALE. No-op if not editing.
    ///
    /// For a frontend whose text field is native (the window's prompt): the
    /// caret is the widget's, and the host receives the full text on
    /// confirm rather than one character at a time. Same contract as
    /// [`Self::edit_push_char`] otherwise — raw, unsanitized, validated by
    /// [`Self::edit_commit`].
    pub fn edit_set(&mut self, text: &str) {
        if let Some(buf) = &mut self.edit {
            text.clone_into(buf);
        }
    }

    /// Cancels the edit WITHOUT writing — the row's value stays as it was.
    pub fn edit_cancel(&mut self) {
        self.edit = None;
    }

    /// Enter/click over the row under the cursor: `Bool`/`Enum`/`ThemeName`/
    /// `PresetName` CYCLE immediately (return the [`PendingWrite`] right
    /// away — nothing else to confirm); `Text`/`Int` OPEN the edit buffer
    /// (return `None` — [`Self::edit_commit`] produces the [`PendingWrite`]
    /// once the user confirms). The Plugins informational row and "nothing
    /// visible" also return `None`, without opening anything. `theme_names`/
    /// `preset_names` are the LIVE lists (not `&'static`, resolved at
    /// runtime) — the caller computes them.
    pub fn activate(
        &mut self,
        theme_names: &[String],
        preset_names: &[&str],
    ) -> Option<PendingWrite> {
        let &real = self.visible.get(self.cursor)?;
        let idx = self.rows[real].def_index?;
        let def = &catalog()[idx];
        let current = self.rows[real].value.clone();
        match def.kind {
            SettingKind::Bool => {
                let next = current != "true";
                Some(self.commit_row(real, def, next.to_string(), toml_edit::Value::from(next)))
            }
            SettingKind::Enum(values) => {
                let next = cycle(&current, values);
                let value = toml_edit::Value::from(next.as_str());
                Some(self.commit_row(real, def, next, value))
            }
            SettingKind::ThemeName => {
                let refs: Vec<&str> = theme_names.iter().map(String::as_str).collect();
                let next = cycle(&current, &refs);
                let value = toml_edit::Value::from(next.as_str());
                Some(self.commit_row(real, def, next, value))
            }
            SettingKind::PresetName => {
                let next = cycle(&current, preset_names);
                let value = toml_edit::Value::from(next.as_str());
                Some(self.commit_row(real, def, next, value))
            }
            SettingKind::Text | SettingKind::Int { .. } | SettingKind::Args => {
                self.edit = Some(current);
                None
            }
        }
    }

    /// Pone un valor CONCRETO en la fila `id` — lo que necesita un control
    /// de ventana (un interruptor, un desplegable, un campo numérico).
    ///
    /// Existe porque [`Self::activate`] **cicla**: con un desplegable de
    /// diez temas, elegir el séptimo serían siete viajes y seis escrituras
    /// en el `norte.toml`. Aquí el control dice a qué valor va y se escribe
    /// una vez.
    ///
    /// La validación es la MISMA que la del teclado: un entero pasa por el
    /// rango del catálogo, una línea de órdenes se trocea igual, y un valor
    /// que no está en la lista de un enum se rechaza. Nada de esto vive en
    /// el renderer — un frontend que validara por su cuenta sería una
    /// segunda regla que se separa de la primera.
    ///
    /// `theme_names`/`preset_names` llegan VIVAS, como en [`Self::activate`].
    ///
    /// # Errors
    /// [`SettingsEditError`] con el mismo criterio que [`Self::edit_commit`];
    /// una fila que no existe, que no sale del catálogo, o un valor fuera de
    /// la lista de su enum se rechazan como un entero inválido — el fallo
    /// inerte que el editor ya usa para «esto no se puede escribir».
    pub fn set_value(
        &mut self,
        id: &str,
        valor: &str,
        theme_names: &[String],
        preset_names: &[&str],
    ) -> Result<PendingWrite, SettingsEditError> {
        if self.edit.is_some() {
            return Err(SettingsEditError::NotAnInt);
        }
        let Some(pos) = self
            .visible
            .iter()
            .position(|&i| self.rows[i].id() == Some(id))
        else {
            return Err(SettingsEditError::NotAnInt);
        };
        let real = self.visible[pos];
        let Some(idx) = self.rows[real].def_index else {
            return Err(SettingsEditError::NotAnInt);
        };
        let def = &catalog()[idx];
        match def.kind {
            SettingKind::Bool => {
                let b = match valor {
                    "true" => true,
                    "false" => false,
                    _ => return Err(SettingsEditError::NotAnInt),
                };
                Ok(self.commit_row(real, def, b.to_string(), toml_edit::Value::from(b)))
            }
            SettingKind::Enum(values) => {
                if !values.contains(&valor) {
                    return Err(SettingsEditError::NotAnInt);
                }
                let v = toml_edit::Value::from(valor);
                Ok(self.commit_row(real, def, valor.to_owned(), v))
            }
            SettingKind::ThemeName => {
                if !theme_names.iter().any(|t| t == valor) {
                    return Err(SettingsEditError::NotAnInt);
                }
                let v = toml_edit::Value::from(valor);
                Ok(self.commit_row(real, def, valor.to_owned(), v))
            }
            SettingKind::PresetName => {
                if !preset_names.contains(&valor) {
                    return Err(SettingsEditError::NotAnInt);
                }
                let v = toml_edit::Value::from(valor);
                Ok(self.commit_row(real, def, valor.to_owned(), v))
            }
            // Los que ya sabe validar el editor de línea: se le pasa el
            // texto entero por su mismo camino, en vez de copiar el troceo
            // de una línea de órdenes o el rango de un entero.
            SettingKind::Int { .. } | SettingKind::Text | SettingKind::Args => {
                let antes = self.cursor;
                self.cursor = pos;
                self.edit = Some(valor.to_owned());
                let salida = self.edit_commit();
                self.edit = None;
                if salida.is_err() {
                    self.cursor = antes;
                }
                salida
            }
        }
    }

    /// Restablecer la fila del cursor: la clave que hay que QUITAR de la
    /// capa de escritura, o `None` si no hay nada que quitar.
    ///
    /// `None` cuando la fila ya está en su valor de fábrica (quitar una
    /// clave que no está es un no-op que no merece un aviso), cuando no sale
    /// del catálogo (un resumen de plugin no tiene valor de fábrica), o
    /// mientras se edita — igual que el resto de esta máquina, editar
    /// congela todo lo demás.
    ///
    /// **Quitar la clave de TU capa no siempre devuelve el valor de
    /// fábrica**: si el sistema, el perfil o el proyecto fijan la misma, el
    /// valor cambia y sigue sin ser el defecto. Esta función no lo sabe;
    /// quien la llama reconstruye las filas después —ya lo hace tras cada
    /// escritura— y mira el punto: si la fila sigue `modified`, lo dice con
    /// `settings-still-set-elsewhere`, y si no, con `settings-reset-done`.
    /// El punto encendido es verdad sin maquinaria de procedencia.
    pub fn reset(&mut self) -> Option<PendingReset> {
        if self.edit.is_some() {
            return None;
        }
        let &real = self.visible.get(self.cursor)?;
        let row = &self.rows[real];
        if !row.modified {
            return None;
        }
        let id = row.id()?;
        let (section, key) = wire_key(id);
        Some(PendingReset {
            section,
            key,
            id,
            name: row.name.clone(),
        })
    }

    /// Confirms the inline edit buffer: `Int` parses the buffer as `f64`
    /// (revisión S, M4 — see [`SettingKind::Int`]'s doc for why a "whole
    /// number" kind accepts a fractional part) and validates `[min, max]`
    /// ([`SettingsEditError`] WITHOUT persisting, buffer intact — the user
    /// corrects and retries); `Text` accepts anything. Only reachable with
    /// [`Self::is_editing`] — the caller guarantees it; without an active
    /// edit this returns `SettingsEditError::NotAnInt` as an inert fallback
    /// (unreachable in practice, defense in depth).
    ///
    /// # Errors
    /// [`SettingsEditError::NotAnInt`] if an `Int` row's buffer does not
    /// parse as a number (or, as an inert fallback, if there is no active
    /// edit); [`SettingsEditError::OutOfRange`] if it parses but falls
    /// outside `[min, max]`. Never for a `Text` row.
    pub fn edit_commit(&mut self) -> Result<PendingWrite, SettingsEditError> {
        let (Some(buf), Some(real)) = (self.edit.clone(), self.visible.get(self.cursor).copied())
        else {
            return Err(SettingsEditError::NotAnInt);
        };
        let Some(idx) = self.rows[real].def_index else {
            return Err(SettingsEditError::NotAnInt);
        };
        let def = &catalog()[idx];
        let write = if let SettingKind::Int { min, max } = def.kind {
            let n: f64 = buf
                .trim()
                .parse()
                .map_err(|_| SettingsEditError::NotAnInt)?;
            // `min`/`max` are catalog constants, always tiny (today: 8/32) —
            // the precision loss `as f64` could theoretically incur past
            // 2^53 never applies here.
            #[expect(clippy::cast_precision_loss, reason = "magnitudes lejos de 2^53")]
            let (min_f, max_f) = (min as f64, max as f64);
            if n < min_f || n > max_f {
                return Err(SettingsEditError::OutOfRange { min, max });
            }
            // Whole number → TOML Integer (keeps `norte.toml` looking the
            // same as before this fix for the common case, "14" not
            // "14.0"); fractional → TOML Float ("14.5"). `n.to_string()`
            // already renders a whole `f64` WITHOUT a trailing ".0" (Rust's
            // `Display` for floats picks the shortest round-tripping form),
            // so `display` needs no separate branch.
            #[expect(clippy::cast_possible_truncation, reason = "n ∈ [min, max], both i64")]
            let value = if n.fract() == 0.0 {
                toml_edit::Value::from(n as i64)
            } else {
                toml_edit::Value::from(n)
            };
            self.commit_row(real, def, n.to_string(), value)
        } else if matches!(def.kind, SettingKind::Args) {
            // Una línea de órdenes se GUARDA como array: `zed %f` viaja como
            // `["zed", "%f"]`, que es lo que el fichero declara. Escribirla
            // como cadena haría que la siguiente carga la rechazara.
            //
            // Vacío = un array vacío, que la configuración lee como «ninguno»
            // y devuelve el mando a `$VISUAL`/`$EDITOR`.
            // Una lista con vocabulario cerrado se valida AQUÍ: escrita mal,
            // la siguiente carga rechazaría el fichero entero.
            if def.id == "ui.status-items" {
                let ids: Vec<&str> = buf.split_ascii_whitespace().collect();
                if norte_config::StatusItems::parse(&ids).is_err() {
                    return Err(SettingsEditError::Invalid {
                        key: "msg-settings-invalid-status-items",
                    });
                }
            }
            let mut arr = toml_edit::Array::new();
            for tok in buf.split_ascii_whitespace() {
                arr.push(tok);
            }
            let display = buf.split_ascii_whitespace().collect::<Vec<_>>().join(" ");
            self.commit_row(real, def, display, toml_edit::Value::Array(arr))
        } else {
            // By construction, only `Text`/`Int`/`Args` open `self.edit`
            // (`Self::activate`) — this is the `Text` arm.
            self.commit_row(real, def, buf.clone(), toml_edit::Value::from(buf.as_str()))
        };
        self.edit = None;
        Ok(write)
    }

    /// OPTIMISTIC update of row `real` to `display` + builds its
    /// [`PendingWrite`] (`section`/`key` via [`wire_key`]). The hot-reload
    /// that follows ([`Self::refresh`]) corrects it if the write didn't
    /// apply (I/O failure) — this is just immediate feedback, the truth
    /// lives on disk.
    fn commit_row(
        &mut self,
        real: usize,
        def: &SettingDef,
        display: String,
        value: toml_edit::Value,
    ) -> PendingWrite {
        let (section, key) = wire_key(def.id);
        self.rows[real].value.clone_from(&display);
        PendingWrite {
            section,
            key,
            value,
            name: self.rows[real].name.clone(),
            display,
        }
    }
}

/// Next value in `values` after `current` (wrapping); if `current` isn't in
/// `values` (a config with a value the catalog no longer recognizes, or a
/// dynamic list that changed), starts at the FIRST — never panics on an
/// empty list (returns `current` untouched). `pub(crate)`: also reused by
/// [`crate::plugin_config`] (G3c) — same cycle semantics for a plugin's
/// `enum`/`bool` config keys, one source of truth.
pub(crate) fn cycle(current: &str, values: &[&str]) -> String {
    if values.is_empty() {
        return current.to_owned();
    }
    let next = values
        .iter()
        .position(|v| *v == current)
        .map_or(0, |i| (i + 1) % values.len());
    values[next].to_owned()
}

/// Whether `app.quit` should open a confirmation modal, given the
/// configured `[ui] confirm_quit` mode and whether there is pending work to
/// lose (only consulted for `Auto` — `Never`/`Always` are unconditional).
/// "Pending work" means something different per frontend (TUI:
/// `TaskBoard::has_active`; GUI: tasks/marks/inflight, see
/// `confirm_quit_task_count`) — the caller computes THAT; this is only the
/// three-way decision from the mode, and it was byte-identical in both
/// frontends before this hoist (revisión S, M6: TUI's `quit_needs_confirm`
/// and the GUI's `confirm_quit_should_open`).
#[must_use]
pub fn quit_needs_confirm(mode: norte_config::ConfirmQuit, pending: bool) -> bool {
    match mode {
        norte_config::ConfirmQuit::Never => false,
        norte_config::ConfirmQuit::Always => true,
        norte_config::ConfirmQuit::Auto => pending,
    }
}

/// Status-bar/inline message for a [`SettingsEditError`] — by CATEGORY
/// (Fluent), never ad hoc text (#73 pattern). Shared by the TUI overlay
/// (S3) and the GUI view (S4, revisión S M6): both had their own
/// byte-identical copy of this match before this hoist.
#[must_use]
pub fn edit_error_message(e: &SettingsEditError) -> String {
    match e {
        SettingsEditError::NotAnInt => t("msg-settings-invalid-int"),
        SettingsEditError::OutOfRange { min, max } => norte_i18n::ta(
            "msg-settings-invalid-range",
            &[("min", &min.to_string()), ("max", &max.to_string())],
        ),
        SettingsEditError::Invalid { key } => t(key),
    }
}

#[cfg(test)]
mod tests {
    use norte_config::{Layer, Layers};
    use norte_i18n::{Lang, t_in};

    use super::*;

    /// Every catalog entry's id is unique — a duplicate would silently
    /// shadow one entry's Fluent keys/current value with another's.
    #[test]
    fn catalog_ids_son_unicos() {
        let ids: Vec<&str> = catalog().iter().map(|d| d.id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ids.len(),
            "id duplicado en catalog(): {ids:?}"
        );
    }

    /// `fluent_name_id`/`fluent_desc_id` dash the id's dots — pinned with a
    /// concrete example so a refactor can't silently change the derivation.
    #[test]
    fn fluent_ids_dashean_los_puntos() {
        assert_eq!(
            fluent_name_id("ui.confirm-quit"),
            "setting-ui-confirm-quit-name"
        );
        assert_eq!(
            fluent_desc_id("ui.confirm-quit"),
            "setting-ui-confirm-quit-desc"
        );
        assert_eq!(
            fluent_name_id("keymap.preset"),
            "setting-keymap-preset-name"
        );
    }

    /// The F1-style coverage test: EVERY catalog entry's `name`/`desc`
    /// Fluent keys must resolve to a REAL message (not fall back to the id
    /// itself) in BOTH locales — a missing translation would otherwise only
    /// surface as a raw id leaking into the settings UI.
    #[test]
    fn fluent_keys_existen_en_ambos_locales_para_cada_entrada() {
        for def in catalog() {
            for lang in [Lang::Es, Lang::En] {
                let name_id = fluent_name_id(def.id);
                let desc_id = fluent_desc_id(def.id);
                assert_ne!(
                    t_in(lang, &name_id),
                    name_id,
                    "falta la clave Fluent {name_id} en {lang:?} (id={})",
                    def.id
                );
                assert_ne!(
                    t_in(lang, &desc_id),
                    desc_id,
                    "falta la clave Fluent {desc_id} en {lang:?} (id={})",
                    def.id
                );
            }
        }
    }

    /// Every def resolves against a DEFAULT `FrontendConfig` (no layers —
    /// same "empty config" fixture the rest of `norte-frontend`/`norte-config`
    /// use) without panicking, and never returns an id-shaped fallback that
    /// would suggest a typo in `current_value`'s match.
    #[test]
    fn current_value_resuelve_para_cada_entrada_sin_panic() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("config vacía carga");
        for def in catalog() {
            let value = current_value(def, &cfg);
            assert_ne!(
                value, def.id,
                "current_value no debería devolver el id como fallback: {}",
                def.id
            );
        }
    }

    /// Una fila de las que se ALTERNAN tiene que enseñar un valor legible, y
    /// no la cadena vacía.
    ///
    /// El test de arriba no bastaba —una celda vacía no es el id, así que
    /// pasaba— y el agujero no era cosmético: `activate` decide el siguiente
    /// valor leyendo el que se PINTA, así que con la celda vacía un `Bool`
    /// leía «no es true» y escribía `true` siempre. `ui.menu-bar` estuvo así:
    /// en el catálogo, sin brazo en `current_value`, y por tanto imposible de
    /// apagar desde esta pantalla.
    #[test]
    fn una_fila_que_se_alterna_nunca_ensena_una_celda_vacia() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("config vacía carga");
        for def in catalog() {
            let value = current_value(def, &cfg);
            match def.kind {
                SettingKind::Bool => assert!(
                    value == "true" || value == "false",
                    "{} pinta {value:?}, que no es un booleano",
                    def.id
                ),
                // Los `Enum` quedan FUERA a sabiendas: `ui.lang` sin valor
                // pinta `auto`, que no es uno de los suyos —es lo que norte
                // hace, negociar con el entorno— y la primera pulsación cae en
                // el primero de la lista igualmente. Lo que aquí se protege es
                // el caso en el que el valor pintado DECIDE el siguiente y una
                // celda vacía lo decide mal.
                //
                // Los de texto libre SÍ pueden estar vacíos: «sin fuente
                // elegida» y «sin editor elegido» son respuestas válidas.
                SettingKind::Enum(_)
                | SettingKind::Text
                | SettingKind::Args
                | SettingKind::Int { .. }
                | SettingKind::ThemeName
                | SettingKind::PresetName => {}
            }
        }
    }

    /// Teclear una línea de órdenes guarda un ARRAY, que es lo que el fichero
    /// declara: una cadena haría que la siguiente carga la rechazara.
    #[test]
    fn una_fila_de_ordenes_se_guarda_como_array() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("config vacía carga");
        let mut st = SettingsState::new(build_rows(&cfg, &[]));
        let fila = st
            .rows()
            .iter()
            .position(|r| r.def_index.map(|i| catalog()[i].id) == Some("ui.editor"))
            .expect("ui.editor está en el catálogo");
        st.set_cursor(fila);
        assert!(
            st.activate(&[], &[]).is_none(),
            "una línea de órdenes se edita, no se alterna"
        );
        for c in "zed %f".chars() {
            st.edit_push_char(c);
        }
        let write = st.edit_commit().expect("texto libre no falla");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "editor");
        assert_eq!(write.value.to_string().trim(), r#"["zed", "%f"]"#);
        assert_eq!(write.display, "zed %f");
    }

    /// `ui.confirm-quit`'s default value round-trips through `current_value`
    /// as the same wire string `[ui] confirm_quit` accepts in `norte.toml`
    /// (S2's exemplar setting — this is the one already wired end-to-end).
    #[test]
    fn current_value_confirm_quit_default_es_auto() {
        let cfg = crate::config::load(&Layers { dirs: vec![] }).expect("config vacía carga");
        let def = catalog()
            .iter()
            .find(|d| d.id == "ui.confirm-quit")
            .expect("ui.confirm-quit está en el catálogo");
        assert_eq!(current_value(def, &cfg), "auto");
    }

    /// `wire_key` splits on the FIRST `.` and dashes-to-underscores the rest
    /// — pinned with concrete examples (mirrors `fluent_ids_dashean_los_puntos`
    /// above, same derivation family, different target vocabulary).
    #[test]
    fn wire_key_deriva_seccion_y_clave_snake_case() {
        assert_eq!(
            wire_key("ui.confirm-quit"),
            ("ui", "confirm_quit".to_owned())
        );
        assert_eq!(wire_key("ui.font-size"), ("ui", "font_size".to_owned()));
        assert_eq!(wire_key("keymap.preset"), ("keymap", "preset".to_owned()));
    }

    /// Coverage: EVERY `catalog()` id resolves through `wire_key` without
    /// panicking (never true for a real id, but a future entry missing the
    /// `section.key` shape would panic here first, not in the TUI/GUI).
    #[test]
    fn wire_key_resuelve_para_cada_entrada_del_catalog() {
        for def in catalog() {
            let (section, key) = wire_key(def.id);
            assert!(!section.is_empty());
            assert!(!key.is_empty());
        }
    }

    /// `ui.confirm-quit` reflects a NON-default value loaded from
    /// `norte.toml` — not just the default path above.
    #[test]
    fn current_value_confirm_quit_refleja_config_cargada() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nconfirm_quit = \"always\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = crate::config::load(&layers).expect("carga");
        let def = catalog()
            .iter()
            .find(|d| d.id == "ui.confirm-quit")
            .expect("ui.confirm-quit está en el catálogo");
        assert_eq!(current_value(def, &cfg), "always");
    }

    // --- `build_rows` (S3/S4 hoist) ---

    /// One row per catalog entry, plus EXACTLY one informational row at the
    /// end when NO plugin declares any `[config]` key (G3c fallback shape).
    #[test]
    fn build_rows_una_fila_por_entrada_mas_la_de_plugins() {
        let rows = build_rows(&cfg_vacia(), &[]);
        assert_eq!(rows.len(), catalog().len() + 1);
        assert!(!rows[0].is_plugins_note());
        assert!(rows.last().unwrap().is_plugins_note());
        assert_eq!(rows.last().unwrap().plugin_id(), None);
    }

    fn cfg_vacia() -> FrontendConfig {
        crate::config::load(&Layers { dirs: vec![] }).expect("config vacía carga")
    }

    /// Each General row's value is EXACTLY what `current_value` (S2) would
    /// resolve for the same `def` — never a diverging copy.
    #[test]
    fn build_rows_valores_coinciden_con_current_value() {
        let cfg = cfg_vacia();
        let rows = build_rows(&cfg, &[]);
        // Por ID, no por posición: las filas salen en orden de PANTALLA
        // (sección primero) y el catálogo va agrupado por sección de
        // `norte.toml`, que es otro orden.
        for def in catalog() {
            let row = rows
                .iter()
                .find(|r| r.id() == Some(def.id))
                .unwrap_or_else(|| panic!("«{}» no está en las filas", def.id));
            assert_eq!(row.value, current_value(def, &cfg));
        }
        // El catálogo entero, más la fila informativa de Plugins.
        assert_eq!(rows.len(), catalog().len() + 1);
    }

    /// The Plugins informational row carries no value (nothing to edit) and
    /// its name/description resolve to REAL text (not the raw Fluent id) in
    /// this suite's active language.
    #[test]
    fn plugins_note_row_sin_valor_y_con_texto_traducido() {
        let rows = build_rows(&cfg_vacia(), &[]);
        let note = rows.last().unwrap();
        assert_eq!(note.value, "");
        assert_ne!(note.name, "settings-plugins-name");
        assert_ne!(note.desc, "settings-plugins-note");
    }

    /// G3c: a NON-EMPTY `plugin_summaries` yields one row PER summary
    /// (never the informational fallback), each with `plugin_id()` set and
    /// a localized `"N settings"` value — the caller's cue to drill in on
    /// Enter, never to call `SettingsState::activate` on it.
    #[test]
    fn build_rows_con_plugins_una_fila_por_resumen() {
        let summaries = vec![
            PluginConfigSummary {
                plugin_id: "org.a".into(),
                name: "Alpha".into(),
                key_count: 3,
            },
            PluginConfigSummary {
                plugin_id: "org.b".into(),
                name: "Beta".into(),
                key_count: 1,
            },
        ];
        let rows = build_rows(&cfg_vacia(), &summaries);
        assert_eq!(rows.len(), catalog().len() + 2);
        let a = &rows[catalog().len()];
        assert_eq!(a.plugin_id(), Some("org.a"));
        assert_eq!(a.name, "Alpha");
        assert!(
            a.is_plugins_note(),
            "no editable vía SettingsState::activate"
        );
        assert!(a.value.contains('3'));
        let b = &rows[catalog().len() + 1];
        assert_eq!(b.plugin_id(), Some("org.b"));
        assert!(b.value.contains('1'));
    }

    /// `PluginConfigSummary::name` is UNTRUSTED plugin text — a hostile
    /// name (bidi override, corpus `rtl_override`) reaches `Row::name`
    /// UNCHANGED by `build_rows` itself: masking is the CALLER's
    /// responsibility (same contract as `palette::plugin_rows`, which
    /// masks BEFORE building the row) — this pins that `build_rows` does
    /// not double-mask nor accidentally corrupt an already-masked name.
    #[test]
    fn build_rows_con_plugins_no_altera_un_nombre_ya_enmascarado() {
        let masked = crate::display_name("\u{202E}evil".as_bytes()).0;
        let summaries = vec![PluginConfigSummary {
            plugin_id: "org.evil".into(),
            name: masked.clone(),
            key_count: 1,
        }];
        let rows = build_rows(&cfg_vacia(), &summaries);
        assert_eq!(rows.last().unwrap().name, masked);
    }

    // --- `SettingsState`/`PendingWrite`/`SettingsEditError` (S3/S4 hoist) ---

    fn rows() -> Vec<Row> {
        build_rows(&cfg_vacia(), &[])
    }

    /// Filtering by a DASHED fragment of the id (`confirm-quit`) — unlikely
    /// in name/description prose — isolates exactly that row.
    fn only(fragment: &str) -> SettingsState {
        let mut s = SettingsState::new(rows());
        for c in fragment.chars() {
            s.push_char(c);
        }
        assert_eq!(
            s.visible().len(),
            1,
            "el fragmento {fragment:?} debería aislar una sola fila"
        );
        s
    }

    #[test]
    fn settings_filtra_por_id_nombre_o_descripcion() {
        let s = only("confirm-quit");
        assert_eq!(
            s.rows()[s.visible()[0]].name,
            t("setting-ui-confirm-quit-name")
        );
    }

    #[test]
    fn settings_query_hostil_se_enmascara() {
        let mut s = SettingsState::new(rows());
        for c in "a\u{202E}b".chars() {
            s.push_char(c);
        }
        let display = s.query_display();
        assert!(!display.chars().any(norte_encoding::is_terminal_hazard));
        assert!(display.contains('\u{FFFD}'));
    }

    #[test]
    fn settings_sin_matches_no_panica_y_activate_es_none() {
        let mut s = SettingsState::new(rows());
        for c in "zzzznuncacasa".chars() {
            s.push_char(c);
        }
        assert!(s.visible().is_empty());
        s.up();
        s.down();
        s.page_up(3);
        s.page_down(3);
        assert_eq!(s.selected_desc(), None);
        assert!(s.activate(&[], &[]).is_none());
    }

    #[test]
    fn activate_en_bool_toggla_y_devuelve_pendingwrite() {
        let mut s = only("reduce-motion");
        assert_eq!(s.rows()[s.visible()[0]].value, "false", "default");
        let write = s.activate(&[], &[]).expect("Bool activa de inmediato");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "reduce_motion");
        assert_eq!(write.value.as_bool(), Some(true));
        assert_eq!(write.display, "true");
        assert_eq!(s.rows()[s.visible()[0]].value, "true", "optimista");
        assert!(!s.is_editing());
    }

    #[test]
    fn activate_en_enum_cicla_con_wrap() {
        let mut s = only("confirm-quit");
        assert_eq!(s.rows()[s.visible()[0]].value, "auto", "default S2");
        let w1 = s.activate(&[], &[]).unwrap();
        assert_eq!(w1.display, "always");
        let w2 = s.activate(&[], &[]).unwrap();
        assert_eq!(w2.display, "never");
        let w3 = s.activate(&[], &[]).unwrap();
        assert_eq!(w3.display, "auto", "wrap al primero");
        assert_eq!(w3.value.as_str(), Some("auto"));
    }

    #[test]
    fn activate_en_theme_name_cicla_sobre_la_lista_viva() {
        // "ui.theme" es prefijo de `ui.theme-light` y `ui.theme-dark` (spec
        // 2026-09-11, V6): el filtro deja TRES filas, y el cursor queda en la
        // primera, que por orden del catálogo es la del tema a secas.
        let mut s = SettingsState::new(rows());
        for c in "ui.theme".chars() {
            s.push_char(c);
        }
        assert_eq!(s.visible().len(), 3, "theme, theme-light y theme-dark");
        assert_eq!(s.rows()[s.visible()[0]].name, t("setting-ui-theme-name"));
        let names = vec!["default".to_owned(), "nord".to_owned()];
        // El valor actual (default de S2) es "default": el próximo es "nord".
        let write = s.activate(&names, &[]).expect("ThemeName activa");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "theme");
        assert_eq!(write.value.as_str(), Some("nord"));
    }

    #[test]
    fn activate_en_preset_name_cicla_sobre_la_lista_viva() {
        let mut s = only("keymap.preset");
        let presets = ["orthodox", "vim", "cua"];
        let write = s.activate(&[], &presets).expect("PresetName activa");
        assert_eq!(write.section, "keymap");
        assert_eq!(write.key, "preset");
        assert_eq!(write.value.as_str(), Some("vim"), "orthodox → vim (wrap)");
    }

    #[test]
    fn activate_en_text_abre_edicion_sin_persistir() {
        // Espacio final: `ui.font` es PREFIJO de `ui.font-size` (el fold
        // pega `"{id} {name} {desc}"`, así que el espacio que sigue al id
        // ancla el fin de token y descarta ese otro id sin ambigüedad).
        let mut s = only("ui.font ");
        assert!(!s.is_editing());
        let write = s.activate(&[], &[]);
        assert!(write.is_none(), "Text no persiste al abrir: solo edita");
        assert!(s.is_editing());
        assert_eq!(s.edit_buffer(), Some(""));
    }

    #[test]
    fn edit_commit_en_text_persiste_lo_tecleado() {
        let mut s = only("mono-font");
        s.activate(&[], &[]);
        for c in "JetBrains Mono".chars() {
            s.edit_push_char(c);
        }
        let write = s.edit_commit().expect("Text siempre válido");
        assert_eq!(write.section, "ui");
        assert_eq!(write.key, "mono_font");
        assert_eq!(write.value.as_str(), Some("JetBrains Mono"));
        assert!(!s.is_editing());
        assert_eq!(s.rows()[s.visible()[0]].value, "JetBrains Mono");
    }

    /// La ventana no teclea carácter a carácter: su campo es nativo y entrega
    /// el texto entero al confirmar. `edit_set` es esa entrada, y fuera de
    /// una edición no hace nada.
    #[test]
    fn edit_set_reemplaza_el_buffer_entero_y_solo_editando() {
        let mut s = only("mono-font");
        s.edit_set("nada");
        assert!(!s.is_editing(), "sin edición abierta no abre una");
        s.activate(&[], &[]);
        s.edit_set("JetBrains Mono");
        assert_eq!(s.edit_buffer(), Some("JetBrains Mono"));
        let write = s.edit_commit().expect("Text siempre válido");
        assert_eq!(write.value.as_str(), Some("JetBrains Mono"));
    }

    #[test]
    fn edit_commit_en_int_valida_rango_sin_persistir_y_conserva_el_buffer() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "999".chars() {
            s.edit_push_char(c);
        }
        let err = s.edit_commit().expect_err("999 fuera de [8,32]");
        assert_eq!(err, SettingsEditError::OutOfRange { min: 8, max: 32 });
        assert!(s.is_editing(), "el buffer se conserva tras un rechazo");
        assert_eq!(s.edit_buffer(), Some("999"));
    }

    #[test]
    fn edit_commit_en_int_no_numerico_rechaza() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "abc".chars() {
            s.edit_push_char(c);
        }
        assert_eq!(s.edit_commit().unwrap_err(), SettingsEditError::NotAnInt);
    }

    #[test]
    fn edit_commit_en_int_valido_persiste() {
        let mut s = only("font-size");
        // El buffer arranca con el valor VIGENTE ("" — sin `[ui] font_size`
        // en la config vacía de este test, `current_value` ya lo documenta).
        s.activate(&[], &[]);
        assert_eq!(s.edit_buffer(), Some(""));
        for c in "16".chars() {
            s.edit_push_char(c);
        }
        let write = s.edit_commit().expect("16 está en [8,32]");
        assert_eq!(write.value.as_integer(), Some(16));
        assert_eq!(write.display, "16");
    }

    /// Revisión S, M4: `ui.font-size` acepta un valor FRACCIONARIO
    /// (`[ui] font_size` es `f32` en `norte_config`, no un entero — un
    /// `norte.toml` editado a mano con `font_size = 14.5` era imposible de
    /// re-editar desde aquí antes de este fix, el `i64::parse` estricto lo
    /// rechazaba). Round-trip: "14.5" → `Value::Float(14.5)` + `display`
    /// SIN ceros de más.
    #[test]
    fn edit_commit_en_font_size_acepta_fraccion_y_round_tripea() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "14.5".chars() {
            s.edit_push_char(c);
        }
        let write = s.edit_commit().expect("14.5 está en [8,32]");
        assert_eq!(write.value.as_float(), Some(14.5));
        assert_eq!(
            write.value.as_integer(),
            None,
            "no debe escribirse como entero"
        );
        assert_eq!(write.display, "14.5");
    }

    /// Un valor fraccionario FUERA de rango (p. ej. `33.5`) sigue
    /// rechazándose — el parse más permisivo (`f64` en vez de `i64`) no
    /// debilita la validación de `[min, max]`.
    #[test]
    fn edit_commit_en_font_size_fraccion_fuera_de_rango_rechaza() {
        let mut s = only("font-size");
        s.activate(&[], &[]);
        for c in "33.5".chars() {
            s.edit_push_char(c);
        }
        let err = s.edit_commit().expect_err("33.5 fuera de [8,32]");
        assert_eq!(err, SettingsEditError::OutOfRange { min: 8, max: 32 });
    }

    #[test]
    fn edit_cancel_no_persiste_y_conserva_el_valor_original() {
        let mut s = only("mono-font");
        let original = s.rows()[s.visible()[0]].value.clone();
        s.activate(&[], &[]);
        s.edit_push_char('x');
        s.edit_cancel();
        assert!(!s.is_editing());
        assert_eq!(s.rows()[s.visible()[0]].value, original);
    }

    /// La fila informativa de Plugins (última con query vacía) nunca abre
    /// edición ni produce un `PendingWrite`.
    #[test]
    fn activate_en_fila_informativa_de_plugins_es_no_op() {
        let mut s = SettingsState::new(rows());
        let n = catalog().len();
        for _ in 0..n {
            s.down();
        }
        assert!(s.rows()[s.visible()[s.cursor()]].is_plugins_note());
        assert!(s.activate(&[], &[]).is_none());
        assert!(!s.is_editing());
    }

    /// `refresh` (hot-reload) rebuilds the VALUES but keeps the query and
    /// cursor the user typed/moved.
    #[test]
    fn refresh_conserva_query_y_recalcula_valores() {
        let mut s = only("reduce-motion");
        assert_eq!(s.rows()[s.visible()[0]].value, "false");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nreduce_motion = true\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = crate::config::load(&layers).expect("carga");
        s.refresh(build_rows(&cfg, &[]));
        assert_eq!(
            s.visible().len(),
            1,
            "la query 'reduce-motion' se conserva tras el refresh"
        );
        assert_eq!(s.rows()[s.visible()[0]].value, "true", "valor fresco");
    }

    /// El ancla tira hacia ARRIBA y el cursor manda abajo.
    ///
    /// Con la ventana abajo, volver a la primera fila (línea 1, porque la 0
    /// es la cabecera «General») dejaba el desplazamiento en 1: la cabecera
    /// no volvía nunca. El ancla es la línea de esa cabecera.
    #[test]
    fn la_cabecera_de_la_seccion_entra_con_su_primera_fila() {
        let mut s = SettingsState::new(rows());
        // Diez líneas de caja sobre cuarenta; la ventana ya bajó.
        s.reconcile_viewport(39, 39, 40, 10);
        assert_eq!(s.viewport_offset(), 30);
        // Volver a la primera fila: su cabecera es la línea 0.
        s.reconcile_viewport(1, 0, 40, 10);
        assert_eq!(s.viewport_offset(), 0, "la cabecera vuelve con su fila");
        // Una fila que NO abre sección no tira de nada: ancla = cursor.
        s.reconcile_viewport(25, 25, 40, 10);
        assert_eq!(s.viewport_offset(), 16);
        // Y una cabecera no puede empujar el cursor fuera por abajo.
        s.reconcile_viewport(39, 38, 40, 10);
        assert!(s.viewport_offset() <= 38 && s.viewport_offset() + 10 > 39);
    }

    #[test]
    fn restablecer_una_fila_tocada_pide_quitar_su_clave() {
        let mut cfg = cfg_vacia();
        cfg.common.ui_theme = Some("nord".to_owned());
        let mut s = SettingsState::new(build_rows(&cfg, &[]));
        let pos = s
            .visible()
            .iter()
            .position(|&i| s.rows()[i].id() == Some("ui.theme"))
            .expect("ui.theme visible");
        s.set_cursor(pos);
        let r = s.reset().expect("hay algo que quitar");
        assert_eq!((r.section, r.key.as_str()), ("ui", "theme"));
    }

    #[test]
    fn restablecer_lo_que_ya_es_de_fabrica_no_pide_nada() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        s.set_cursor(0);
        assert!(s.reset().is_none());
    }

    #[test]
    fn una_fila_de_plugins_no_se_restablece() {
        let resumen = PluginConfigSummary {
            plugin_id: "org.a".into(),
            name: "A".into(),
            key_count: 2,
        };
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[resumen]));
        let ultima = s.visible().len() - 1;
        s.set_cursor(ultima);
        assert!(s.reset().is_none());
    }

    /// Editando, restablecer no hace nada: igual que el resto de esta
    /// máquina, una edición abierta congela todo lo demás.
    #[test]
    fn editando_no_se_restablece() {
        let mut cfg = cfg_vacia();
        cfg.common.ui_font = Some("Inter".to_owned());
        let mut s = SettingsState::new(build_rows(&cfg, &[]));
        let pos = s
            .visible()
            .iter()
            .position(|&i| s.rows()[i].id() == Some("ui.font"))
            .expect("ui.font visible");
        s.set_cursor(pos);
        s.activate(&[], &[]);
        assert!(s.is_editing());
        assert!(s.reset().is_none());
    }

    #[test]
    fn el_operador_modified_deja_solo_lo_tocado() {
        let mut cfg = cfg_vacia();
        cfg.common.ui_theme = Some("nord".to_owned());
        let mut s = SettingsState::new(build_rows(&cfg, &[]));
        for c in "@modified".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 1);
        assert_eq!(s.rows()[s.visible()[0]].id(), Some("ui.theme"));
    }

    /// En los DOS idiomas, y por la clave estable: un fichero de traducción
    /// no puede ser la diferencia entre encontrar algo y no encontrarlo.
    #[test]
    fn el_operador_section_acepta_el_nombre_traducido_y_el_estable() {
        for q in ["@section:appearance", "@section:apariencia"] {
            let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
            for c in q.chars() {
                s.push_char(c);
            }
            assert!(s.shown() > 0, "«{q}» no encontró nada");
            assert!(
                s.visible()
                    .iter()
                    .all(|&i| s.rows()[i].section == Section::Appearance)
            );
        }
    }

    /// CADA sección, en los DOS idiomas, por su rótulo entero y por un
    /// prefijo. Cinco de las siete tienen el rótulo de dos palabras, y la
    /// consulta se trocea por espacios: con igualdad exacta eran
    /// inencontrables, y el test que solo probaba «apariencia» —la única de
    /// una palabra en ambos idiomas— no lo veía.
    #[test]
    fn cada_seccion_se_encuentra_por_su_rotulo_en_los_dos_idiomas() {
        for s in Section::ORDER {
            let mut consultas = vec![s.stable_key().to_owned()];
            for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
                let rotulo = norte_i18n::t_in(lang, s.label_key());
                // La primera palabra: es lo que sobrevive al troceo.
                let primera = rotulo.split(' ').next().unwrap_or(&rotulo).to_owned();
                consultas.push(primera);
            }
            for q in consultas {
                assert_eq!(
                    section_by_name(&q),
                    Some(*s),
                    "«{q}» tenía que llevar a {s:?}"
                );
            }
        }
    }

    /// Y el otro operador se compara igual: plegado. Dos operadores con dos
    /// reglas de mayúsculas es una trampa.
    #[test]
    fn el_operador_modified_no_distingue_mayusculas() {
        let mut cfg = cfg_vacia();
        cfg.common.ui_theme = Some("nord".to_owned());
        for q in ["@modified", "@Modified", "@MODIFIED"] {
            let mut s = SettingsState::new(build_rows(&cfg, &[]));
            for c in q.chars() {
                s.push_char(c);
            }
            assert_eq!(s.shown(), 1, "«{q}»");
        }
    }

    #[test]
    fn los_operadores_se_combinan_con_el_texto() {
        let mut cfg = cfg_vacia();
        cfg.common.ui_theme = Some("nord".to_owned());
        cfg.common.ui_show_hidden = Some(true);
        let mut s = SettingsState::new(build_rows(&cfg, &[]));
        for c in "@modified".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 2, "dos tocadas");
        for c in " theme".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 1, "y con el texto, una");
    }

    /// Una arroba que no abre operador conocido es TEXTO. Nadie tiene que
    /// escapar nada para buscar una arroba.
    #[test]
    fn una_arroba_suelta_es_texto_normal() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        for c in "@nada".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 0);
        assert_eq!(s.total(), s.rows().len(), "el total no lo toca el filtro");
    }

    /// Una sección que no existe filtra a NADA. Ignorar el operador
    /// enseñaría la lista entera, y el lector la leería como «esto es todo
    /// lo que pediste».
    #[test]
    fn una_seccion_que_no_existe_no_ensena_la_lista_entera() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        for c in "@section:loquesea".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 0);
    }

    #[test]
    fn el_indice_lista_todas_las_secciones_aunque_el_filtro_vacie_alguna() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        // Por el ID, no por el rótulo: estos tests corren en el locale por
        // defecto, y un filtro escrito en español no casa nada en inglés.
        for c in "theme".chars() {
            s.push_char(c);
        }
        let idx = s.sections();
        assert_eq!(
            idx.len(),
            Section::ORDER.len(),
            "el índice no encoge al filtrar"
        );
        let apariencia = idx
            .iter()
            .find(|v| v.section == Section::Appearance)
            .expect("apariencia");
        assert!(apariencia.visible > 0);
        let abrir = idx
            .iter()
            .find(|v| v.section == Section::OpenWith)
            .expect("abrir con");
        assert_eq!(abrir.visible, 0);
        assert_eq!(abrir.first_row, None);
    }

    #[test]
    fn saltar_a_una_seccion_pone_el_cursor_en_su_primera_fila_visible() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        s.jump_to(Section::Input);
        let fila = &s.rows()[s.visible()[s.cursor()]];
        assert_eq!(fila.section, Section::Input);
        // Y es la PRIMERA de la sección, no una cualquiera.
        assert!(s.cursor() == 0 || s.rows()[s.visible()[s.cursor() - 1]].section != Section::Input);
    }

    /// Poner un valor concreto valida con las MISMAS reglas que el teclado:
    /// es lo que hace que un control de ventana no sea una segunda regla.
    #[test]
    fn set_value_valida_como_el_editor() {
        let temas = vec!["default".to_owned(), "nord".to_owned()];
        let presets = ["orthodox", "vim"];
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));

        // Un booleano, sin ciclar.
        let w = s
            .set_value("ui.mouse", "false", &temas, &presets)
            .expect("bool");
        assert_eq!(
            (w.section, w.key.as_str(), w.display.as_str()),
            ("ui", "mouse", "false")
        );
        assert_eq!(w.value.as_bool(), Some(false));

        // Un tema de la lista VIVA, y uno que no está.
        assert!(s.set_value("ui.theme", "nord", &temas, &presets).is_ok());
        assert!(
            s.set_value("ui.theme", "inventado", &temas, &presets)
                .is_err()
        );

        // Un entero fuera de rango se rechaza CON sus topes, como el editor.
        let e = s
            .set_value("ui.font-size", "999", &temas, &presets)
            .expect_err("fuera de rango");
        assert!(matches!(e, SettingsEditError::OutOfRange { .. }));

        // Y una línea de órdenes se trocea igual: array, no cadena.
        let w = s
            .set_value("ui.editor", "zed %f", &temas, &presets)
            .expect("args");
        assert!(w.value.as_array().is_some(), "se guarda troceada");
    }

    /// Un valor que no es de una lista cerrada no entra, venga de donde
    /// venga: el renderer no valida, y un puente puede traer cualquier cosa.
    #[test]
    fn set_value_rechaza_lo_que_no_esta_en_la_lista() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        assert!(s.set_value("ui.confirm-quit", "quizas", &[], &[]).is_err());
        assert!(s.set_value("ui.mouse", "SI", &[], &[]).is_err());
        // Los elementos de la barra de estado (ADR 0132): un id que no
        // existe, o uno repetido, rompería la siguiente carga del fichero.
        assert_eq!(
            s.set_value("ui.status-items", "tasks git", &[], &[])
                .expect_err("id desconocido"),
            SettingsEditError::Invalid {
                key: "msg-settings-invalid-status-items"
            }
        );
        assert!(
            s.set_value("ui.status-items", "tasks tasks", &[], &[])
                .is_err()
        );
        let w = s
            .set_value("ui.status-items", "notices  position", &[], &[])
            .expect("válida");
        assert_eq!(w.display, "notices position");
        assert!(s.set_value("no.existe", "1", &[], &[]).is_err());
    }

    #[test]
    fn el_control_de_cada_entrada_sale_del_catalogo() {
        assert_eq!(control_of("ui.mouse"), Some(Control::Toggle));
        assert_eq!(control_of("ui.theme"), Some(Control::ThemeChoice));
        assert_eq!(control_of("keymap.preset"), Some(Control::PresetChoice));
        assert!(matches!(control_of("ui.editor"), Some(Control::Args)));
        assert!(matches!(
            control_of("ui.confirm-quit"),
            Some(Control::Choice(_))
        ));
        // Y ninguna entrada del catálogo se queda sin control.
        for d in catalog() {
            assert!(control_of(d.id).is_some(), "«{}» sin control", d.id);
        }
    }

    /// Con el foco en el índice, las flechas recorren SECCIONES y la lista
    /// sigue — como la barra lateral de la ayuda, que abre el tema al pasar
    /// por él. Sin un segundo cursor que sincronizar.
    #[test]
    fn con_el_foco_en_el_indice_las_flechas_cambian_de_seccion() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        let seccion = |s: &SettingsState| s.rows()[s.visible()[s.cursor()]].section;
        assert_eq!(s.focus(), Focus::List);
        s.down();
        assert_eq!(
            seccion(&s),
            Section::Appearance,
            "en la lista, baja una fila"
        );

        s.toggle_focus();
        assert_eq!(s.focus(), Focus::Index);
        s.down();
        assert_eq!(
            seccion(&s),
            Section::Panes,
            "en el índice, baja una sección"
        );
        s.up();
        assert_eq!(seccion(&s), Section::Appearance);

        // Y volver al otro lado devuelve las flechas a las filas.
        s.toggle_focus();
        assert_eq!(s.focus(), Focus::List);
        let antes = s.cursor();
        s.down();
        assert_eq!(s.cursor(), antes + 1);
    }

    /// Sin filas visibles no hay sección a la que ir: el foco no cruza.
    #[test]
    fn con_la_lista_vacia_el_foco_no_pasa_al_indice() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        for c in "@section:loquesea".chars() {
            s.push_char(c);
        }
        assert_eq!(s.shown(), 0);
        s.toggle_focus();
        assert_eq!(s.focus(), Focus::List);
    }

    /// Y editando no cruza tampoco: una edición abierta congela lo demás.
    #[test]
    fn editando_el_foco_no_cambia() {
        let mut cfg = cfg_vacia();
        cfg.common.ui_font = Some("Inter".to_owned());
        let mut s = SettingsState::new(build_rows(&cfg, &[]));
        let pos = s
            .visible()
            .iter()
            .position(|&i| s.rows()[i].id() == Some("ui.font"))
            .expect("ui.font visible");
        s.set_cursor(pos);
        s.activate(&[], &[]);
        assert!(s.is_editing());
        s.toggle_focus();
        assert_eq!(s.focus(), Focus::List);
    }

    /// El recorrido de secciones, que es el MISMO en las dos pantallas.
    #[test]
    fn el_paso_de_seccion_va_y_vuelve() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        let seccion = |s: &SettingsState| s.rows()[s.visible()[s.cursor()]].section;
        assert_eq!(seccion(&s), Section::Appearance);
        assert_eq!(s.step_section(1), Some(Section::Panes));
        assert_eq!(seccion(&s), Section::Panes);
        assert_eq!(s.step_section(-1), Some(Section::Appearance));
    }

    /// En el extremo no hay a dónde ir y el cursor se queda: fingir que da
    /// la vuelta es un cursor que se teletransporta.
    #[test]
    fn el_paso_de_seccion_para_en_el_extremo() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        assert_eq!(s.step_section(-1), None);
        assert_eq!(s.cursor(), 0);
    }

    /// Una sección que el filtro vació se ATRAVIESA, y si no queda ninguna
    /// con filas, no se mueve nada.
    #[test]
    fn el_paso_de_seccion_se_salta_las_vacias() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        // «theme» solo deja filas en Apariencia — ni la nota de Plugins,
        // cuyo heno no lleva esa palabra.
        for c in "theme".chars() {
            s.push_char(c);
        }
        let antes = s.cursor();
        assert_eq!(s.step_section(1), None);
        assert_eq!(s.cursor(), antes);
    }

    #[test]
    fn saltar_a_una_seccion_vacia_no_mueve_nada() {
        let mut s = SettingsState::new(build_rows(&cfg_vacia(), &[]));
        for c in "theme".chars() {
            s.push_char(c);
        }
        let antes = s.cursor();
        s.jump_to(Section::OpenWith);
        assert_eq!(
            s.cursor(),
            antes,
            "una sección sin filas visibles no mueve el cursor"
        );
    }

    /// El punto de «esto lo has tocado tú» se calcula contra el valor DE
    /// FÁBRICA, con la misma función que pinta el valor: una tabla de
    /// defectos escrita a mano se desincroniza del esquema en cuanto alguien
    /// cambia uno.
    #[test]
    fn sobre_la_config_por_defecto_no_hay_nada_modificado() {
        for r in build_rows(&cfg_vacia(), &[]) {
            assert!(!r.modified, "«{}» no debería salir modificada", r.name);
        }
    }

    #[test]
    fn cambiar_un_campo_enciende_el_punto_de_esa_fila_y_de_ninguna_otra() {
        let mut cfg = cfg_vacia();
        cfg.common.ui_theme = Some("nord".to_owned());
        let filas = build_rows(&cfg, &[]);
        let tocadas: Vec<_> = filas
            .iter()
            .filter(|r| r.modified)
            .map(super::Row::id)
            .collect();
        assert_eq!(tocadas, vec![Some("ui.theme")]);
    }

    /// Una fila que no sale del catálogo nunca está modificada: no hay valor
    /// de fábrica con el que compararla.
    #[test]
    fn una_fila_de_plugins_no_esta_modificada() {
        let resumen = PluginConfigSummary {
            plugin_id: "org.a".into(),
            name: "A".into(),
            key_count: 2,
        };
        let filas = build_rows(&cfg_vacia(), &[resumen]);
        let fila = filas.last().expect("hay fila de plugin");
        assert_eq!(fila.section, Section::Plugins);
        assert!(!fila.modified);
    }

    /// Ninguna entrada se queda sin sitio. Un id nuevo sin sección cae en
    /// «Comportamiento» por el `unwrap_or` de `SettingDef::section`, y este
    /// test es lo único que separa ese apaño de un archivado en silencio.
    #[test]
    fn cada_entrada_del_catalogo_tiene_seccion() {
        for d in catalog() {
            assert!(
                section_of(d.id).is_some(),
                "«{}» no está repartida en ninguna sección",
                d.id
            );
        }
    }

    /// Y ninguna sección del catálogo se queda vacía: una sección que el
    /// índice lista y nunca tiene nada es una promesa rota.
    #[test]
    fn cada_seccion_del_catalogo_tiene_al_menos_una_entrada() {
        for s in Section::ORDER {
            if matches!(s, Section::Plugins | Section::Paths) {
                continue; // No salen del catálogo.
            }
            assert!(
                catalog().iter().any(|d| d.section() == *s),
                "la sección {s:?} no tiene ninguna entrada"
            );
        }
    }

    /// Cada sección se dice en los dos idiomas. Media pantalla traducida es
    /// peor que ninguna.
    #[test]
    fn cada_seccion_tiene_su_rotulo_en_ambos_locales() {
        for s in Section::ORDER {
            for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
                let txt = norte_i18n::t_in(lang, s.label_key());
                assert!(
                    !txt.is_empty() && !txt.contains(s.label_key()),
                    "{s:?} sin traducir en {lang:?}: {txt}"
                );
            }
        }
    }

    /// Avanzar y retroceder por el índice no da la vuelta: en los extremos
    /// no hay a dónde ir, y fingir que sí es un cursor que se teletransporta.
    #[test]
    fn el_paso_entre_secciones_para_en_los_extremos() {
        assert_eq!(Section::Appearance.step(-1), None);
        assert_eq!(Section::Appearance.step(1), Some(Section::Panes));
        assert_eq!(Section::Paths.step(1), None);
        assert_eq!(Section::Paths.step(-1), Some(Section::Plugins));
    }

    #[test]
    fn set_cursor_clampa_al_ultimo_visible() {
        let mut s = SettingsState::new(rows());
        let last = s.visible().len() - 1;
        s.set_cursor(last + 50);
        assert_eq!(s.cursor(), last, "clampa al último visible");
        s.set_cursor(0);
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn set_cursor_es_no_op_mientras_se_edita() {
        let mut s = SettingsState::new(rows());
        // Sin filtrar: TODAS las filas siguen visibles, así que si el guard
        // de edición fallara habría a dónde moverse de verdad. Se busca
        // `ui.font` por su ID —una fila `Text`, que al activarse abre
        // edición—, no por su posición: las filas salen en orden de
        // pantalla, que no es el del catálogo.
        let idx = s
            .visible()
            .iter()
            .position(|&i| s.rows()[i].id() == Some("ui.font"))
            .expect("ui.font visible");
        s.set_cursor(idx);
        assert_eq!(s.rows()[s.visible()[idx]].name, t("setting-ui-font-name"));
        s.activate(&[], &[]);
        assert!(s.is_editing());
        s.set_cursor(0);
        assert_eq!(
            s.cursor(),
            idx,
            "editando, un click en otra fila no mueve el cursor"
        );
    }

    #[test]
    fn row_id_devuelve_el_id_del_catalogo_o_none_para_la_nota_de_plugins() {
        let rows = rows();
        // Cada id del catálogo sale UNA vez; el orden es el de pantalla
        // (sección primero), no el del catálogo.
        for def in catalog() {
            assert_eq!(
                rows.iter().filter(|r| r.id() == Some(def.id)).count(),
                1,
                "«{}» tiene que salir exactamente una vez",
                def.id
            );
        }
        assert_eq!(rows.last().unwrap().id(), None);
    }

    #[test]
    fn cycle_envuelve_y_arranca_en_el_primero_si_no_encuentra() {
        let values = ["a", "b", "c"];
        assert_eq!(cycle("a", &values), "b");
        assert_eq!(cycle("c", &values), "a", "wrap");
        assert_eq!(
            cycle("x", &values),
            "a",
            "no encontrado: arranca en el primero"
        );
        assert_eq!(cycle("a", &[]), "a", "lista vacía: no panica, no cambia");
    }

    // --- `quit_needs_confirm`/`edit_error_message` (revisión S, M6 hoist) ---

    #[test]
    fn quit_needs_confirm_los_tres_modos() {
        use norte_config::ConfirmQuit;
        assert!(
            !quit_needs_confirm(ConfirmQuit::Never, true),
            "Never: jamás"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Always, false),
            "Always: siempre"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Auto, true),
            "Auto: sigue a pending"
        );
        assert!(!quit_needs_confirm(ConfirmQuit::Auto, false));
    }

    #[test]
    fn edit_error_message_por_categoria_nunca_vacio() {
        assert!(!edit_error_message(&SettingsEditError::NotAnInt).is_empty());
        let msg = edit_error_message(&SettingsEditError::OutOfRange { min: 8, max: 32 });
        assert!(!msg.is_empty());
        assert!(msg.contains('8') && msg.contains("32"), "{msg}");
    }
}
