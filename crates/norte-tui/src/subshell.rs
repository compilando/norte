//! El SUBSHELL persistente: el pty y el hijo de larga vida (#142).
//!
//! La otra mitad —el marcador del prompt, el parseo del cwd y el `cd` en
//! bytes— vive en [`norte_frontend::subshell`], sin I/O y con sus tests.
//!
//! # Qué cambia respecto a lo que había
//!
//! `app.toggle-panels` cedía la terminal y enseñaba el SCROLLBACK de donde
//! arrancó norte, hasta que se pulsara una tecla. Eso no es un shell: no
//! recuerda nada, no se le puede escribir, y el directorio del panel le da
//! igual. Lo que hace Midnight Commander —y lo que se nota a la primera— es
//! tener un shell VIVO detrás de los paneles.
//!
//! # Las cuatro decisiones que esto encierra
//!
//! **Se arranca PEREZOSO**, en el primer `Ctrl+O`. Un shell por sesión de
//! norte que nadie va a usar es un proceso, un pty y el `.bashrc` de alguien
//! ejecutándose por si acaso.
//!
//! **El hijo hereda el pty y NO la terminal de norte.** Por eso puede seguir
//! vivo mientras los paneles se pintan: nadie comparte el tty. Mientras está
//! adjunto, este módulo copia bytes en las dos direcciones.
//!
//! **El cwd lo DICE el shell**, con un marcador que norte le mete en el prompt
//! al arrancarlo (nunca tocando su configuración). Al adjuntar, el shell sigue
//! al panel con un `cd`; al soltarlo, el panel puede seguir al shell.
//!
//! **Al salir norte, el shell se va con él** (`SIGHUP` por el drop de
//! `portable-pty`): dejar un shell huérfano hablándole a un pty que ya no lee
//! nadie es un proceso que nadie sabe que existe.

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};

use norte_frontend::subshell::{Nonce, install, scan_cwd};

/// El subshell vivo de esta sesión.
pub struct Subshell {
    /// Por dónde se le escribe.
    ///
    /// COMPARTIDA con el hilo lector, que también escribe: es quien contesta
    /// las consultas de terminal del shell ([`Escritor`]).
    escritura: Escritor,
    /// El pty, que además es quien redimensiona.
    maestro: Box<dyn portable_pty::MasterPty + Send>,
    /// El hijo. Se conserva para poder matarlo y para saber si sigue vivo.
    hijo: Box<dyn portable_pty::Child + Send + Sync>,
    /// Lo que el shell ha escrito y todavía no se ha pintado, más el ÚLTIMO
    /// cwd que anunció. Lo llena un hilo lector.
    buzon: Arc<Mutex<Buzon>>,
    /// Cuál de los tres es, si es uno de los tres. `None` = norte no le
    /// instaló nada y no le teclea nada.
    cual: Option<norte_frontend::shell::Shell>,
    /// El BUZÓN de esta sesión: por dónde se le dice al shell que cambie de
    /// directorio, en vez de teclearle un `cd` (#363).
    ///
    /// Un fichero 0600 con un nonce en el nombre, bajo el directorio de
    /// ejecución del usuario (`$XDG_RUNTIME_DIR`, que ya es 0700 suyo) o bajo
    /// `/tmp/norte-<uid>` si no lo hay — el mismo sitio y el mismo criterio
    /// que el socket del daemon. `None` = no se pudo crear, y entonces el
    /// panel simplemente no arrastra al shell: degradar así es correcto, y
    /// caerse o volver a teclear el `cd` no lo serían.
    ///
    /// Se borra al soltar el subshell. Si norte muere de golpe queda un
    /// fichero de unas decenas de bytes en un directorio que el sistema
    /// limpia al cerrar sesión.
    buzon_fichero: Option<std::path::PathBuf>,
}

/// La entrada del pty, compartida entre quien adjunta y el hilo lector.
///
/// Dos escritores, y los dos legítimos: las teclas del lector entran por
/// [`Subshell::escribir`], y las RESPUESTAS a las consultas de terminal del
/// shell las manda el hilo que las ve pasar. Un shell moderno pregunta qué
/// terminal tiene delante y se PARA hasta que le contestan (fish 4 lo hace
/// antes de su primer prompt), así que la respuesta no puede esperar a que
/// alguien adjunte.
type Escritor = Arc<Mutex<Box<dyn std::io::Write + Send>>>;

/// Lo que el hilo lector deja para quien adjunte.
#[derive(Default)]
struct Buzon {
    /// Bytes pendientes de pintar, ya SIN los marcadores.
    pendiente: Vec<u8>,
    /// El último cwd anunciado, en bytes (regla 1).
    cwd: Option<Vec<u8>>,
    /// Un marcador partido entre dos lecturas, esperando su final.
    cola: Vec<u8>,
    /// El pty se cerró: el shell se fue.
    cerrado: bool,
}

/// Cuánto se guarda de lo que el shell escribió mientras nadie mira.
///
/// Un `find /` lanzado y dejado corriendo escribe sin fin, y esto vive en
/// memoria: se conserva la COLA, que es lo que un lector querría ver al
/// volver, y lo de más atrás se tira.
const BUFFER_MAX: usize = 256 * 1024;

impl Subshell {
    /// Arranca un shell en su propio pty.
    ///
    /// `dir` es dónde empieza; `size` el tamaño de la terminal, que el hijo
    /// necesita saber para pintar.
    ///
    /// # Errors
    /// Lo que falle al abrir el pty o al lanzar el shell.
    pub fn arrancar(dir: &std::path::Path, size: (u16, u16)) -> std::io::Result<Self> {
        Self::arrancar_con(&norte_frontend::shell::login_shell(), &[], dir, size)
    }

    /// [`Self::arrancar`] con el programa y los argumentos DADOS.
    ///
    /// Existe por los tests, y no es una comodidad: `arrancar` usa `$SHELL`, o
    /// sea el shell de quien corre la suite, con su configuración entera
    /// detrás. En esta máquina eso es un zsh cuyo primer arranque interactivo
    /// lanza el asistente de powerlevel10k y jamás llega a un prompt — un test
    /// rojo que no dice nada del código. Un test que necesita un shell
    /// necesita UN shell, no el de quien lo corre.
    fn arrancar_con(
        programa: &std::path::Path,
        args: &[&str],
        dir: &std::path::Path,
        size: (u16, u16),
    ) -> std::io::Result<Self> {
        let sistema = portable_pty::native_pty_system();
        let par = sistema
            .openpty(portable_pty::PtySize {
                rows: size.1,
                cols: size.0,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(std::io::Error::other)?;
        let shell = programa.to_path_buf();
        let mut cmd = portable_pty::CommandBuilder::new(&shell);
        for a in args {
            cmd.arg(a);
        }
        cmd.cwd(dir);
        // El hijo sabe que está DENTRO de norte, como el de una suspensión: es
        // el mismo contrato de `NORTE_LEVEL` y lo lee el prompt del lector.
        cmd.env(
            norte_frontend::shell::LEVEL_VAR,
            norte_frontend::shell::next_norte_level(),
        );
        let hijo = par
            .slave
            .spawn_command(cmd)
            .map_err(std::io::Error::other)?;
        // El esclavo se SUELTA aquí: mientras norte lo tenga abierto, cerrar
        // el shell no cierra el pty y el lector nunca vería EOF.
        drop(par.slave);
        let escritura: Escritor = Arc::new(Mutex::new(
            par.master.take_writer().map_err(std::io::Error::other)?,
        ));
        let lector = par
            .master
            .try_clone_reader()
            .map_err(std::io::Error::other)?;
        let buzon = Arc::new(Mutex::new(Buzon::default()));
        // El nonce de ESTA sesión: lo que separa un marcador que imprimió el
        // gancho de uno que venía dentro de un fichero. Ver `Nonce`.
        let nonce = Nonce::new();
        lanzar_lector(
            lector,
            Arc::clone(&buzon),
            Arc::clone(&escritura),
            nonce.clone(),
        );

        let cual = shell_conocido(&shell);
        // El buzón se crea ANTES del gancho: el gancho lleva su ruta dentro.
        let buzon_fichero = cual.and_then(|_| crear_buzon(&nonce));
        let mut yo = Self {
            escritura,
            maestro: par.master,
            hijo,
            buzon,
            cual,
            buzon_fichero,
        };
        // El gancho del prompt se manda como si el lector lo tecleara: NO se
        // toca ningún fichero suyo. Un `.bashrc` que norte editara sería una
        // modificación permanente por una función que se apaga al salir — y
        // sobreviviría a un norte que ya no está.
        //
        // Sin buzón no se instala nada: el gancho lleva su ruta dentro, y uno
        // apuntando a un fichero que no existe sería fontanería tecleada en la
        // cara del lector a cambio de nada.
        if let (Some(cual), Some(buzon)) = (cual, yo.buzon_fichero.clone()) {
            let _ = yo.escribir(install(cual, &nonce, &buzon).as_bytes());
            // Ctrl+L: la orden de `readline`/ZLE/fish que limpia la pantalla y
            // repinta el prompt. Sin esto, lo primero que el lector ve en su
            // primer Ctrl+O es la pared de fontanería que acabamos de teclear.
            let _ = yo.escribir_tecla(b"\x0c");
        }
        Ok(yo)
    }

    /// Le escribe al shell.
    ///
    /// **norte ya no le teclea ÓRDENES** (#363): por aquí salen las teclas del
    /// lector y la fontanería del arranque, nada más. Mover el shell de
    /// directorio se hace por el buzón y no por el editor de línea — ver
    /// [`Self::ir_a`].
    ///
    /// # Errors
    /// Lo que falle el pty.
    pub fn escribir(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        escribir_crudo(&self.escritura, bytes)
    }

    /// Le escribe al shell UNA TECLA del lector.
    ///
    /// Hoy es [`Self::escribir`] y nada más. Se queda como puerta aparte
    /// porque lo que entra por aquí lo TECLEÓ alguien y lo que entra por la
    /// otra lo manda norte, y eso conviene que se vea en la llamada. Lo que
    /// hubo aquí —una exención para el Ctrl+L, que repinta el prompt sin
    /// tocar la línea— existía para no bajar un permiso que ya no existe:
    /// desde #363 norte no teclea órdenes, así que no hay permiso que cuidar.
    ///
    /// # Errors
    /// Lo que falle el pty.
    pub fn escribir_tecla(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.escribir(bytes)
    }

    /// Manda al shell al directorio `dir` (bytes nativos), SI se le puede.
    ///
    /// **No le teclea nada** (#363). Deja la ruta en el BUZÓN de esta sesión
    /// —un fichero 0600 bajo el directorio de ejecución— y el gancho del
    /// prompt la recoge y hace el `cd` la próxima vez que el shell esté entre
    /// dos órdenes. Devuelve si se dejó anotada.
    ///
    /// # Por qué no se teclea
    ///
    /// Teclear un `cd` exige saber que el editor de línea está en un prompt
    /// VACÍO, y eso no se puede saber desde fuera. norte lo aproximaba con
    /// «llegó un marcador de prompt y nadie ha escrito desde entonces», que es
    /// otra frase, y había al menos dos maneras de cumplir la segunda sin la
    /// primera:
    ///
    /// - la **pila de buffers de zsh**. `push-line` (Ctrl+Q por defecto)
    ///   aparta la línea, el shell pinta un prompt nuevo —marcador, permiso— y
    ///   acto seguido la devuelve al buffer. Un `print -z` de una función del
    ///   lector llega al mismo sitio sin tocar una tecla;
    /// - el **type-ahead**, en los tres. Lo que el lector teclea mientras el
    ///   shell está ocupado espera en la cola del pty. El marcador llega con
    ///   esos bytes todavía sin consumir, y el `cd` se concatenaba a ellos.
    ///
    /// En los dos casos el shell EJECUTABA una orden que nadie dio:
    /// `rm -rf tmpdir __norte_cd '...'`. Con el buzón no hay nada a lo que
    /// concatenarse —el canal de inyección al editor de línea desaparece— y el
    /// movimiento ocurre exactamente cuando el shell está demostrablemente
    /// entre órdenes, que es lo que había que demostrar.
    ///
    /// Lo que cambia para el lector: el `cd` se aplica en el prompt siguiente
    /// y no al instante. Es la semántica honesta, y es lo que ya pasaba cada
    /// vez que esto se negaba.
    ///
    /// Se niega si el shell no es uno de los tres que norte prepara: sin
    /// gancho no hay quien lea el buzón.
    ///
    /// # Errors
    /// Lo que falle al escribir el buzón.
    pub fn ir_a(&mut self, dir: &std::path::Path) -> std::io::Result<bool> {
        use std::os::unix::ffi::OsStrExt as _;
        if self.cual.is_none() {
            return Ok(false);
        }
        let Some(buzon) = self.buzon_fichero.as_deref() else {
            return Ok(false);
        };
        // El `_` final es un centinela y no un adorno: el shell lee el fichero
        // con `$(<f)`, que se come los saltos de línea del final, y un
        // directorio PUEDE acabar en uno.
        let mut bytes = dir.as_os_str().as_bytes().to_vec();
        bytes.push(b'_');
        escribir_buzon(buzon, &bytes)?;
        Ok(true)
    }

    /// Lo que el shell escribió desde la última vez, y se lo lleva.
    ///
    /// # Panics
    /// Si el hilo lector entró en pánico con el buzón cogido. No se recupera a
    /// propósito: el buzón envenenado significa que el lector murió a mitad de
    /// una escritura, así que lo que hubiera dentro ya no describe la pantalla
    /// del shell — y pintarlo igual es peor que caerse (regla 6).
    #[must_use]
    pub fn drenar(&self) -> Vec<u8> {
        let mut b = buzon_de(&self.buzon);
        std::mem::take(&mut b.pendiente)
    }

    /// El último directorio que el shell anunció, si anunció alguno.
    ///
    /// # Panics
    /// Igual que [`Self::drenar`]: buzón envenenado.
    #[must_use]
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        use std::os::unix::ffi::OsStrExt as _;
        let b = buzon_de(&self.buzon);
        let bytes = b.cwd.as_ref()?;
        Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }

    /// ¿Se fue el shell? (el lector vio EOF, o el hijo murió).
    ///
    /// # Panics
    /// Igual que [`Self::drenar`]: buzón envenenado.
    #[must_use]
    pub fn muerto(&mut self) -> bool {
        let cerrado = buzon_de(&self.buzon).cerrado;
        cerrado || matches!(self.hijo.try_wait(), Ok(Some(_)))
    }

    /// Dice al shell de qué tamaño es la terminal ahora.
    pub fn redimensionar(&self, size: (u16, u16)) {
        let _ = self.maestro.resize(portable_pty::PtySize {
            rows: size.1,
            cols: size.0,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Lo mata. Lo llama el cierre de norte: un shell huérfano hablándole a un
    /// pty que ya no lee nadie es un proceso que nadie sabe que existe.
    pub fn matar(&mut self) {
        let _ = self.hijo.kill();
        let _ = self.hijo.wait();
    }
}

/// El shell muere CON norte, salga norte por donde salga.
///
/// En `Drop` y no solo en el brazo de `app.quit`: el bucle también sale por
/// [`crate::event_loop::RunError`] —la terminal o el stream de eventos
/// rompiéndose—, y por ahí no pasa ningún cierre ordenado. Un shell que
/// sobrevive a su norte se queda hablándole a un pty que ya no lee nadie, con
/// el terminal del lector detrás.
impl Drop for Subshell {
    fn drop(&mut self) {
        self.matar();
        // Y se lleva su buzón: es de esta sesión y no le sirve a nadie más.
        // Un error aquí no se cuenta — el shell ya se fue y no hay a quién
        // decírselo—, y lo que queda si norte muere de golpe son unas decenas
        // de bytes en un directorio que el sistema limpia al cerrar sesión.
        if let Some(p) = &self.buzon_fichero {
            let _ = std::fs::remove_file(p);
            let _ = std::fs::remove_file(p.with_extension("cd.tmp"));
        }
    }
}

/// El nombre del ejecutable de una ruta de shell, para elegir el gancho.
///
/// Por BYTES (regla 1) y no por `to_string_lossy`: un `$SHELL` con un
/// componente que no es UTF-8 se convertía en `\u{FFFD}`, `Shell::parse`
/// fallaba, y el lector se quedaba con un subshell que nunca decía dónde
/// estaba — sin gancho, sin `cd` y sin un solo mensaje explicándolo. Ahora un
/// nombre no-UTF-8 sencillamente no es ninguno de los tres que conocemos, que
/// es la verdad.
fn shell_conocido(shell: &std::path::Path) -> Option<norte_frontend::shell::Shell> {
    use std::os::unix::ffi::OsStrExt as _;
    let nombre = shell.file_name()?;
    let texto = std::str::from_utf8(nombre.as_bytes()).ok()?;
    norte_frontend::shell::Shell::parse(texto)
}

/// El hilo que lee del pty sin parar y deja lo leído en el buzón.
///
/// Un hilo y no una task: `portable_pty` da un lector BLOQUEANTE, y meter una
/// lectura bloqueante en el executor es la regla 2. El hilo muere solo cuando
/// el pty se cierra.
/// El buzón, aunque el hilo lector haya muerto con él cogido.
///
/// `PoisonError::into_inner` y no un `expect` (regla 6): el veneno significa
/// que el lector cayó a mitad de una escritura, y lo peor que hay dentro es un
/// trozo de salida a medio añadir. Caerse por eso sería un pánico en el hilo
/// principal CON LA TERMINAL EN RAW MODE y los paneles sin pintar — un precio
/// desproporcionado por unos bytes de pantalla. Y no hay invariante que
/// sostenga un `expect`: nadie puede prometer que un hilo no entre en pánico.
fn buzon_de(buzon: &Mutex<Buzon>) -> std::sync::MutexGuard<'_, Buzon> {
    buzon
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Crea el buzón de esta sesión: un fichero VACÍO y 0600 con el nonce en el
/// nombre (#363).
///
/// Vive donde vive el socket del daemon y por el mismo criterio:
/// `$XDG_RUNTIME_DIR` si lo hay —ya es 0700 del usuario— y si no
/// `/tmp/norte-<uid>`. El modo se pone al CREARLO y no después: un fichero que
/// nace 0644 y se arregla luego tiene una ventana en la que otro usuario puede
/// abrirlo, y lo que se escribe aquí manda a un shell a un directorio.
///
/// `None` si no se puede. El llamante degrada: el panel no arrastra al shell y
/// no se instala el gancho. Es preferible a las dos alternativas —caerse, o
/// volver a teclear el `cd`—.
fn crear_buzon(nonce: &Nonce) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = match std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        Some(r) => std::path::PathBuf::from(r).join("norte"),
        // Sin `XDG_RUNTIME_DIR`, el mismo sitio que el socket del daemon:
        // `/tmp/norte-<uid>`. El uid sale del dueño de un fichero que acabamos
        // de crear, que es nuestro euid sin `unsafe` (regla 5) — la misma
        // vuelta que da `norte-client` para nombrar ese directorio.
        None => std::path::PathBuf::from(format!("/tmp/norte-{}", uid_propio()?)),
    };
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("subshell-{}.cd", nonce.as_str()));
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .ok()?;
    Some(path)
}

/// El uid de este proceso SIN `unsafe` (regla 5): el dueño de un fichero que
/// acabamos de crear es nuestro euid.
///
/// Solo se usa para NOMBRAR el directorio de `/tmp`, como en `norte-client`.
/// Lo que protege de verdad es el modo 0700 de ese directorio y el 0600 del
/// buzón, no el número del nombre.
fn uid_propio() -> Option<u32> {
    use std::os::unix::fs::MetadataExt as _;
    let sonda = std::env::temp_dir().join(format!(".norte-uid-{}", std::process::id()));
    std::fs::File::create(&sonda).ok()?;
    let uid = std::fs::metadata(&sonda).ok().map(|m| m.uid());
    let _ = std::fs::remove_file(&sonda);
    uid
}

/// Deja `bytes` en el buzón, de una pieza.
///
/// Por temporal y `rename` y no escribiendo encima: el gancho puede leerlo en
/// cualquier momento —corre en CADA prompt— y media ruta es un `cd` a un sitio
/// que no es. El `rename` dentro del mismo directorio es atómico, así que el
/// shell ve la ruta entera o la de antes, nunca un trozo.
fn escribir_buzon(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let tmp = path.with_extension("cd.tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
    }
    std::fs::rename(&tmp, path)
}

/// Escribe al pty.
///
/// Lo usan los dos escritores. La distinción importa: teclear una orden pone
/// algo en la línea, y contestar una consulta de terminal no — el programa que
/// preguntó está esperando esos bytes, no `readline`.
fn escribir_crudo(escritura: &Escritor, bytes: &[u8]) -> std::io::Result<()> {
    let mut e = escritura
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    e.write_all(bytes)?;
    e.flush()
}

fn lanzar_lector(
    mut lector: LectorDelPty,
    buzon: Arc<Mutex<Buzon>>,
    escritura: Escritor,
    nonce: norte_frontend::subshell::Nonce,
) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match lector.read(&mut buf) {
                Ok(0) | Err(_) => {
                    buzon_de(&buzon).cerrado = true;
                    return;
                }
                Ok(n) => {
                    // Contestar va ANTES de nada: el shell está PARADO
                    // esperándolo. Fuera del lock del buzón, que no pinta
                    // nada aquí y `escribir_crudo` coge el suyo.
                    if let Some(r) = norte_frontend::subshell::terminal_reply(&buf[..n]) {
                        let _ = escribir_crudo(&escritura, &r);
                    }
                    let mut b = buzon_de(&buzon);
                    // La cola del trozo anterior va DELANTE: un marcador
                    // partido entre dos lecturas se reconstruye aquí, y sin
                    // esto el cwd se perdería y el escape saldría por pantalla.
                    let mut trozo = std::mem::take(&mut b.cola);
                    trozo.extend_from_slice(&buf[..n]);
                    let s = scan_cwd(&trozo, &nonce);
                    b.cola = s.tail;
                    if let Some(c) = s.cwd {
                        // El gancho del prompt habló: el shell acaba de
                        // terminar lo que tuviera y va a pintar su `PS1`. Y ya
                        // recogió el buzón si había algo, porque el `cd` va
                        // DENTRO del gancho y antes del anuncio: esto es dónde
                        // se ha quedado, no dónde estaba.
                        b.cwd = Some(c);
                    }
                    b.pendiente.extend_from_slice(&s.visible);
                    // Se conserva la COLA de lo escrito: un proceso que
                    // escribe sin fin mientras nadie mira no puede comerse la
                    // memoria de norte.
                    //
                    // El corte se busca en el siguiente `\n` (regla 1 del
                    // terminal, no la de los nombres): el punto de corte lo
                    // elige norte, así que cortar a mitad de un CSI dejaría al
                    // terminal comiéndose los bytes de detrás como parámetros
                    // — la primera línea del volcado saldría rota.
                    if b.pendiente.len() > BUFFER_MAX {
                        let sobra = b.pendiente.len() - BUFFER_MAX;
                        let corte = b.pendiente[sobra..]
                            .iter()
                            .position(|c| *c == b'\n')
                            .map_or(b.pendiente.len(), |p| sobra + p + 1);
                        b.pendiente.drain(..corte);
                    }
                }
            }
        }
    });
}

/// El lector BLOQUEANTE que devuelve `portable_pty`, con nombre propio para
/// que la firma del hilo se lea.
type LectorDelPty = Box<dyn std::io::Read + Send>;

/// Los bytes que una tecla le manda a un shell, o `None` si esta tecla no
/// significa nada ahí.
///
/// Se traduce en vez de copiar el flujo crudo del tty, y esa es la decisión
/// que evita el fallo clásico: con un hilo leyendo `/dev/tty` en crudo, al
/// soltar el subshell ese hilo se queda bloqueado dentro de un `read` y se
/// come la SIGUIENTE tecla del lector — la que ya era para los paneles. Con un
/// solo lector (el de crossterm, el que la TUI ya usa) eso no puede pasar.
///
/// El precio es lo que no está en la tabla: ratón y pegado con corchetes no
/// llegan al shell. Un shell no los pide, y la alternativa era la tecla
/// robada.
///
/// ```
/// use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
/// use norte_tui::subshell::tecla_a_bytes;
///
/// assert_eq!(tecla_a_bytes(&KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)), Some(b"a".to_vec()));
/// assert_eq!(tecla_a_bytes(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Some(b"\r".to_vec()));
/// // Ctrl+C viaja como el byte 3, que es lo que hace que interrumpa.
/// assert_eq!(tecla_a_bytes(&KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)), Some(vec![3]));
/// ```
#[must_use]
pub fn tecla_a_bytes(k: &crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    use norte_frontend::keymap::{Chord, KeyCode, Mods};
    // **La TABLA es compartida** (`norte_frontend::subshell::chord_a_bytes`), y
    // aquí solo queda traducir el evento de crossterm al acorde canónico que
    // ella entiende. Estuvo escrita dos veces desde que el panel de terminal
    // (#362) la necesitó también en la ventana, y dos tablas son dos sitios
    // donde `F10` deja de salir de un `htop`.
    //
    // `BackTab` es lo único que el acorde canónico no nombra: para el keymap
    // es shift+tab, que es exactamente lo que se construye aquí.
    let (mods, code) = if k.code == crossterm::event::KeyCode::BackTab {
        (
            Mods {
                shift: true,
                ..Mods::default()
            },
            KeyCode::Tab,
        )
    } else {
        crate::keymap::chord_from_crossterm(k.modifiers, k.code)?.parts()
    };
    norte_frontend::subshell::chord_a_bytes(Chord::new(mods, code))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un bash SIN la configuración de nadie, interactivo.
    ///
    /// `--norc --noprofile` porque lo que se prueba es lo que norte instala,
    /// no lo que el `.bashrc` de quien corre el test haga; `-i` porque un
    /// shell no interactivo no tiene prompt, y el gancho del prompt es
    /// justamente lo que hay que ver funcionar.
    fn bash(dir: &std::path::Path) -> Subshell {
        Subshell::arrancar_con(
            std::path::Path::new("/bin/bash"),
            &["--norc", "--noprofile", "-i"],
            dir,
            (80, 24),
        )
        .expect("arranca bash")
    }

    /// **Un shell de verdad, vivo, que recuerda** (#142).
    ///
    /// Se afirma sobre lo que SOLO el shell puede producir (`echo $((6*7))`),
    /// no sobre el texto de la orden: el eco de la disciplina de línea devuelve
    /// lo tecleado tal cual, así que buscar la propia orden en la salida no
    /// demuestra que se haya ejecutado nada.
    #[test]
    fn un_subshell_vive_entre_dos_ordenes() {
        let dir = tempfile::tempdir().expect("tempdir");
        // `arrancar` usa `$SHELL` (`login_shell`), o sea el de quien corre el
        // test: solo se comprueba lo que todo shell POSIX hace igual.
        let mut sh = bash(dir.path());

        sh.escribir(b"echo uno-$((6*7))\n").expect("escribe");
        assert!(
            espera_hasta(&sh, b"uno-42").is_some(),
            "el shell EJECUTA lo primero"
        );
        sh.escribir(b"echo dos-$((6*7))\n").expect("escribe");
        assert!(
            espera_hasta(&sh, b"dos-42").is_some(),
            "y sigue vivo para lo segundo: eso es lo que lo hace un subshell"
        );
        sh.matar();
    }

    /// **El shell DICE dónde está, y norte lo entiende.**
    ///
    /// Es el test que faltaba, y su ausencia dejó pasar que el gancho llevaba
    /// el `ESC` y el `BEL` CRUDOS: el editor de línea se comía el `ESC ]` como
    /// prefijo meta, así que lo que quedaba instalado imprimía
    /// `777;norte-cwd;/casa` sin marco OSC. `scan_cwd` no reconocía nada, el
    /// panel no seguía al shell JAMÁS —la promesa entera de #142— y el lector
    /// veía ese texto en cada prompt. Con el shell arrancado en `dir`, el
    /// primer prompt ya tiene que anunciarlo.
    /// Corre para LOS TRES shells que norte prepara, cada uno con su propia
    /// sintaxis de gancho: son tres textos distintos y un solo test los
    /// cubría a uno. Se salta el que no esté instalado — es un test de
    /// integración, no una razón para poner roja la máquina de alguien.
    #[test]
    fn el_shell_anuncia_donde_esta() {
        let mut probados = 0;
        for (bin, args) in [
            ("/bin/bash", &["--norc", "--noprofile", "-i"][..]),
            ("/usr/bin/zsh", &["-f", "-i"][..]),
            ("/usr/bin/fish", &["--no-config", "-i"][..]),
        ] {
            let ruta = std::path::Path::new(bin);
            if !ruta.exists() {
                continue;
            }
            probados += 1;
            let dir = tempfile::tempdir().expect("tempdir");
            let real = dir.path().canonicalize().expect("canonicalize");
            let sh = Subshell::arrancar_con(ruta, args, &real, (80, 24)).expect("arranca");
            let hasta = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut dicho = None;
            while std::time::Instant::now() < hasta && dicho.is_none() {
                dicho = sh.cwd();
                let _ = sh.drenar();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert_eq!(
                dicho.as_deref(),
                Some(real.as_path()),
                "{bin}: el gancho no llegó entero, o dijo otro sitio"
            );
        }
        assert!(probados > 0, "ningún shell conocido en esta máquina");
    }

    /// Y SIGUE al panel: un `cd` sobre un directorio con un nombre hostil no
    /// se convierte en una orden.
    ///
    /// El nombre lleva los bytes que readline EJECUTA (`0x15` borra la línea
    /// entera), que es lo que el entrecomillado no paraba: la comilla protege
    /// un byte que entra en el buffer, y ése no entra. `/tmp/…\x15id #`
    /// ejecutaba `id`.
    #[test]
    fn el_subshell_sigue_al_panel_con_un_nombre_hostil() {
        use std::os::unix::ffi::OsStrExt as _;
        let raiz = tempfile::tempdir().expect("tempdir");
        let raiz = raiz.path().canonicalize().expect("canonicalize");
        let hostil = raiz.join(std::ffi::OsStr::from_bytes(
            b"a b; echo pwned\x15echo pwned #",
        ));
        std::fs::create_dir(&hostil).expect("mkdir");

        let sh = bash(&raiz);
        // El `cd` solo se manda con el shell PARADO en su prompt, que es lo
        // que este bucle espera (y lo que impide que se concatene con lo que
        // el lector hubiera dejado a medias).
        let mut sh = sh;
        let hasta = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut mandado = false;
        // Lo drenado se GUARDA: cuando este bucle se agotó una vez bajo carga
        // (#360) el rojo no dijo qué había escrito el shell hasta entonces, y
        // sin eso no se podía distinguir «no llegó el prompt» de «llegó y el
        // permiso se había perdido». Es la diferencia que costó el diagnóstico.
        let mut visto = Vec::new();
        while std::time::Instant::now() < hasta && !mandado {
            visto.extend_from_slice(&sh.drenar());
            mandado = sh.ir_a(&hostil).expect("cd");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            mandado,
            "nunca hubo un prompt al que mandarle el cd; el shell escribió: {}",
            String::from_utf8_lossy(&visto)
        );

        // Se comprueba por el MARCADOR, no por el eco: el shell dice dónde
        // está, y ahí es donde tiene que estar.
        let hasta = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut llego = false;
        while std::time::Instant::now() < hasta {
            visto.extend_from_slice(&sh.drenar());
            if sh.cwd().as_deref() == Some(hostil.as_path()) {
                llego = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(llego, "el shell no acabó dentro del directorio hostil");
        // Y nada se ejecutó. `pwned` aparece en el nombre del directorio —y por
        // tanto en el prompt de muchos shells—, así que lo que se busca es la
        // SALIDA de un `echo`: la palabra sola en su línea.
        let texto = String::from_utf8_lossy(&visto);
        assert!(
            !texto.lines().any(|l| l.trim() == "pwned"),
            "algo del nombre se ejecutó: {texto}"
        );
        sh.matar();
    }

    /// **Un Ctrl+L no apaga el seguimiento del panel** (#360).
    ///
    /// Ctrl+L no es una orden: `readline`/ZLE lo ejecutan como una función que
    /// limpia la pantalla y REPINTA el prompt, dejando la línea exactamente
    /// como estaba. No pone nada en ella.
    ///
    /// Lo apagaba igual. Cuando norte tecleaba el `cd`, hacía falta un permiso
    /// —«llegó un marcador y nadie ha escrito desde entonces»— y `escribir` lo
    /// bajaba para TODO lo que se mandara. El repintado no corre
    /// `PROMPT_COMMAND` —lo corre bash antes de leer una orden NUEVA—, así que
    /// detrás del Ctrl+L no venía ningún marcador y el permiso se quedaba
    /// abajo hasta el Intro siguiente. Entre medias el panel no seguía al
    /// shell: la promesa entera de #142, apagada por la tecla de limpiar la
    /// pantalla.
    ///
    /// Desde #363 no hay permiso que bajar: el destino se anota en el buzón
    /// pase lo que pase, y el gancho lo recoge en el prompt siguiente. El test
    /// se queda porque la propiedad sigue siendo la misma —un Ctrl+L no puede
    /// dejar al panel sin poder arrastrar al shell— y porque es la carrera que
    /// la suite vio una vez bajo carga.
    ///
    /// El Intro de en medio es del LECTOR y no de norte: el gancho corre entre
    /// dos órdenes, así que hace falta que el shell llegue a una. Ése es el
    /// precio del cambio, y es el que #363 aceptó a cambio de cerrar la
    /// inyección: el movimiento se aplica en el prompt siguiente y no al
    /// instante.
    #[test]
    fn un_ctrl_l_no_apaga_el_seguimiento() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = dir.path().canonicalize().expect("canonicalize");
        let otro = dir.join("otro");
        std::fs::create_dir(&otro).expect("mkdir");
        let mut sh = bash(&dir);
        espera_quieto(&sh);
        sh.escribir_tecla(b"\x0c").expect("ctrl+l");
        let _ = sh.drenar();

        assert!(
            sh.ir_a(&otro).expect("cd"),
            "un Ctrl+L dejó al panel sin poder seguir al shell"
        );
        // La línea está vacía, así que el Intro del lector no ejecuta nada:
        // solo lleva al shell a su prompt siguiente, que es donde el gancho
        // recoge el buzón.
        sh.escribir_tecla(b"\n").expect("intro");
        let hasta = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut llego = false;
        while std::time::Instant::now() < hasta {
            let _ = sh.drenar();
            if sh.cwd().as_deref() == Some(otro.as_path()) {
                llego = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(llego, "el `cd` de después del Ctrl+L no llegó a ejecutarse");
        sh.matar();
    }

    /// **Una línea a medio escribir no se convierte en una orden.**
    ///
    /// Es el peor de los fallos que tuvo esto: el `cd` se tecleaba al entrar,
    /// así que un `rm -rf tmpdir` que el lector había escrito y no ejecutado
    /// se convertía en `rm -rf tmpdircd -- \'/otro\'` en cuanto volvía. Y lo
    /// que hace el caso normal y no raro es que este subshell PROMETE
    /// conservar la línea a medias.
    ///
    /// Desde #363 no se teclea nada: el destino se deja en el buzón y el
    /// gancho lo recoge en el prompt siguiente. Así que `ir_a` SÍ acepta —hay
    /// dónde anotarlo— y lo que se comprueba es que la línea del lector siga
    /// intacta y que no se ejecute.
    #[test]
    fn una_linea_a_medias_no_se_ejecuta() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = dir.path().canonicalize().expect("canonicalize");
        let otro = dir.join("otro");
        std::fs::create_dir(&otro).expect("mkdir");
        let mut sh = bash(&dir);
        // Esperar a que el shell se QUEDE quieto, y entonces dejar algo a
        // medias: las órdenes de instalación producen un prompt cada una, y
        // teclear encima de una que aún no llegó sería una carrera del test.
        espera_quieto(&sh);
        // `pwn''ed` y no `pwned`: lo que el pty ECOA es la línea tal cual, así
        // que buscar «pwned» encontraría el eco y no la ejecución. Con las
        // comillas en medio, la cadena entera solo aparece si bash la ejecutó.
        sh.escribir(b"echo pwn''ed").expect("media línea");
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = sh.drenar();

        assert!(
            sh.ir_a(&otro).expect("cd"),
            "hay buzón donde anotarlo, así que se anota"
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        let texto = String::from_utf8_lossy(&sh.drenar()).into_owned();
        assert!(
            !texto.contains("pwned"),
            "se ejecutó lo que el lector no ejecutó: {texto}"
        );
        // Y la línea sigue ahí: el Intro la ejecuta ENTERA y sola.
        sh.escribir(b"\n").expect("intro");
        std::thread::sleep(std::time::Duration::from_millis(400));
        let texto = String::from_utf8_lossy(&sh.drenar()).into_owned();
        assert!(
            texto.contains("pwned"),
            "la línea del lector no sobrevivió al movimiento: {texto}"
        );
        sh.matar();
    }

    /// **El type-ahead no se concatena con nada** (#363).
    ///
    /// El caso que el flag `en_prompt` no podía ver, y que no necesita ningún
    /// shell en particular: lo que el lector teclea mientras el shell está
    /// OCUPADO espera en la cola del pty. El marcador del prompt llegaba con
    /// esos bytes todavía sin consumir —«nadie escribió desde el marcador» era
    /// cierto y «la línea está vacía» era falso— y el `cd` se pegaba detrás:
    /// `rm -rf tmpdir __norte_cd \'...\'`.
    ///
    /// Con el buzón no hay nada que concatenar. Aquí se reproduce con un
    /// `sleep` por delante, que es lo que mantiene a readline sin leer.
    #[test]
    fn lo_tecleado_mientras_el_shell_esta_ocupado_no_arrastra_un_cd() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = dir.path().canonicalize().expect("canonicalize");
        let otro = dir.join("otro");
        std::fs::create_dir(&otro).expect("mkdir");
        let mut sh = bash(&dir);
        espera_quieto(&sh);

        // El shell se pone a dormir, y el lector teclea encima sin Intro: esos
        // bytes se quedan en la cola del pty hasta que `sleep` acabe.
        sh.escribir(b"sleep 1\n").expect("sleep");
        std::thread::sleep(std::time::Duration::from_millis(150));
        // Mismo truco que en el test de al lado: el eco no puede confundirse
        // con la ejecución.
        sh.escribir(b"echo pwn''ed").expect("type-ahead");
        // Y norte mueve el panel justo cuando el prompt vuelve.
        assert!(sh.ir_a(&otro).expect("cd"), "se anota en el buzón");
        std::thread::sleep(std::time::Duration::from_millis(1800));

        let texto = String::from_utf8_lossy(&sh.drenar()).into_owned();
        assert!(
            !texto.contains("pwned"),
            "el type-ahead del lector acabó ejecutándose: {texto}"
        );
        // Y el shell SÍ se movió: el gancho recogió el buzón en su prompt.
        assert_eq!(
            sh.cwd().as_deref(),
            Some(otro.as_path()),
            "el gancho no recogió el buzón"
        );
        sh.matar();
    }

    /// Las teclas que un shell necesita llegan como los bytes que espera, y lo
    /// que no significa nada ahí no llega.
    #[test]
    fn las_teclas_llegan_como_bytes_de_terminal() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let t = |code, mods| tecla_a_bytes(&KeyEvent::new(code, mods));
        assert_eq!(t(KeyCode::Up, KeyModifiers::NONE), Some(b"\x1b[A".to_vec()));
        assert_eq!(t(KeyCode::Backspace, KeyModifiers::NONE), Some(vec![0x7f]));
        assert_eq!(t(KeyCode::Char('d'), KeyModifiers::CONTROL), Some(vec![4]));
        // Alt es ESC delante: es lo que hace que `alt+f` mueva una palabra.
        assert_eq!(
            t(KeyCode::Char('f'), KeyModifiers::ALT),
            Some(vec![0x1b, b'f'])
        );
        // Las de función llegan: un `htop` dentro del subshell las usa, y F10
        // es lo que lo cierra.
        assert_eq!(
            t(KeyCode::F(1), KeyModifiers::NONE),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            t(KeyCode::F(10), KeyModifiers::NONE),
            Some(b"\x1b[21~".to_vec())
        );
        // Los acordes de control que no son letras también son bytes:
        // Ctrl+\ es SIGQUIT, no una barra invertida.
        assert_eq!(
            t(KeyCode::Char('\\'), KeyModifiers::CONTROL),
            Some(vec![28])
        );
        assert_eq!(t(KeyCode::Char(' '), KeyModifiers::CONTROL), Some(vec![0]));
        // Y una tecla que un shell no usa no se inventa.
        assert_eq!(t(KeyCode::F(20), KeyModifiers::NONE), None);
        assert_eq!(t(KeyCode::CapsLock, KeyModifiers::NONE), None);
    }

    /// Un carácter no-ASCII viaja en UTF-8 entero: escribir `ñ` en el subshell
    /// no puede mandar medio carácter.
    #[test]
    fn un_caracter_multibyte_viaja_entero() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        assert_eq!(
            tecla_a_bytes(&KeyEvent::new(KeyCode::Char('ñ'), KeyModifiers::NONE)),
            Some("ñ".as_bytes().to_vec())
        );
    }

    /// Espera a que lo escrito por el shell contenga `aguja`, sin colgarse.
    ///
    /// Sondea en vez de dormir un rato fijo: un shell tarda lo que tarda en
    /// arrancar el `.bashrc` de quien corre el test, y un `sleep` elegido a
    /// ojo es un test que se pone rojo en la máquina cargada de otro.
    /// Devuelve TODO lo acumulado, no un `bool`: el que llama suele querer
    /// afirmar algo sobre lo que llegó, y con un `bool` esos bytes se quedaban
    /// dentro de esta función y se tiraban — la comprobación de después miraba
    /// un buzón ya vacío y no podía fallar dijera lo que dijera.
    /// Espera a que el shell deje de escribir y haya anunciado un prompt.
    ///
    /// Las órdenes de instalación producen un prompt —y un marcador— CADA UNA,
    /// y llegan cuando llegan: un test que teclee encima de una que aún no ha
    /// llegado se pone rojo por la carrera y no por el código.
    fn espera_quieto(sh: &Subshell) {
        let hasta = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut quietos = 0;
        while std::time::Instant::now() < hasta {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if sh.drenar().is_empty() && sh.cwd().is_some() {
                quietos += 1;
                if quietos >= 3 {
                    return;
                }
            } else {
                quietos = 0;
            }
        }
        panic!("el shell no se quedó quieto");
    }

    fn espera_hasta(sh: &Subshell, aguja: &[u8]) -> Option<Vec<u8>> {
        let hasta = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut visto: Vec<u8> = Vec::new();
        while std::time::Instant::now() < hasta {
            visto.extend_from_slice(&sh.drenar());
            if visto.windows(aguja.len()).any(|w| w == aguja) {
                return Some(visto);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        None
    }
}
