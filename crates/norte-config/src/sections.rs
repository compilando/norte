//! Las secciones que solo se fusionan campo a campo, «gana la última capa»:
//! `[log]`, `[daemon]` y `[archive]`, ya fusionadas.
//!
//! Antes cada clave vivía en seis sitios: el schema, un campo suelto en
//! [`CommonConfig`](crate::CommonConfig), su acumulador en `load`, su
//! `merge_*_layer`, el literal que construye la config y la lista de claves
//! que avisa cuando un perfil la pide. Esa lista ya había olvidado una:
//! `[log] format` en un perfil se descartaba sin aviso. Aquí son cuatro, y
//! las dos que se pueden olvidar —`merge` y `declara`— desestructuran la
//! sección del schema SIN `..`: una clave nueva que ninguna de las dos mire
//! no compila.
//!
//! Qué capas pueden fijarlas NO se decide aquí. Las tres son de las que no
//! son presentación (dónde escribe un proceso, a qué socket habla, qué
//! programa lee un RAR), y el filtro que las deja fuera de la capa de
//! proyecto está en `load`, por sección, en la llamada a `merge`.

use std::path::PathBuf;

use crate::schema::{ArchiveSection, DaemonMode, DaemonSection, LogFormat, LogSection};

/// `[log]` fusionada. Nunca desde la capa de proyecto: elegir dónde escribe
/// un proceso no es presentación.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogSettings {
    /// `[log] dir`. `None` = `<state_dir>/logs`.
    pub dir: Option<PathBuf>,
    /// `[log] retain`: cuántos ficheros rotados sobreviven. `None` = el
    /// valor por defecto del appender.
    pub retain: Option<usize>,
    /// `[log] format`: cómo se escribe el FICHERO (ADR 0127).
    pub format: LogFormat,
}

impl LogSettings {
    /// ¿Declara esta capa alguna clave de `[log]`? Es lo que decide si una
    /// capa que no puede fijarla tiene que AVISAR de que se ignora.
    #[must_use]
    pub fn declara(capa: &LogSection) -> bool {
        let LogSection {
            dir,
            retain,
            format,
        } = capa;
        dir.is_some() || retain.is_some() || format.is_some()
    }

    /// Fusiona una capa: cada clave presente pisa a la anterior.
    ///
    /// ```
    /// use norte_config::{LogFormat, LogSettings};
    /// use norte_config::schema::LogSection;
    ///
    /// let mut log = LogSettings::default();
    /// log.merge(LogSection { retain: Some(3), format: Some(LogFormat::Json), ..Default::default() });
    /// log.merge(LogSection { retain: Some(5), ..Default::default() });
    /// assert_eq!(log.retain, Some(5));
    /// assert_eq!(log.format, LogFormat::Json, "una capa que no la dice no la borra");
    /// ```
    pub fn merge(&mut self, capa: LogSection) {
        let LogSection {
            dir,
            retain,
            format,
        } = capa;
        if dir.is_some() {
            self.dir = dir;
        }
        if retain.is_some() {
            self.retain = retain;
        }
        if let Some(f) = format {
            self.format = f;
        }
    }
}

/// `[daemon]` fusionada. Se lee al arrancar y no se recarga en caliente.
/// Nunca desde la capa de proyecto (MAJOR-1 de la revisión): un repositorio
/// ajeno no redirige el transporte.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonSettings {
    /// `[daemon] mode`. `None` = embebido.
    pub mode: Option<DaemonMode>,
    /// `[daemon] socket`. `None` = el del sistema operativo.
    pub socket: Option<PathBuf>,
}

impl DaemonSettings {
    /// ¿Declara esta capa alguna clave de `[daemon]`? Ver
    /// [`LogSettings::declara`].
    #[must_use]
    pub fn declara(capa: &DaemonSection) -> bool {
        let DaemonSection { mode, socket } = capa;
        mode.is_some() || socket.is_some()
    }

    /// Fusiona una capa: cada clave presente pisa a la anterior.
    ///
    /// ```
    /// use norte_config::{DaemonMode, DaemonSettings};
    /// use norte_config::schema::DaemonSection;
    ///
    /// let mut d = DaemonSettings::default();
    /// d.merge(DaemonSection { mode: Some(DaemonMode::Daemon), socket: None });
    /// assert_eq!(d.mode, Some(DaemonMode::Daemon));
    /// assert_eq!(d.socket, None);
    /// ```
    pub fn merge(&mut self, capa: DaemonSection) {
        let DaemonSection { mode, socket } = capa;
        if mode.is_some() {
            self.mode = mode;
        }
        if socket.is_some() {
            self.socket = socket;
        }
    }
}

/// `[archive]` fusionada: los límites anti-bomba locales y el programa que lee
/// RAR. Nunca desde la capa de proyecto: `rar_delegate` nombra un ejecutable,
/// y honrarlo desde el `.norte.toml` de un repositorio sería ejecutar código
/// arbitrario al entrar en el directorio.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveSettings {
    /// `[archive] max_entries`. `None` = el tope compilado.
    pub max_entries: Option<u64>,
    /// `[archive] max_decompressed_bytes`. `None` = el tope compilado.
    pub max_decompressed_bytes: Option<u64>,
    /// `[archive] max_nesting` (#56). `None` = el tope compilado.
    pub max_nesting: Option<usize>,
    /// `[archive] rar_delegate` (ruta absoluta). `None` = sondear `PATH`.
    pub rar_delegate: Option<String>,
}

impl ArchiveSettings {
    /// ¿Declara esta capa alguna clave de `[archive]`? Ver
    /// [`LogSettings::declara`].
    #[must_use]
    pub fn declara(capa: &ArchiveSection) -> bool {
        let ArchiveSection {
            max_entries,
            max_decompressed_bytes,
            max_nesting,
            rar_delegate,
        } = capa;
        max_entries.is_some()
            || max_decompressed_bytes.is_some()
            || max_nesting.is_some()
            || rar_delegate.is_some()
    }

    /// Fusiona una capa: cada clave presente pisa a la anterior.
    ///
    /// ```
    /// use norte_config::ArchiveSettings;
    /// use norte_config::schema::ArchiveSection;
    ///
    /// let mut a = ArchiveSettings::default();
    /// a.merge(ArchiveSection { max_entries: Some(10), ..Default::default() });
    /// a.merge(ArchiveSection { max_nesting: Some(2), ..Default::default() });
    /// assert_eq!((a.max_entries, a.max_nesting), (Some(10), Some(2)));
    /// ```
    pub fn merge(&mut self, capa: ArchiveSection) {
        let ArchiveSection {
            max_entries,
            max_decompressed_bytes,
            max_nesting,
            rar_delegate,
        } = capa;
        if max_entries.is_some() {
            self.max_entries = max_entries;
        }
        if max_decompressed_bytes.is_some() {
            self.max_decompressed_bytes = max_decompressed_bytes;
        }
        if max_nesting.is_some() {
            self.max_nesting = max_nesting;
        }
        if rar_delegate.is_some() {
            self.rar_delegate = rar_delegate;
        }
    }
}
