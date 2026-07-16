//! Colores por TIPO de archivo (estilo `LS_COLORS`, ADR 0020 D2): por `kind`
//! (dir/symlink/exec…) y por EXTENSIÓN. La resolución es extensión > kind >
//! rol `regular`.

use std::collections::HashMap;

use serde::Deserialize;

use crate::style::Style;

/// Tipo de nodo del filesystem, para colorear la entrada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum FileKind {
    /// Directorio.
    Dir,
    /// Symlink.
    Symlink,
    /// Fichero regular ejecutable.
    Executable,
    /// FIFO / named pipe.
    Fifo,
    /// Socket.
    Socket,
    /// Dispositivo de bloque.
    BlockDevice,
    /// Dispositivo de carácter.
    CharDevice,
    /// Fichero regular (sin distinción especial).
    Regular,
}

/// Estilos por tipo de archivo. `[files.kind]` colorea por clase; `[files.ext]`
/// por extensión (más específico, gana).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct FileColors {
    /// Por clase de nodo.
    pub kind: HashMap<FileKind, Style>,
    /// Por extensión, en minúsculas ASCII (la búsqueda también minuscula).
    pub ext: HashMap<String, Style>,
}

impl FileColors {
    /// El [`Style`] para una entrada `name` (bytes, regla 1) de tipo `kind`, o
    /// `None` si el tema no la colorea (el caller cae al rol `regular`).
    /// Prioridad: extensión > kind.
    #[must_use]
    pub fn style_for(&self, name: &[u8], kind: FileKind) -> Option<Style> {
        if let Some(ext) = extension_of(name) {
            // Minúsculas ASCII para casar de forma amable; un byte no-UTF8 en
            // la extensión simplemente no casa ninguna clave (cae a kind).
            if let Ok(ext_str) = std::str::from_utf8(ext) {
                let key = ext_str.to_ascii_lowercase();
                if let Some(s) = self.ext.get(&key) {
                    return Some(*s);
                }
            }
        }
        self.kind.get(&kind).copied()
    }
}

/// La extensión de `name` = bytes tras el ÚLTIMO `.`, si lo hay y no es un
/// fichero oculto sin extensión (`.bashrc` no tiene extensión `bashrc`).
#[must_use]
pub fn extension_of(name: &[u8]) -> Option<&[u8]> {
    let dot = name.iter().rposition(|&b| b == b'.')?;
    // `.` en la posición 0 (oculto) o final (sin ext) no cuentan.
    if dot == 0 || dot + 1 == name.len() {
        return None;
    }
    Some(&name[dot + 1..])
}
