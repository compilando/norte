//! Qué programa externo lee el RAR, y cómo se encuentra.

use std::path::PathBuf;

/// Los fallos propios de la delegación, con el detalle que
/// [`norte_proto::Error`] no puede llevar por el cable.
///
/// La conversión al error de protocolo es deliberadamente pobre —
/// `Unsupported` — porque el cable no transporta prosa; la frase vive aquí,
/// para el log y para `norte doctor`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RarError {
    /// No hay ningún lector de RAR instalado.
    ///
    /// El mensaje NOMBRA qué instalar a propósito: un `.rar` que se abre y no
    /// enseña nada no le enseña nada al usuario.
    #[error("no RAR reader found: install `7z` (p7zip) or `unrar` and try again")]
    NoDelegate,
}

/// El programa externo que hace de lector de RAR.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Delegate {
    /// `7z` o `7zz` (p7zip). Preferido: conserva los bytes crudos del nombre.
    SevenZip(PathBuf),
    /// `unrar`.
    Unrar(PathBuf),
}

/// Los ejecutables que se sondean, **en orden de preferencia**.
///
/// El orden está medido, no elegido por gusto: `unrar` TRUNCA un nombre no
/// UTF-8 en su listado (`cp437-\xa4\xa5.txt` sale como `cp437-`, sin
/// extensión), y `7z -slt` lo entrega entero. Un provider que pierde la
/// extensión de un fichero no es aceptable mientras haya alternativa.
const CANDIDATES: [&str; 3] = ["7z", "7zz", "unrar"];

impl Delegate {
    /// Sondea `PATH` en busca de un lector: `7z`, `7zz`, `unrar`.
    ///
    /// Toca el sistema de ficheros (un `is_file` por candidato y directorio de
    /// `PATH`), así que se llama UNA vez fuera del camino async — al construir
    /// el provider —, nunca por operación.
    ///
    /// # Errors
    ///
    /// [`RarError::NoDelegate`] si ninguno está instalado.
    pub fn discover() -> Result<Self, RarError> {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut found = Vec::new();
        for dir in std::env::split_paths(&path) {
            for exe in CANDIDATES {
                let candidate = dir.join(exe);
                if candidate.is_file() {
                    found.push((exe, candidate));
                }
            }
        }
        Self::discover_in(&found)
    }

    /// La mitad pura de [`discover`](Self::discover): elige entre candidatos ya
    /// resueltos, respetando el orden de preferencia y no el de llegada.
    ///
    /// # Errors
    ///
    /// [`RarError::NoDelegate`] si la lista viene vacía o no trae ningún
    /// nombre conocido.
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use norte_vfs_rar::Delegate;
    ///
    /// let elegido = Delegate::discover_in(&[
    ///     ("unrar", PathBuf::from("/usr/bin/unrar")),
    ///     ("7z", PathBuf::from("/usr/bin/7z")),
    /// ])
    /// .expect("hay candidatos");
    /// assert!(matches!(elegido, Delegate::SevenZip(_)));
    /// ```
    pub fn discover_in(candidates: &[(&str, PathBuf)]) -> Result<Self, RarError> {
        for exe in CANDIDATES {
            if let Some((_, path)) = candidates.iter().find(|(name, _)| *name == exe) {
                return Ok(match exe {
                    "unrar" => Self::Unrar(path.clone()),
                    _ => Self::SevenZip(path.clone()),
                });
            }
        }
        Err(RarError::NoDelegate)
    }

    /// La ruta absoluta del ejecutable elegido.
    #[must_use]
    pub fn program(&self) -> &PathBuf {
        match self {
            Self::SevenZip(p) | Self::Unrar(p) => p,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_delegado_el_error_nombra_el_ejecutable() {
        let err = Delegate::discover_in(&[]).expect_err("sin candidatos falla");
        let msg = err.to_string();
        assert!(
            msg.contains("7z") && msg.contains("unrar"),
            "el error debe decir QUÉ instalar: {msg}"
        );
    }

    #[test]
    fn se_prefiere_7z_a_unrar() {
        // Orden medido, no gusto: unrar TRUNCA un nombre no-UTF8 en el listado.
        let found = Delegate::discover_in(&[
            ("unrar", PathBuf::from("/usr/bin/unrar")),
            ("7z", PathBuf::from("/usr/bin/7z")),
        ])
        .expect("hay candidatos");
        assert!(matches!(found, Delegate::SevenZip(_)), "7z gana a unrar");
    }

    #[test]
    fn siete_zeta_zeta_tambien_vale_y_va_antes_que_unrar() {
        let found = Delegate::discover_in(&[
            ("unrar", PathBuf::from("/usr/bin/unrar")),
            ("7zz", PathBuf::from("/opt/7zz")),
        ])
        .expect("hay candidatos");
        assert_eq!(found, Delegate::SevenZip(PathBuf::from("/opt/7zz")));
    }

    #[test]
    fn solo_unrar_se_acepta() {
        let found = Delegate::discover_in(&[("unrar", PathBuf::from("/usr/bin/unrar"))])
            .expect("unrar sirve");
        assert_eq!(found, Delegate::Unrar(PathBuf::from("/usr/bin/unrar")));
    }
}
