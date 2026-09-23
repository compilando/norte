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

use norte_frontend::subshell::{Nonce, cd_command, install, scan_cwd};

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
    /// Llegó un marcador de prompt y NADIE le ha escrito al shell desde
    /// entonces: está parado, con la línea vacía, esperando una orden.
    ///
    /// Es la condición que autoriza a teclearle un `cd` ([`Subshell::ir_a`]).
    /// Sin ella, el `cd` se concatenaba a lo que el lector hubiera dejado a
    /// medio escribir —que es justo lo que este subshell promete conservar— y
    /// el shell EJECUTABA una orden que nadie tecleó: un `rm -rf tmpdir` a
    /// medias se convertía en `rm -rf tmpdircd -- '/otro/sitio'`. Y si lo que
    /// había delante era un `vim`, los bytes entraban en su buffer.
    ///
    /// La condición es «desde el marcador nadie ESCRIBIÓ», y no «detrás del
    /// marcador no vino nada». Lo segundo parece más directo y es falso: el
    /// gancho corre ANTES de que el shell pinte su prompt (`PROMPT_COMMAND`
    /// primero, `PS1` después), así que detrás del marcador viene SIEMPRE el
    /// prompt y la condición no se cumplía jamás. Lo que sí distingue los dos
    /// casos es quién escribe: las teclas del lector pasan por
    /// [`Subshell::escribir`] igual que las de norte, así que una línea a
    /// medias baja este flag y solo lo vuelve a subir el marcador del prompt
    /// SIGUIENTE — el que aparece cuando esa línea se ejecutó o se abandonó.
    en_prompt: bool,
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
        let mut yo = Self {
            escritura,
            maestro: par.master,
            hijo,
            buzon,
            cual,
        };
        // El gancho del prompt y la función del `cd` se mandan como si el
        // lector los tecleara: NO se toca ningún fichero suyo. Un `.bashrc`
        // que norte editara sería una modificación permanente por una función
        // que se apaga al salir — y sobreviviría a un norte que ya no está.
        if let Some(cual) = cual {
            let _ = yo.escribir(install(cual, &nonce).as_bytes());
            // Ctrl+L: la orden de `readline`/ZLE/fish que limpia la pantalla y
            // repinta el prompt. Sin esto, lo primero que el lector ve en su
            // primer Ctrl+O es la pared de fontanería que acabamos de teclear.
            // No baja `en_prompt` —ver `escribir_tecla`—, y eso es lo que
            // impide que se lleve por delante el primer marcador (#360).
            let _ = yo.escribir_tecla(b"\x0c");
        }
        Ok(yo)
    }

    /// Le escribe al shell.
    ///
    /// Toda escritura —las teclas del lector y las órdenes de norte— baja
    /// `Buzon::en_prompt`: a partir de aquí hay algo en la línea, y no se le
    /// puede teclear nada más hasta que el prompt siguiente diga que se fue.
    /// Sin excepciones: lo que sea una TECLA del lector va por
    /// [`Self::escribir_tecla`], que es donde vive la única que hay.
    ///
    /// # Errors
    /// Lo que falle el pty.
    ///
    /// # Panics
    /// Igual que [`Self::drenar`]: buzón envenenado.
    pub fn escribir(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        buzon_de(&self.buzon).en_prompt = false;
        escribir_crudo(&self.escritura, bytes)
    }

    /// Le escribe al shell UNA TECLA del lector.
    ///
    /// Es [`Self::escribir`] salvo para las teclas que el editor de línea
    /// ejecuta sin poner nada en la línea, que no bajan `Buzon::en_prompt`.
    /// Hoy es una: el Ctrl+L (`0x0c`), que limpia la pantalla y REPINTA el
    /// prompt dejando el buffer como estaba.
    ///
    /// Bajarlo ahí lo dejaba abajo para siempre, y el motivo es que nada lo
    /// volvería a subir: el marcador del cwd lo imprime `PROMPT_COMMAND` —o
    /// `precmd`, o el evento de fish—, que el shell corre antes de leer una
    /// orden NUEVA y no en un repintado. Hasta el Intro siguiente,
    /// [`Self::ir_a`] decía no y el panel no seguía al shell: la promesa de
    /// #142, apagada por la tecla de limpiar la pantalla. Y como el arranque
    /// manda esa misma tecla detrás de la fontanería, bajo carga el primer
    /// marcador caía entre los dos escritos y se perdía (#360).
    ///
    /// **La excepción depende de quién llama, no de los bytes**, y esa es la
    /// diferencia que importa: un PEGADO llega con contenido cualquiera, y un
    /// pegado que sea exactamente un `^L` —un separador de página, que en texto
    /// plano es corriente— se llevaría la excepción sin ser una tecla. Hoy
    /// además saldría bien por accidente, porque norte no envuelve el pegado en
    /// `ESC[200~`/`ESC[201~` y el editor de línea lo EJECUTA; el día que lo
    /// envuelva —que es lo que un `vim` delante pide— ese byte pasaría a ser un
    /// carácter en la línea con el permiso todavía puesto, o sea el fallo de
    /// #142 otra vez y sin test que lo pille. El pegado va por
    /// [`Self::escribir`] y baja el flag siempre.
    ///
    /// # Errors
    /// Lo que falle el pty.
    ///
    /// # Panics
    /// Igual que [`Self::drenar`]: buzón envenenado.
    pub fn escribir_tecla(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if no_toca_la_linea(bytes) {
            return escribir_crudo(&self.escritura, bytes);
        }
        self.escribir(bytes)
    }

    /// Manda al shell al directorio `dir` (bytes nativos), SI se le puede.
    ///
    /// Devuelve si se mandó. Se niega en tres casos, y los tres son la misma
    /// idea —no teclear en un sitio que no es un prompt vacío—:
    ///
    /// - el shell no es uno de los tres que norte prepara (no tiene
    ///   `__norte_cd`, así que el `cd` sería un error de sintaxis impreso en
    ///   la cara del lector, y en CADA pulsación);
    /// - el shell no está parado en su prompt (`Buzon::en_prompt`): puede
    ///   haber una línea a medio escribir —que este subshell promete
    ///   conservar— o un `vim` delante, y los bytes irían a parar ahí;
    /// - la ruta lleva un NUL, que ningún nombre puede tener.
    ///
    /// # Errors
    /// Lo que falle el pty.
    ///
    /// # Panics
    /// Igual que [`Self::drenar`]: buzón envenenado.
    pub fn ir_a(&mut self, dir: &std::path::Path) -> std::io::Result<bool> {
        use std::os::unix::ffi::OsStrExt as _;
        let Some(cual) = self.cual else {
            return Ok(false);
        };
        if !buzon_de(&self.buzon).en_prompt {
            return Ok(false);
        }
        let Some(cmd) = cd_command(cual, dir.as_os_str().as_bytes()) else {
            return Ok(false);
        };
        // `escribir` baja `en_prompt` solo: hasta que llegue el marcador del
        // prompt siguiente no se le teclea nada más.
        self.escribir(&cmd)?;
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

/// ¿Es una tecla que el editor de línea ejecuta SIN poner nada en la línea?
///
/// Hoy es una sola: el Ctrl+L (`0x0c`). El por qué —y por qué lo pregunta solo
/// `Subshell::escribir_tecla`— está ahí.
///
/// Se compara la tecla ENTERA y no se busca el byte dentro: una escritura que
/// lleve un `0x0c` entre otros bytes sí pone algo en la línea, y las teclas
/// llegan de una en una ([`tecla_a_bytes`]) — un Alt+Ctrl+L, que viaja como
/// `ESC 0x0c`, no es ésta.
///
/// Vale para la atadura DE FÁBRICA, y no se puede comprobar: `Subshell::arrancar`
/// usa el `$SHELL` del lector con su configuración entera detrás, y un
/// `bind '"\C-l": self-insert'` —o una macro de `readline`, que dejaría su texto
/// en la línea— convierte esto en mentira. norte no puede verlo desde aquí; lo
/// que sí puede es decirlo en vez de suponerlo.
fn no_toca_la_linea(bytes: &[u8]) -> bool {
    bytes == [0x0c]
}

/// Escribe al pty SIN tocar `en_prompt`.
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
                        b.cwd = Some(c);
                        // El gancho del prompt habló: el shell acaba de
                        // terminar lo que tuviera y va a pintar su `PS1`. Es
                        // lo ÚNICO que autoriza a teclearle un `cd` (ver
                        // `Buzon::en_prompt`), y lo baja cualquier escritura.
                        b.en_prompt = true;
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
    use crossterm::event::{KeyCode, KeyModifiers};
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let cuerpo: Vec<u8> = match k.code {
        // Ctrl+letra es el byte de control de toda la vida: `a`→1, `c`→3.
        // Sin esto, un Ctrl+C dentro del subshell no interrumpe nada.
        KeyCode::Char(c) if ctrl && c.is_ascii_alphabetic() => {
            vec![(c.to_ascii_lowercase() as u8) - b'a' + 1]
        }
        // Los OTROS acordes de control, que también son bytes y no letras:
        // Ctrl+\ es SIGQUIT, Ctrl+espacio es el NUL con el que `readline` pone
        // la marca, Ctrl+[ es Escape. Sin esta rama viajaba el carácter tal
        // cual, así que Ctrl+\ mandaba una barra invertida.
        KeyCode::Char(c) if ctrl && matches!(c, '@' | ' ' | '[' | '\\' | ']' | '^' | '_' | '?') => {
            vec![match c {
                '@' | ' ' => 0,
                '?' => 0x7f,
                otro => (otro as u8) & 0x1f,
            }]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        // Las teclas de función, que un `htop` o un editor dentro del subshell
        // sí usan: sin esto, F10 no salía de nada. Códigos xterm, que son los
        // que `terminfo` da para `xterm`/`screen`/`tmux`.
        KeyCode::F(1) => b"\x1bOP".to_vec(),
        KeyCode::F(2) => b"\x1bOQ".to_vec(),
        KeyCode::F(3) => b"\x1bOR".to_vec(),
        KeyCode::F(4) => b"\x1bOS".to_vec(),
        // El salto 16, 22 no es un error: xterm nunca los asignó.
        KeyCode::F(n @ 5..=12) => {
            let num = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n) - 5];
            format!("\x1b[{num}~").into_bytes()
        }
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        // DEL (127) y no BS (8): es lo que manda un terminal moderno, y lo que
        // `readline` espera para borrar hacia atrás.
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        _ => return None,
    };
    // Alt es ESC delante, que es lo que hace que `alt+f` mueva una palabra.
    if alt {
        let mut con_esc = vec![0x1b];
        con_esc.extend_from_slice(&cuerpo);
        return Some(con_esc);
    }
    Some(cuerpo)
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
    /// como estaba. No pone nada en ella, así que no puede quitarle a `ir_a` el
    /// permiso para teclear.
    ///
    /// Lo quitaba. `escribir` baja `en_prompt` para TODO lo que se manda, y el
    /// repintado de `readline` no corre `PROMPT_COMMAND` —lo corre bash antes
    /// de leer una orden NUEVA—, así que detrás del Ctrl+L no viene ningún
    /// marcador y el flag se quedaba abajo hasta que el lector pulsara Intro.
    /// Entre medias el panel no seguía al shell: la promesa entera de #142,
    /// apagada por la tecla de limpiar la pantalla.
    ///
    /// Y es la carrera que la suite vio una vez bajo carga: el arranque manda
    /// la fontanería y después un Ctrl+L, y entre los dos escritos cabe el
    /// primer marcador. Si cabe, el Ctrl+L lo tira y `ir_a` dice no para
    /// siempre — diez segundos de espera y rojo.
    #[test]
    fn un_ctrl_l_no_apaga_el_seguimiento() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dir = dir.path().canonicalize().expect("canonicalize");
        let otro = dir.join("otro");
        std::fs::create_dir(&otro).expect("mkdir");
        let mut sh = bash(&dir);
        // Con el shell quieto hay marcador, y por tanto permiso para teclear.
        espera_quieto(&sh);
        sh.escribir_tecla(b"\x0c").expect("ctrl+l");
        let _ = sh.drenar();

        assert!(
            sh.ir_a(&otro).expect("cd"),
            "un Ctrl+L dejó al panel sin poder seguir al shell"
        );
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
    /// Es el peor de los fallos que tuvo esto: el `cd` se tecleaba SIEMPRE al
    /// entrar, así que un `rm -rf tmpdir` que el lector había escrito y no
    /// ejecutado se convertía en `rm -rf tmpdircd -- '/otro'` en cuanto
    /// volvía. Y lo que hace que el caso sea normal y no raro es que este
    /// subshell PROMETE conservar la línea a medias.
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
        sh.escribir(b"echo pwned").expect("media línea");
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = sh.drenar();
        // Y un Ctrl+L por medio no lo arregla: la excepción de
        // `escribir_tecla` solo puede DEJAR el permiso como esté, nunca
        // ponerlo. Sin esta línea, un refactor que leyera «un Ctrl+L significa
        // que volvemos a un prompt limpio» y subiera el flag pasaría la suite
        // entera y reabriría el `rm -rf tmpdir` de #142. Va aquí y no en un
        // test hermano porque la línea a medias es exactamente el estado que
        // tiene que sobrevivir al repintado.
        sh.escribir_tecla(b"\x0c").expect("ctrl+l");
        std::thread::sleep(std::time::Duration::from_millis(300));
        let _ = sh.drenar();

        assert!(
            !sh.ir_a(&otro).expect("cd"),
            "con una línea a medias no se teclea nada, ni después de un Ctrl+L"
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
        let texto = String::from_utf8_lossy(&sh.drenar()).into_owned();
        assert!(
            !texto.lines().any(|l| l.trim() == "pwned"),
            "se ejecutó lo que el lector no ejecutó: {texto}"
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
