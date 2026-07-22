//! Openers declarativos (#28, spec §7.3, ADR 0010): mapean mimetype+OS a un
//! binario externo elegido por el usuario (`bat`, `delta`, `unrar`, `xdg-open`…)
//! con códigos de campo `%f`/`%F`/`%d`. Son CONFIGURACIÓN, no plugins: los
//! plugins WASM no tienen `exec` (spec §7.1), así que este es el único camino
//! para delegar en herramientas externas — detección en runtime y degradación
//! limpia («instala X»), jamás linkado.
//!
//! Este módulo es RESOLUCIÓN PURA (parseo, selección por mimetype+OS,
//! construcción del argv byte-safe, sondeo del PATH); el `spawn` efectivo lo
//! hace el frontend. Sin I/O de red ni de disco salvo el sondeo del binario.

use std::ffi::OsStr;
use std::path::Path;

use serde::Deserialize;

/// Error de carga de `openers.toml`.
#[derive(Debug, thiserror::Error)]
pub enum OpenerError {
    /// El TOML no parsea o tiene claves desconocidas.
    #[error("openers.toml: {0}")]
    Toml(String),
    /// Una entrada con `command` vacío (no hay binario que lanzar).
    #[error("opener para {mime:?}: `command` no puede estar vacío")]
    EmptyCommand {
        /// El mimetype de la entrada inválida.
        mime: String,
    },
}

/// Una entrada de `openers.toml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Opener {
    /// Glob de mimetype: `text/*` o exacto `application/pdf`.
    mime: String,
    /// OS al que aplica (`linux`/`macos`/`windows`, valores de
    /// [`std::env::consts::OS`]); ausente = cualquiera.
    #[serde(default)]
    os: Option<String>,
    /// argv plantilla: `["bat", "--paging=always", "%f"]`. El primer token es
    /// el binario; los códigos de campo `%f`/`%F`/`%d` se sustituyen SOLO como
    /// tokens completos (nunca dentro de un literal — así una ruta no-UTF8
    /// jamás se concatena con texto y se preserva byte a byte, regla 1).
    command: Vec<String>,
}

/// `openers.toml` parseado.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenersConfig {
    /// Las entradas, en orden de declaración.
    #[serde(default, rename = "opener")]
    openers: Vec<Opener>,
}

impl OpenersConfig {
    /// Config vacía (ningún opener): el default cuando el fichero no existe.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parsea un `openers.toml`.
    ///
    /// # Errors
    /// [`OpenerError::Toml`] si no parsea o hay claves desconocidas;
    /// [`OpenerError::EmptyCommand`] si una entrada trae `command` vacío.
    ///
    /// ```
    /// let cfg = norte_frontend::openers::OpenersConfig::parse(
    ///     "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
    /// )
    /// .unwrap();
    /// assert!(cfg.resolve("text/plain").is_some());
    /// ```
    pub fn parse(s: &str) -> Result<Self, OpenerError> {
        let cfg: Self = toml::from_str(s).map_err(|e| OpenerError::Toml(e.message().to_owned()))?;
        for o in &cfg.openers {
            if o.command.is_empty() {
                return Err(OpenerError::EmptyCommand {
                    mime: o.mime.clone(),
                });
            }
        }
        Ok(cfg)
    }

    /// El opener para `mime` en el OS actual: entre las entradas cuyo glob de
    /// mimetype casa, gana la específica de ESTE OS sobre la agnóstica; a
    /// igualdad, la primera declarada. `None` si ninguna aplica.
    #[must_use]
    pub fn resolve(&self, mime: &str) -> Option<&Opener> {
        self.resolve_for(mime, std::env::consts::OS)
    }

    /// Como [`Self::resolve`] pero con el OS explícito (testeable sin depender
    /// del OS del runner).
    #[must_use]
    pub fn resolve_for(&self, mime: &str, os: &str) -> Option<&Opener> {
        let matches = |o: &&Opener| mimetype_matches(&o.mime, mime);
        // Específica de este OS primero; luego la agnóstica (os = None).
        self.openers
            .iter()
            .find(|o| o.os.as_deref() == Some(os) && matches(o))
            .or_else(|| self.openers.iter().find(|o| o.os.is_none() && matches(o)))
    }
}

impl Opener {
    /// El binario a lanzar (primer token del `command`). Nunca vacío: la
    /// construcción lo garantiza ([`OpenersConfig::parse`]).
    #[must_use]
    pub fn program(&self) -> &str {
        &self.command[0]
    }

    /// El argv completo con los códigos de campo sustituidos. `%f` → la
    /// PRIMERA ruta; `%F` → TODAS (un arg por ruta); `%d` → el directorio.
    /// Sustitución SOLO de tokens completos y byte-safe: una ruta viaja como
    /// `OsStr` sin pasar por `String` (regla 1). Un código de campo cuyo input
    /// esté vacío se OMITE (no se lanza el binario con un token literal `%f`).
    #[must_use]
    pub fn argv(&self, files: &[&Path], dir: &Path) -> Vec<std::ffi::OsString> {
        let mut out = Vec::with_capacity(self.command.len());
        // El primer token (binario) NUNCA se interpola.
        out.push(self.command[0].as_str().into());
        for tok in &self.command[1..] {
            match tok.as_str() {
                "%f" => {
                    if let Some(first) = files.first() {
                        out.push(first.as_os_str().to_os_string());
                    }
                }
                "%F" => out.extend(files.iter().map(|p| p.as_os_str().to_os_string())),
                "%d" => out.push(dir.as_os_str().to_os_string()),
                lit => out.push(OsStr::new(lit).to_os_string()),
            }
        }
        out
    }
}

/// `true` si `program` es un binario ejecutable localizable: ruta absoluta
/// existente, o un nombre presente en alguna entrada del `PATH`. Sondeo puro
/// (stat), sin ejecutar nada — la base de la degradación limpia («instala X»).
#[must_use]
pub fn program_available(program: &str) -> bool {
    let p = Path::new(program);
    if p.is_absolute() {
        return is_executable(p);
    }
    // Un nombre con separador pero relativo (`./tool`) se resuelve contra el
    // cwd; uno simple (`bat`) se busca en el PATH.
    if program.contains(std::path::MAIN_SEPARATOR) {
        return is_executable(p);
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(program)))
}

/// `true` si `p` existe y (en unix) tiene algún bit de ejecución.
#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// `true` si `p` existe como fichero (Windows no tiene bit de ejecución; la
/// ejecutabilidad la decide la extensión, fuera del alcance de este sondeo).
#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// Adivina el mimetype por EXTENSIÓN (heurística ligera, sin sniffing ni
/// lectura de contenido). Copia LOCAL del criterio de `norte-core` (el crate
/// de frontend no depende del core): sin extensión reconocible →
/// `application/octet-stream`. Opera sobre bytes crudos (regla 1): una
/// extensión no-UTF8 no casa nada.
#[must_use]
pub fn guess_mime(name: &[u8]) -> &'static str {
    let ext = std::str::from_utf8(name)
        .ok()
        .and_then(|n| n.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()));
    match ext.as_deref() {
        Some("txt" | "md" | "rs" | "toml" | "log" | "csv" | "ini" | "conf") => "text/plain",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("xml") => "text/xml",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

/// ¿El glob `pat` (`text/*` o exacto `application/pdf`) casa `mime`?
fn mimetype_matches(pat: &str, mime: &str) -> bool {
    match pat.strip_suffix("/*") {
        Some(prefix) => mime.split('/').next() == Some(prefix),
        None => pat == mime,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[cfg(unix)]
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    const SAMPLE: &str = r#"
[[opener]]
mime = "application/pdf"
os = "linux"
command = ["xdg-open", "%f"]

[[opener]]
mime = "text/*"
command = ["bat", "--paging=always", "%f"]

[[opener]]
mime = "text/plain"
os = "macos"
command = ["open", "-t", "%f"]
"#;

    #[test]
    fn parse_vacio_y_ausente() {
        assert!(OpenersConfig::empty().resolve("text/plain").is_none());
        assert!(OpenersConfig::parse("").unwrap().openers.is_empty());
    }

    #[test]
    fn parse_rechaza_command_vacio_y_claves_desconocidas() {
        let empty_cmd = "[[opener]]\nmime = \"text/*\"\ncommand = []\n";
        assert!(matches!(
            OpenersConfig::parse(empty_cmd),
            Err(OpenerError::EmptyCommand { .. })
        ));
        let unknown = "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\"]\nfoo = 1\n";
        assert!(matches!(
            OpenersConfig::parse(unknown),
            Err(OpenerError::Toml(_))
        ));
    }

    #[test]
    fn resolve_prefiere_os_especifico_luego_agnostico() {
        let cfg = OpenersConfig::parse(SAMPLE).unwrap();
        // En macOS gana la entrada os=macos sobre el text/* agnóstico.
        assert_eq!(
            cfg.resolve_for("text/plain", "macos").unwrap().program(),
            "open"
        );
        // En linux no hay text/plain específico: cae al text/* agnóstico.
        assert_eq!(
            cfg.resolve_for("text/plain", "linux").unwrap().program(),
            "bat"
        );
        // pdf sólo existe para linux: en windows no resuelve.
        assert_eq!(
            cfg.resolve_for("application/pdf", "linux")
                .unwrap()
                .program(),
            "xdg-open"
        );
        assert!(cfg.resolve_for("application/pdf", "windows").is_none());
    }

    #[test]
    fn glob_de_mimetype() {
        assert!(mimetype_matches("text/*", "text/html"));
        assert!(mimetype_matches("application/pdf", "application/pdf"));
        assert!(!mimetype_matches("text/*", "application/pdf"));
        assert!(!mimetype_matches("text/plain", "text/html"));
    }

    #[test]
    fn argv_sustituye_codigos_de_campo() {
        let cfg = OpenersConfig::parse(
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"-n\", \"%f\"]\n",
        )
        .unwrap();
        let o = cfg.resolve_for("text/plain", "linux").unwrap();
        let f = PathBuf::from("/home/u/a.txt");
        let argv = o.argv(&[&f], Path::new("/home/u"));
        assert_eq!(
            argv,
            vec![
                OsString::from("bat"),
                OsString::from("-n"),
                OsString::from("/home/u/a.txt")
            ]
        );
    }

    #[test]
    fn argv_multiple_y_dir() {
        let cfg = OpenersConfig::parse(
            "[[opener]]\nmime = \"text/*\"\ncommand = [\"ls\", \"%F\", \"%d\"]\n",
        )
        .unwrap();
        let o = cfg.resolve_for("text/x", "linux").unwrap();
        let (a, b) = (PathBuf::from("/x/a"), PathBuf::from("/x/b"));
        let argv = o.argv(&[&a, &b], Path::new("/x"));
        assert_eq!(
            argv,
            vec![
                OsString::from("ls"),
                OsString::from("/x/a"),
                OsString::from("/x/b"),
                OsString::from("/x"),
            ]
        );
    }

    /// Regla 1: una ruta con bytes no-UTF8 sobrevive byte a byte en el argv —
    /// `%f` es un token completo, jamás se concatena con un literal ni pasa
    /// por `String`.
    #[cfg(unix)]
    #[test]
    fn argv_preserva_bytes_no_utf8() {
        let cfg =
            OpenersConfig::parse("[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n")
                .unwrap();
        let o = cfg.resolve_for("text/x", "linux").unwrap();
        let hostil = PathBuf::from(OsString::from_vec(b"/x/\xff\xfe.txt".to_vec()));
        let argv = o.argv(&[&hostil], Path::new("/x"));
        assert_eq!(argv[1].as_bytes(), b"/x/\xff\xfe.txt");
    }

    #[test]
    fn field_code_con_input_vacio_se_omite() {
        let cfg =
            OpenersConfig::parse("[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n")
                .unwrap();
        let o = cfg.resolve_for("text/x", "linux").unwrap();
        // Sin ficheros: %f se omite, no queda un literal "%f".
        assert_eq!(o.argv(&[], Path::new("/x")), vec![OsString::from("bat")]);
    }

    #[test]
    fn guess_mime_por_extension() {
        assert_eq!(guess_mime(b"a.txt"), "text/plain");
        assert_eq!(guess_mime(b"a.PDF"), "application/pdf"); // case-insensitive
        assert_eq!(guess_mime(b"a.png"), "image/png");
        assert_eq!(guess_mime(b"sin_ext"), "application/octet-stream");
        // Extensión no-UTF8: no casa nada.
        assert_eq!(guess_mime(b"a.\xff\xfe"), "application/octet-stream");
    }

    #[test]
    fn program_available_encuentra_binarios_del_path() {
        // `sh` existe en cualquier unix de CI; un nombre inventado no.
        #[cfg(unix)]
        assert!(program_available("sh"));
        assert!(!program_available("norte-binario-que-no-existe-xyz"));
    }
}
