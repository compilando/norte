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
    /// ¿Abre VENTANA propia? Ausente = `false`.
    ///
    /// Un programa de terminal (`bat`, `vim`) necesita que el frontend se
    /// aparte y lo espere; uno gráfico (`zed`, `loupe`) devuelve el control al
    /// instante, y suspender la TUI por él deja al lector mirando un terminal
    /// en blanco hasta que cierre una ventana que está en otro sitio. Norte no
    /// puede adivinar cuál es cuál: lo dice quien escribe la regla.
    #[serde(default)]
    detached: Option<bool>,
    /// argv plantilla: `["bat", "--paging=always", "%f"]`. El primer token es
    /// el binario; los códigos de campo `%f`/`%F`/`%d` se sustituyen SOLO como
    /// tokens completos (nunca dentro de un literal — así una ruta no-UTF8
    /// jamás se concatena con texto y se preserva byte a byte, regla 1). Un
    /// código de campo EMBEBIDO en un literal (`--file=%f`) NO se expande: se
    /// pasa tal cual como argumento literal (usa un token propio en su lugar).
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

    /// Antepone los openers de `higher` (capa de MAYOR precedencia): en un
    /// empate de mimetype+OS, la capa superior gana ([`Self::resolve`] casa la
    /// primera entrada). Lo usa el frontend al fusionar capas (usuario sobre
    /// sistema); la capa de PROYECTO se excluye antes de llamar aquí — un repo
    /// hostil no debe inyectar comandos externos que se ejecuten.
    pub fn extend_front(&mut self, higher: OpenersConfig) {
        let mut merged = higher.openers;
        merged.append(&mut self.openers);
        self.openers = merged;
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
    ///
    /// ```
    /// use norte_frontend::openers::OpenersConfig;
    /// let cfg = OpenersConfig::parse(
    ///     "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
    /// )
    /// .unwrap();
    /// assert_eq!(cfg.resolve_for("text/html", "linux").unwrap().program(), "bat");
    /// assert!(cfg.resolve_for("application/pdf", "linux").is_none());
    /// ```
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
    ///
    /// Seguridad: el caller pasa rutas ABSOLUTAS (`vpath_to_native`), que
    /// empiezan por `/` (o `\\?\…` en Windows). Eso impide que un nombre hostil
    /// tipo `-rf` o `--config=…` se cuele como FLAG del programa destino: cada
    /// código de campo es un solo elemento del argv Y nunca empieza por `-`.
    #[must_use]
    pub fn argv(&self, files: &[&Path], dir: &Path) -> Vec<std::ffi::OsString> {
        expand_argv(&self.command, files, dir)
    }

    /// ¿Abre ventana propia y por tanto NO se le espera? (`detached`).
    #[must_use]
    pub fn detached(&self) -> bool {
        self.detached.unwrap_or(false)
    }
}

/// Sustituye los códigos de campo de una plantilla de argv. `%f` → la PRIMERA
/// ruta; `%F` → TODAS (un arg por ruta); `%d` → el directorio.
///
/// Suelta y pública porque `openers.toml` no es el único sitio donde el
/// usuario escribe una plantilla: `[ui] editor` usa la misma gramática, y
/// tener dos expansores sería tener dos reglas de citado sobre rutas que son
/// BYTES (regla 1). Ver [`Opener::argv`] para el contrato completo, incluida
/// la razón por la que un código de campo es siempre un elemento entero del
/// argv y nunca se concatena con texto.
#[must_use]
pub fn expand_argv(command: &[String], files: &[&Path], dir: &Path) -> Vec<std::ffi::OsString> {
    let mut out = Vec::with_capacity(command.len());
    let Some((programa, resto)) = command.split_first() else {
        return out;
    };
    // El primer token (binario) NUNCA se interpola.
    out.push(programa.as_str().into());
    for tok in resto {
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

/// `true` si `program` es un binario ejecutable localizable: ruta absoluta
/// existente, o un nombre presente en alguna entrada del `PATH`. Sondeo puro
/// (stat), sin ejecutar nada — la base de la degradación limpia («instala X»).
#[must_use]
pub fn program_available(program: &str) -> bool {
    resolve_program(std::ffi::OsStr::new(program)).is_some()
}

/// La ruta ABSOLUTA del binario que `program` nombra, o `None` si no se
/// encuentra. Mismo sondeo que [`program_available`] —del que es ahora la
/// implementación— pero devolviendo QUÉ se encontró.
///
/// La diferencia importa cuando el hijo se lanza con `Command::current_dir`
/// puesto (S4, `app.terminal`): en unix, `current_dir` se aplica ANTES de
/// resolver el programa, así que un nombre relativo lo resuelve `execvp`
/// contra el directorio que el usuario está NAVEGANDO, no contra el de
/// norte. Con un `.` (o un componente vacío) en el `PATH`, un fichero
/// llamado `kitty` dentro de un archivo recién extraído se ejecutaría como
/// el usuario — y la sonda no lo vería, porque ella corre con el cwd de
/// norte: sonda y lanzamiento estarían mirando directorios distintos por
/// construcción. Lanzar la ruta absoluta que devolvió la sonda es lo que
/// hace que los dos coincidan.
///
/// Un `program` que YA es absoluto se devuelve tal cual si existe. Uno
/// relativo con separador (`./tool`) se resuelve contra el cwd actual y se
/// canonicaliza a absoluto, por el mismo motivo.
///
/// Toma `OsStr` y no `&str` porque desde #302 también resuelve el editor, que
/// sale de `$VISUAL`/`$EDITOR`: una variable de entorno es BYTES y un editor
/// puede vivir bajo una ruta que no es UTF-8 como cualquier otra cosa
/// (regla 1).
#[must_use]
pub fn resolve_program(program: &std::ffi::OsStr) -> Option<std::path::PathBuf> {
    resolve_program_in(program, std::env::var_os("PATH").as_deref())
}

/// El núcleo probable de [`resolve_program`]: el `PATH` entra como ARGUMENTO.
///
/// Misma disciplina que los `*_from` de [`crate::shell`]: un test que tocara
/// la variable del proceso competiría con todos los demás del mismo binario,
/// y `std::env::set_var` es `unsafe` desde Rust 2024 (regla 5). Lo que este
/// núcleo NO abstrae es el disco: el sondeo es un `stat` de verdad, así que
/// sus tests montan un directorio temporal en vez de fingir uno.
#[must_use]
pub fn resolve_program_in(
    program: &std::ffi::OsStr,
    path_var: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    let p = Path::new(program);
    if p.is_absolute() {
        return is_executable(p).then(|| p.to_path_buf());
    }
    // Un nombre con separador pero relativo (`./tool`) se resuelve contra el
    // cwd; uno simple (`bat`) se busca en el PATH. «Con separador» se pregunta
    // por el PADRE y no por el byte del separador: así vale igual en Windows,
    // donde los separadores son dos.
    if p.parent().is_some_and(|d| !d.as_os_str().is_empty()) {
        if !is_executable(p) {
            return None;
        }
        // Absoluto ANTES de que nadie cambie el cwd del hijo.
        return std::env::current_dir().ok().map(|c| c.join(p));
    }
    let path = path_var?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        // Una entrada VACÍA del `PATH` significa «el directorio actual», que
        // para un hijo con `current_dir` puesto es el directorio navegado:
        // jamás se resuelve contra él, ni siquiera si existe.
        .filter(|c| c.is_absolute())
        .find(|c| is_executable(c))
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
/// `application/octet-stream`.
///
/// Opera sobre bytes crudos (regla 1): se parte por el ÚLTIMO `.` a nivel de
/// bytes y solo la EXTENSIÓN se valida como UTF-8 — un nombre con stem no-UTF8
/// pero extensión ASCII (`caf\xe9\xff.txt`) sí detecta `text/plain`. Una
/// extensión no-UTF8 no casa nada.
///
/// ```
/// use norte_frontend::openers::guess_mime;
/// assert_eq!(guess_mime(b"notes.md"), "text/plain");
/// assert_eq!(guess_mime(b"sin_extension"), "application/octet-stream");
/// // stem no-UTF8 + extensión ASCII: la extensión manda.
/// assert_eq!(guess_mime(b"caf\xe9\xff.pdf"), "application/pdf");
/// ```
#[must_use]
pub fn guess_mime(name: &[u8]) -> &'static str {
    let ext = name
        .iter()
        .rposition(|&b| b == b'.')
        .and_then(|dot| std::str::from_utf8(&name[dot + 1..]).ok())
        .map(str::to_ascii_lowercase);
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

/// El lanzador del PROPIO escritorio: el programa que el sistema tiene
/// asociado al fichero. Es el último recurso de `pane.open` cuando `ns.toml`
/// no declara ningún opener para ese mimetype — sin esto, un usuario que no
/// ha escrito configuración no puede abrir nada.
///
/// Devuelve `(programa, argv)` listos para `spawn`; el binario se sondea
/// aparte con [`program_available`] (un Linux sin `xdg-utils` instalado es
/// un caso real, no teórico).
///
/// El argv se construye byte a byte desde la ruta NATIVA, nunca desde una
/// conversión a texto (regla 1): un nombre no-UTF8 llega intacto al
/// programa asociado.
///
/// Por plataforma:
/// - **Linux y demás unix**: `xdg-open`, el estándar de freedesktop.
/// - **macOS**: `open`, que viene en el sistema base.
/// - **Windows**: `explorer.exe`, NO `cmd /C start`. La diferencia importa:
///   `cmd` re-interpreta su línea de comandos, así que un nombre de fichero
///   con `&` o `^` puede ejecutar lo que no debe; `explorer.exe` recibe el
///   argumento tal cual. (`explorer` devuelve código de salida 1 incluso
///   cuando abre bien — por eso este camino no interpreta el estado.)
///
/// ```
/// # use std::path::Path;
/// let (program, argv) = norte_frontend::openers::system_opener(Path::new("/tmp/a.pdf"));
/// assert!(!program.is_empty());
/// assert_eq!(argv.last().map(std::ffi::OsString::as_os_str), Some(Path::new("/tmp/a.pdf").as_os_str()));
/// ```
#[must_use]
pub fn system_opener(file: &Path) -> (String, Vec<std::ffi::OsString>) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer.exe"
    } else {
        "xdg-open"
    };
    (
        program.to_owned(),
        vec![
            std::ffi::OsString::from(program),
            file.as_os_str().to_os_string(),
        ],
    )
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
        // Stem no-UTF8 pero extensión ASCII: la extensión manda (byte-split).
        assert_eq!(guess_mime(b"caf\xe9\xff.txt"), "text/plain");
        assert_eq!(guess_mime(b"\xff\xff.png"), "image/png");
    }

    /// El lanzador del sistema pasa la ruta como UN argumento propio y
    /// byte-exacto: un nombre no-UTF8 o con metacaracteres de shell llega
    /// intacto y jamás se re-interpreta (por eso Windows usa `explorer.exe`
    /// y no `cmd /C start`).
    #[test]
    fn system_opener_pasa_la_ruta_como_argumento_byte_exacto() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let hostil =
                std::path::PathBuf::from(OsStr::from_bytes(b"/tmp/a & b; rm -rf \xff\xfe.pdf"));
            let (program, argv) = system_opener(&hostil);
            assert_eq!(argv.len(), 2, "binario + fichero, sin shell de por medio");
            assert_eq!(argv[0], OsString::from(&program));
            assert_eq!(
                argv[1].as_os_str().as_bytes(),
                b"/tmp/a & b; rm -rf \xff\xfe.pdf",
                "la ruta viaja byte a byte"
            );
        }
        let (program, argv) = system_opener(Path::new("/tmp/x.pdf"));
        assert!(!program.is_empty());
        assert_eq!(argv[1], OsString::from("/tmp/x.pdf"));
        // El binario del plato de cada plataforma, no uno inventado.
        let esperado = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "explorer.exe"
        } else {
            "xdg-open"
        };
        assert_eq!(program, esperado);
    }

    #[test]
    fn program_available_encuentra_binarios_del_path() {
        // `sh` existe en cualquier unix de CI; un nombre inventado no.
        #[cfg(unix)]
        assert!(program_available("sh"));
        assert!(!program_available("norte-binario-que-no-existe-xyz"));
    }

    /// Una entrada RELATIVA del `PATH` —`.`, o la vacía que significa lo
    /// mismo— no resuelve NADA (#302).
    ///
    /// Es la condición del agujero entera: el hijo se lanza con
    /// `Command::current_dir` puesto en el directorio que el lector navega y,
    /// en unix, `current_dir` se aplica ANTES de resolver el programa. Con un
    /// `.` en el `PATH`, un fichero llamado `vim` dentro de un archivo recién
    /// extraído se ejecutaría al pulsar F4. Aquí el sondeo mira desde el cwd
    /// del test, donde el ejecutable SÍ está, y aun así dice que no.
    #[cfg(unix)]
    #[test]
    fn una_entrada_relativa_del_path_no_resuelve_nada() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let malo = dir.path().join("editor-hostil");
        std::fs::write(&malo, b"#!/bin/sh\n").expect("escribe");
        std::fs::set_permissions(&malo, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let nombre = OsStr::new("editor-hostil");
        // Absoluta: se encuentra, que es lo que hace honesto al caso de abajo.
        assert_eq!(
            resolve_program_in(nombre, Some(dir.path().as_os_str())).as_deref(),
            Some(malo.as_path())
        );
        // Relativa y vacía: ni una ni otra, aunque el fichero esté ahí.
        let relativo: std::path::PathBuf = dir
            .path()
            .file_name()
            .map(|n| std::path::Path::new("..").join(n))
            .expect("tiene nombre");
        for path_var in [OsStr::new("."), OsStr::new(""), relativo.as_os_str()] {
            assert_eq!(
                resolve_program_in(nombre, Some(path_var)),
                None,
                "una entrada relativa del PATH no puede resolver un programa"
            );
        }
        // Y sin `PATH` no hay dónde buscar.
        assert_eq!(resolve_program_in(nombre, None), None);
    }

    /// El programa se toma como BYTES: un editor bajo una ruta que no es
    /// UTF-8 se resuelve igual (regla 1).
    #[cfg(unix)]
    #[test]
    fn un_programa_con_nombre_no_utf8_se_resuelve() {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        let nombre = OsStr::from_bytes(b"ed\xffitor");
        let ruta = dir.path().join(nombre);
        std::fs::write(&ruta, b"#!/bin/sh\n").expect("escribe");
        std::fs::set_permissions(&ruta, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        assert_eq!(
            resolve_program_in(nombre, Some(dir.path().as_os_str())).as_deref(),
            Some(ruta.as_path())
        );
    }
}
