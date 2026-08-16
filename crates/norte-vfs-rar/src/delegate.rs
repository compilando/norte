//! Qué programa externo lee el RAR, cómo se encuentra y **cómo se le acota**.
//!
//! La regla 9 vive aquí: al delegado se le da una ruta, un nombre y una
//! tubería, jamás el sistema de ficheros del usuario. Cada endurecimiento de
//! [`Delegate::command`] carga peso, y ninguno es decorativo.

use futures::stream::StreamExt;
use norte_proto::Error;
use norte_vfs::ByteStream;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

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
    /// El ejecutable no arrancó (no existe, no es ejecutable, sin permisos).
    #[error("could not run the RAR reader `{program}`: {source}")]
    Spawn {
        /// Ruta del ejecutable que se intentó lanzar.
        program: String,
        /// El fallo del sistema operativo.
        source: std::io::Error,
    },
    /// El hijo pasó del plazo de pared y fue MUERTO. No es un `Io` con
    /// reintento: repetir lo mismo vuelve a colgarse.
    #[error("the RAR reader took longer than {}s and was killed", .0.as_secs())]
    Timeout(Duration),
    /// El nombre de la entrada, tratado como el patrón que el delegado
    /// aplicaría, alcanza a OTRA entrada del archivo.
    ///
    /// Se rehúsa en vez de adivinar: el flujo de la entrada equivocada tiene
    /// exactamente el mismo aspecto que el de la correcta.
    #[error("the entry name would match more than one entry as a pattern; refusing to guess")]
    AmbiguousForDelegate,
    /// El hijo terminó mal. `stderr` va recortado: es diagnóstico, no un canal.
    #[error("the RAR reader failed (exit {code}): {stderr}")]
    Failed {
        /// Código de salida, o `-1` si murió por señal.
        code: i32,
        /// Primeras líneas de `stderr`, en lossy — solo para el log.
        stderr: String,
    },
}

impl From<RarError> for Error {
    /// El cable no transporta prosa: todo esto colapsa a un puñado de
    /// categorías, y la frase se queda en el log de este lado.
    fn from(e: RarError) -> Self {
        match e {
            // Ninguna de las dos se arregla reintentando, y las dos tienen
            // una frase que el log sí lleva.
            RarError::NoDelegate | RarError::AmbiguousForDelegate => Self::Unsupported,
            RarError::Spawn { .. } | RarError::Timeout(_) => {
                Self::ProviderUnavailable { retryable: false }
            }
            // El delegado responde y dice que no: el contenedor es lo que
            // falla, no la I/O.
            RarError::Failed { .. } => Self::Corrupt,
        }
    }
}

/// Cuántos hijos pueden vivir a la vez en todo el proceso.
///
/// Un panel que lista un directorio con cuarenta `.rar` no puede convertirse
/// en cuarenta procesos: el semáforo es el que hace que la delegación tenga un
/// coste acotado.
const MAX_CHILDREN: usize = 4;

static CHILDREN: Semaphore = Semaphore::const_new(MAX_CHILDREN);

/// Plazo de pared por invocación de listado.
pub const LIST_TIMEOUT: Duration = Duration::from_secs(30);

/// Directorio de trabajo del hijo: uno VACÍO y propio del proceso.
///
/// Nunca el árbol del usuario. Un delegado que decida escribir rutas
/// relativas —`7z e` sin `-so`, una versión futura, un flag mal puesto—
/// escribe aquí, donde no hay nada que pisar. Si no se puede crear, el hijo
/// corre sin `current_dir` explícito antes que fallar la lectura entera; ese
/// caso ya solo puede pasar con el temporal del sistema roto.
fn sandbox_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("norte-rar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    })
    .as_deref()
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

    /// El delegado FIJADO por configuración (`[archive] rar_delegate`).
    ///
    /// El dialecto se decide por el nombre del ejecutable —`unrar` habla
    /// `vt`/`p`, cualquier otra cosa se trata como `7z`—, y un binario que no
    /// exista no falla aquí sino al usarlo, con un error que lo NOMBRA: fijar
    /// una ruta rota y no enterarse hasta abrir un `.rar` es peor que
    /// enterarse abriendo un `.rar`.
    ///
    /// ```
    /// use std::path::PathBuf;
    /// use norte_vfs_rar::Delegate;
    ///
    /// assert!(matches!(
    ///     Delegate::pinned(PathBuf::from("/opt/bin/unrar")),
    ///     Delegate::Unrar(_)
    /// ));
    /// ```
    #[must_use]
    pub fn pinned(program: PathBuf) -> Self {
        let name = program.file_name().unwrap_or_default().to_string_lossy();
        if name.contains("unrar") {
            Self::Unrar(program)
        } else {
            Self::SevenZip(program)
        }
    }

    /// La ruta absoluta del ejecutable elegido.
    #[must_use]
    pub fn program(&self) -> &Path {
        match self {
            Self::SevenZip(p) | Self::Unrar(p) => p,
        }
    }

    /// `argv` del LISTADO. Todo argumento va tras `--` y la contraseña va
    /// vacía en la propia línea de órdenes: una pregunta por `stdin` no puede
    /// ocurrir si nadie va a preguntar.
    #[must_use]
    pub fn list_argv(&self, archive: &Path) -> Vec<OsString> {
        let mut argv: Vec<OsString> = match self {
            Self::SevenZip(_) => ["l", "-slt", "-p", "-bd", "-y", "--"],
            Self::Unrar(_) => ["vt", "-p-", "-idc", "-y", "--", ""],
        }
        .iter()
        .filter(|a| !a.is_empty())
        .map(OsString::from)
        .collect();
        argv.push(archive.as_os_str().to_os_string());
        argv
    }

    /// `argv` de la LECTURA de UNA entrada a `stdout`.
    ///
    /// El nombre viaja en **bytes**, sin pasar por `String`: un nombre que no
    /// es UTF-8 es un nombre igualmente (regla 1). Que el delegado trate ese
    /// nombre como un patrón es problema del provider, que rehúsa antes de
    /// llegar aquí.
    #[must_use]
    pub fn read_argv(&self, archive: &Path, entry: &[u8]) -> Vec<OsString> {
        let mut argv: Vec<OsString> = match self {
            Self::SevenZip(_) => ["e", "-so", "-bd", "-y", "-p", "--"],
            Self::Unrar(_) => ["p", "-inul", "-p-", "-y", "--", ""],
        }
        .iter()
        .filter(|a| !a.is_empty())
        .map(OsString::from)
        .collect();
        argv.push(archive.as_os_str().to_os_string());
        argv.push(os_from_bytes(entry));
        argv
    }

    /// Construye el proceso hijo con la regla 9 puesta. Cada línea carga peso:
    ///
    /// - `stdin` a `null`: una pregunta de contraseña no puede colgar el
    ///   daemon, porque no hay nadie a quien preguntar;
    /// - `stderr` capturado: los mensajes del delegado no contaminan el flujo
    ///   de datos ni el log del proceso;
    /// - `current_dir` en un directorio vacío: nunca el árbol del usuario;
    /// - `env_clear`: el hijo no hereda ni credenciales ni `LD_PRELOAD`;
    /// - `kill_on_drop`: soltar el futuro mata al hijo, que es lo que hace
    ///   que cancelar signifique algo.
    fn command(&self, argv: &[OsString]) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(self.program());
        cmd.args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .kill_on_drop(true);
        if let Some(dir) = sandbox_dir() {
            cmd.current_dir(dir);
        }
        cmd
    }

    /// Lanza el hijo, espera su salida COMPLETA y la devuelve en bytes.
    ///
    /// Para el listado, que es pequeño y hay que parsear entero. Pasado
    /// `timeout` el hijo muere y el error lo dice.
    ///
    /// # Errors
    ///
    /// [`RarError::Spawn`] si el ejecutable no arranca, [`RarError::Timeout`]
    /// si agota el plazo, [`RarError::Failed`] si termina con estado no cero.
    ///
    /// # Panics
    ///
    /// Si el semáforo de hijos se cerrase, cosa que este crate nunca hace.
    pub async fn run_capture(
        &self,
        argv: &[OsString],
        timeout: Duration,
    ) -> Result<Vec<u8>, RarError> {
        let _permit = CHILDREN.acquire().await.expect("el semáforo no se cierra");
        let child = self
            .command(argv)
            .spawn()
            .map_err(|source| RarError::Spawn {
                program: self.program().display().to_string(),
                source,
            })?;
        // El hijo vive DENTRO del futuro: si el timeout lo suelta, `kill_on_drop`
        // lo mata. No hay camino en el que quede un proceso huérfano.
        let out = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| RarError::Timeout(timeout))?
            .map_err(|source| RarError::Spawn {
                program: self.program().display().to_string(),
                source,
            })?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(RarError::Failed {
                code: out.status.code().unwrap_or(-1),
                stderr: first_lines(&out.stderr),
            })
        }
    }

    /// Lanza el hijo y devuelve su `stdout` como flujo, sin acumularlo.
    ///
    /// Cancelar el token mata al hijo (regla 3): el flujo termina en
    /// [`Error::Cancelled`] y el permiso del semáforo se libera.
    ///
    /// # Errors
    ///
    /// [`RarError::Spawn`] si el ejecutable no arranca.
    ///
    /// # Panics
    ///
    /// Si el semáforo de hijos se cerrase, cosa que este crate nunca hace, o
    /// si `stdout` no viniese como tubería habiéndolo pedido así.
    pub async fn run_stream(
        &self,
        argv: &[OsString],
        cancel: CancellationToken,
    ) -> Result<ByteStream, RarError> {
        // El permiso se OLVIDA aquí y se devuelve a mano cuando la task de
        // abajo termina: el flujo sobrevive a esta función, así que no puede
        // atarse a un guard con el ámbito de ella.
        CHILDREN
            .acquire()
            .await
            .expect("el semáforo no se cierra")
            .forget();
        let mut child = self.command(argv).spawn().map_err(|source| {
            CHILDREN.add_permits(1);
            RarError::Spawn {
                program: self.program().display().to_string(),
                source,
            }
        })?;
        let mut stdout = child.stdout.take().expect("stdout pedido como pipe");
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, Error>>(4);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let read = tokio::select! {
                    () = cancel.cancelled() => {
                        let _ = tx.send(Err(Error::Cancelled)).await;
                        break;
                    }
                    r = stdout.read(&mut buf) => r,
                };
                match read {
                    Ok(0) => {
                        // EOF: el veredicto lo da el estado de salida, no el
                        // silencio. Un `.rar` cifrado da cero bytes y error.
                        match child.wait().await {
                            Ok(st) if st.success() => {}
                            Ok(_) | Err(_) => {
                                let _ = tx.send(Err(Error::Corrupt)).await;
                            }
                        }
                        break;
                    }
                    Ok(n) => {
                        if tx
                            .send(Ok(bytes::Bytes::copy_from_slice(&buf[..n])))
                            .await
                            .is_err()
                        {
                            break; // el consumidor se fue: `kill_on_drop` remata
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(Err(Error::Io { retryable: false })).await;
                        break;
                    }
                }
            }
            drop(child); // kill_on_drop: ni cancelado ni roto deja proceso vivo
            CHILDREN.add_permits(1);
        });
        Ok(tokio_stream::wrappers::ReceiverStream::new(rx).boxed())
    }
}

/// Un nombre en bytes crudos a `OsString`, sin pasar por `String`.
#[cfg(unix)]
fn os_from_bytes(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    OsStr::from_bytes(bytes).to_os_string()
}

/// En Windows el `argv` es UTF-16 y no hay forma de pasar bytes arbitrarios:
/// la conversión lossy es del sistema operativo, no una decisión nuestra.
#[cfg(not(unix))]
fn os_from_bytes(bytes: &[u8]) -> OsString {
    OsString::from(String::from_utf8_lossy(bytes).into_owned())
}

/// Las primeras líneas de `stderr` en lossy, acotadas: es diagnóstico para el
/// log, no un canal de datos.
fn first_lines(stderr: &[u8]) -> String {
    let cut = stderr.len().min(512);
    String::from_utf8_lossy(&stderr[..cut])
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_de_listado_lleva_separador_y_sin_password() {
        let d = Delegate::SevenZip(PathBuf::from("/usr/bin/7z"));
        let argv = d.list_argv(Path::new("/tmp/a.rar"));
        assert!(
            argv.contains(&OsString::from("--")),
            "todo argumento va tras `--`"
        );
        assert!(
            argv.iter().any(|a| a == "-p"),
            "password vacía: jamás una pregunta por stdin"
        );
        assert_eq!(argv.last().unwrap(), "/tmp/a.rar");
    }

    #[test]
    fn argv_de_listado_de_unrar_tambien_calla_la_password() {
        let d = Delegate::Unrar(PathBuf::from("/usr/bin/unrar"));
        let argv = d.list_argv(Path::new("/tmp/a.rar"));
        assert!(argv.iter().any(|a| a == "-p-"), "unrar: password vacía");
        let sep = argv.iter().position(|a| a == "--").expect("hay separador");
        assert_eq!(sep, argv.len() - 2, "el archivo va DESPUÉS del separador");
    }

    #[test]
    fn argv_de_lectura_pasa_el_nombre_en_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let d = Delegate::SevenZip(PathBuf::from("/usr/bin/7z"));
        let argv = d.read_argv(Path::new("/tmp/a.rar"), b"cp437-\xa4\xa5.txt");
        assert_eq!(argv.last().unwrap().as_bytes(), b"cp437-\xa4\xa5.txt");
        assert!(
            argv.iter().any(|a| a == "-so"),
            "el contenido sale por stdout"
        );
    }

    /// La propiedad es «no se cuelga». Con `stdin` ABIERTO este test tarda
    /// para siempre; con `stdin` a null, el hijo muere solo.
    #[tokio::test]
    async fn el_hijo_nunca_espera_en_stdin() {
        let d = Delegate::Unrar(PathBuf::from("/bin/cat")); // cat lee stdin hasta EOF
        let out = tokio::time::timeout(
            Duration::from_secs(5),
            d.run_capture(&[], Duration::from_secs(30)),
        )
        .await;
        assert!(out.is_ok(), "stdin abierto: el hijo se quedó esperando");
    }

    /// Un hijo que no termina se MATA, y el error lo dice en vez de callar.
    #[tokio::test]
    async fn el_plazo_de_pared_mata_al_hijo() {
        let d = Delegate::Unrar(PathBuf::from("/bin/sleep"));
        let err = d
            .run_capture(&[OsString::from("30")], Duration::from_millis(200))
            .await
            .expect_err("30s no caben en 200ms");
        assert!(matches!(err, RarError::Timeout(_)), "{err}");
        assert_eq!(
            Error::from(err),
            Error::ProviderUnavailable { retryable: false }
        );
    }

    #[tokio::test]
    async fn cancelar_mata_al_hijo() {
        let token = CancellationToken::new();
        let d = Delegate::Unrar(PathBuf::from("/bin/sleep"));
        let stream = d
            .run_stream(&[OsString::from("30")], token.clone())
            .await
            .expect("sleep arranca");
        token.cancel();
        let items = tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
            .await
            .expect("el hijo sobrevivió a la cancelación");
        assert_eq!(
            items.last().and_then(|r| r.as_ref().err().cloned()),
            Some(Error::Cancelled),
            "el flujo termina DICIENDO que se canceló"
        );
    }

    /// Un ejecutable que no existe no es un panic ni un listado vacío: es un
    /// error que NOMBRA el programa.
    #[tokio::test]
    async fn un_ejecutable_ausente_nombra_el_programa() {
        let d = Delegate::SevenZip(PathBuf::from("/nonexistent/7z"));
        let err = d
            .run_capture(&[], Duration::from_secs(5))
            .await
            .expect_err("no existe");
        assert!(err.to_string().contains("/nonexistent/7z"), "{err}");
        assert!(matches!(err, RarError::Spawn { .. }));
    }

    /// Un hijo que sale con estado no cero es `Corrupt`: el delegado responde
    /// y dice que ese contenedor no vale.
    #[tokio::test]
    async fn un_estado_no_cero_es_corrupt() {
        let d = Delegate::SevenZip(PathBuf::from("/bin/false"));
        let err = d
            .run_capture(&[], Duration::from_secs(5))
            .await
            .expect_err("false siempre falla");
        assert!(matches!(err, RarError::Failed { .. }), "{err}");
        assert_eq!(Error::from(err), Error::Corrupt);
    }

    /// El hijo NO hereda el entorno: nada de credenciales en variables, nada
    /// de `LD_PRELOAD`. `env` imprime lo que tenga, y no debe tener nada —
    /// `PATH` está puesto en cualquier entorno de test.
    #[tokio::test]
    async fn el_hijo_no_hereda_el_entorno() {
        assert!(
            std::env::var_os("PATH").is_some(),
            "el padre SÍ tiene entorno"
        );
        let d = Delegate::SevenZip(PathBuf::from("/usr/bin/env"));
        let out = d
            .run_capture(&[], Duration::from_secs(5))
            .await
            .expect("env arranca");
        let text = String::from_utf8_lossy(&out);
        assert!(text.trim().is_empty(), "entorno heredado: {text}");
    }

    /// El hijo corre en un directorio VACÍO y propio, jamás en el árbol del
    /// usuario: `pwd` lo dice.
    #[tokio::test]
    async fn el_hijo_corre_fuera_del_arbol_del_usuario() {
        let d = Delegate::SevenZip(PathBuf::from("/bin/pwd"));
        let out = d
            .run_capture(&[], Duration::from_secs(5))
            .await
            .expect("pwd arranca");
        let cwd = String::from_utf8_lossy(&out).trim().to_string();
        assert_eq!(
            Some(std::path::Path::new(&cwd)),
            sandbox_dir(),
            "el hijo no corre donde está el usuario"
        );
        assert_eq!(
            std::fs::read_dir(&cwd).unwrap().count(),
            0,
            "y el directorio está vacío"
        );
    }

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
    fn un_delegado_fijado_elige_dialecto_por_su_nombre() {
        assert_eq!(
            Delegate::pinned(PathBuf::from("/usr/local/bin/7zz")),
            Delegate::SevenZip(PathBuf::from("/usr/local/bin/7zz"))
        );
        assert_eq!(
            Delegate::pinned(PathBuf::from("/opt/unrar")),
            Delegate::Unrar(PathBuf::from("/opt/unrar"))
        );
        // Un nombre que no dice nada se trata como 7z: es el dialecto que
        // conserva los bytes crudos, o sea el que menos pierde si acertamos
        // a medias.
        assert_eq!(
            Delegate::pinned(PathBuf::from("/opt/lector")),
            Delegate::SevenZip(PathBuf::from("/opt/lector"))
        );
    }

    #[test]
    fn solo_unrar_se_acepta() {
        let found = Delegate::discover_in(&[("unrar", PathBuf::from("/usr/bin/unrar"))])
            .expect("unrar sirve");
        assert_eq!(found, Delegate::Unrar(PathBuf::from("/usr/bin/unrar")));
    }
}
