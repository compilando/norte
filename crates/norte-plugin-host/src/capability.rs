//! Capabilities de un plugin (ADR 0022 D4): lo que el manifiesto DECLARA y el
//! host hace cumplir. `exec` es SIEMPRE `none` (spec §7.1) — se valida al
//! parsear el manifiesto, no se representa aquí.

use serde::Deserialize;

/// Alcance de un permiso de FS: nada, o solo lo que el host abre y pasa (jamás
/// el FS a pelo — regla dura 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// Sin acceso.
    #[default]
    None,
    /// Solo los recursos que el host entrega explícitamente.
    Scoped,
}

impl Scope {
    /// `true` si concede algún acceso (para pintar el badge).
    #[must_use]
    pub fn granted(self) -> bool {
        matches!(self, Scope::Scoped)
    }
}

/// Permiso de red: una allow-list de hosts.
#[derive(Debug, Clone, Deserialize)]
pub struct NetCap {
    /// Hosts a los que el plugin puede conectar (exacto, sin comodines por ahora).
    pub hosts: Vec<String>,
}

/// El bloque `[capabilities]` del manifiesto, ya validado. Un permiso ausente =
/// `None`/vacío: sin syscall.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Lectura de FS.
    #[serde(default, rename = "fs-read")]
    pub fs_read: Scope,
    /// Escritura de FS.
    #[serde(default, rename = "fs-write")]
    pub fs_write: Scope,
    /// Red (allow-list de hosts); ausente = sin red.
    #[serde(default)]
    pub net: Option<NetCap>,
    /// Acceso a IA (`ai = "chat"`); ausente = sin IA. Se guarda el string
    /// crudo (los modos concretos se tipan en M4-ai).
    #[serde(default)]
    pub ai: Option<String>,
    /// `exec`: DEBE ser `none` o estar ausente. Se valida y descarta al parsear
    /// el manifiesto ([`crate::Manifest::from_toml`]); jamás se expone aquí.
    #[serde(default)]
    pub(crate) exec: Option<String>,
}

impl Capabilities {
    /// Etiquetas cortas de los permisos concedidos, para el badge del gestor
    /// (ADR 0022 D5): p. ej. `["fs-read", "net"]`.
    #[must_use]
    pub fn badges(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.fs_read.granted() {
            out.push("fs-read");
        }
        if self.fs_write.granted() {
            out.push("fs-write");
        }
        if self.net.is_some() {
            out.push("net");
        }
        if self.ai.is_some() {
            out.push("ai");
        }
        out
    }
}
