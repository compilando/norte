//! Sections merged only field by field, "the last layer wins": `[log]`,
//! `[daemon]` and `[archive]`, already merged.
//!
//! Each key used to live in six places: the schema, a loose field in
//! [`CommonConfig`](crate::CommonConfig), its accumulator in `load`, its
//! `merge_*_layer`, the literal that builds the config, and the list of keys
//! that warns when a profile requests it. That list had already forgotten
//! one: `[log] format` in a profile was dropped with no warning. Here there
//! are four, and the two that are easy to forget — `merge` and `declares`
//! (TODO(translation): review — the doc kept the Spanish name because it is
//! the method's real identifier, a pub item this task must not rename) —
//! destructure the schema section WITHOUT `..`: a new key that neither one
//! looks at does not compile.
//!
//! Which layers may set them is NOT decided here. All three are the
//! non-presentation kind (where a process writes, which socket it talks to,
//! what program reads a RAR), and the filter that keeps them out of the
//! project layer is in `load`, per section, at the call to `merge`.

use std::path::PathBuf;

use crate::schema::{ArchiveSection, DaemonMode, DaemonSection, LogFormat, LogSection};

/// `[log]`, merged. Never from the project layer: choosing where a process
/// writes is not presentation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogSettings {
    /// `[log] dir`. `None` = `<state_dir>/logs`.
    pub dir: Option<PathBuf>,
    /// `[log] retain`: how many rotated files survive. `None` = the
    /// appender's default value.
    pub retain: Option<usize>,
    /// `[log] format`: how the FILE is written (ADR 0127).
    pub format: LogFormat,
}

impl LogSettings {
    /// Does this layer declare any `[log]` key? This is what decides whether
    /// a layer that may not set it has to WARN that it is being ignored.
    #[must_use]
    pub fn declares(layer: &LogSection) -> bool {
        let LogSection {
            dir,
            retain,
            format,
        } = layer;
        dir.is_some() || retain.is_some() || format.is_some()
    }

    /// Merges a layer: each key present overrides the previous one.
    ///
    /// ```
    /// use norte_config::{LogFormat, LogSettings};
    /// use norte_config::schema::LogSection;
    ///
    /// let mut log = LogSettings::default();
    /// log.merge(LogSection { retain: Some(3), format: Some(LogFormat::Json), ..Default::default() });
    /// log.merge(LogSection { retain: Some(5), ..Default::default() });
    /// assert_eq!(log.retain, Some(5));
    /// assert_eq!(log.format, LogFormat::Json, "a layer that does not say it does not clear it");
    /// ```
    pub fn merge(&mut self, layer: LogSection) {
        let LogSection {
            dir,
            retain,
            format,
        } = layer;
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

/// `[daemon]`, merged. Read at startup and never hot-reloaded. Never from the
/// project layer (MAJOR-1 of the review): a foreign repository does not
/// redirect the transport.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonSettings {
    /// `[daemon] mode`. `None` = embedded.
    pub mode: Option<DaemonMode>,
    /// `[daemon] socket`. `None` = the operating system's.
    pub socket: Option<PathBuf>,
}

impl DaemonSettings {
    /// Does this layer declare any `[daemon]` key? See
    /// [`LogSettings::declares`].
    #[must_use]
    pub fn declares(layer: &DaemonSection) -> bool {
        let DaemonSection { mode, socket } = layer;
        mode.is_some() || socket.is_some()
    }

    /// Merges a layer: each key present overrides the previous one.
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
    pub fn merge(&mut self, layer: DaemonSection) {
        let DaemonSection { mode, socket } = layer;
        if mode.is_some() {
            self.mode = mode;
        }
        if socket.is_some() {
            self.socket = socket;
        }
    }
}

/// `[archive]`, merged: the local anti-bomb limits and the program that reads
/// RAR. Never from the project layer: `rar_delegate` names an executable, and
/// honoring it from a repository's `.norte.toml` would be running arbitrary
/// code on entering the directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveSettings {
    /// `[archive] max_entries`. `None` = the compiled-in cap.
    pub max_entries: Option<u64>,
    /// `[archive] max_decompressed_bytes`. `None` = the compiled-in cap.
    pub max_decompressed_bytes: Option<u64>,
    /// `[archive] max_nesting` (#56). `None` = the compiled-in cap.
    pub max_nesting: Option<usize>,
    /// `[archive] rar_delegate` (absolute path). `None` = probe `PATH`.
    pub rar_delegate: Option<String>,
}

impl ArchiveSettings {
    /// Does this layer declare any `[archive]` key? See
    /// [`LogSettings::declares`].
    #[must_use]
    pub fn declares(layer: &ArchiveSection) -> bool {
        let ArchiveSection {
            max_entries,
            max_decompressed_bytes,
            max_nesting,
            rar_delegate,
        } = layer;
        max_entries.is_some()
            || max_decompressed_bytes.is_some()
            || max_nesting.is_some()
            || rar_delegate.is_some()
    }

    /// Merges a layer: each key present overrides the previous one.
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
    pub fn merge(&mut self, layer: ArchiveSection) {
        let ArchiveSection {
            max_entries,
            max_decompressed_bytes,
            max_nesting,
            rar_delegate,
        } = layer;
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
