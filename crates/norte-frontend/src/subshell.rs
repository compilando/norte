//! El SUBSHELL persistente: la mitad que no toca ningún pty (#142).
//!
//! `app.toggle-panels` enseñaba el scrollback del terminal desde el que
//! arrancó norte. Midnight Commander hace otra cosa que se nota a la primera:
//! mantiene un shell VIVO detrás de los paneles, así que la tecla te deja en
//! un shell que recuerda lo que escribiste la vez anterior, y el directorio
//! del panel y el del shell se siguen el uno al otro.
//!
//! Tener un shell vivo pide tres cosas, y solo la primera es fontanería:
//!
//! 1. un pty y un hijo de larga vida — eso vive en el frontend que tiene la
//!    terminal (`norte-tui`), porque es quien puede cedérsela;
//! 2. saber DÓNDE está ese shell, que es lo que hace que el panel lo pueda
//!    seguir. No se adivina: se le pide al shell que lo DIGA, imprimiendo un
//!    marcador en cada prompt;
//! 3. mandarle un `cd` cuando el panel se mueve, sin que un nombre de fichero
//!    hostil se convierta en una orden.
//!
//! Los puntos 2 y 3 son texto y bytes, no I/O, así que viven aquí y se
//! prueban sin levantar un shell.
//!
//! # La regla que gobierna este módulo entero
//!
//! **Lo que se le escribe a un pty NO lo lee un parser de shell: lo lee el
//! EDITOR DE LÍNEA.** readline (bash), ZLE (zsh) y el lector de fish ven cada
//! byte antes que nadie, y los bytes de control son ÓRDENES suyas, no texto:
//!
//! | byte | readline |
//! | --- | --- |
//! | `0x15` | `unix-line-discard` — borra la línea entera |
//! | `0x01` | `beginning-of-line` — lo que siga se inserta DELANTE |
//! | `0x7f` | `backward-delete-char` — borra hacia atrás |
//! | `0x1b` | prefijo meta — se come la secuencia que venga |
//!
//! Entrecomillar no defiende de ninguno: la comilla protege un byte que ENTRA
//! en el buffer, y estos no entran, se ejecutan. Un directorio llamado
//! `<0x15>id #` —legal en cualquier Unix, y creable por un `tar` cualquiera—
//! convertía un `cd -- '…'` entrecomillado en un `id` ejecutado.
//!
//! De ahí las dos reglas duras de este módulo:
//!
//! - **Todo lo que norte teclea es ASCII imprimible.** Los bytes de verdad
//!   viajan como escapes OCTALES dentro de un `printf`, que es texto plano
//!   para el editor de línea y bytes exactos para el shell.
//! - **Nada de lo que el shell imprime se cree sin autenticar.** El marcador
//!   lleva un nonce de sesión: ver [`Nonce`].
//!
//! # Por qué un marcador y no OSC 7
//!
//! OSC 7 (`\e]7;file://host/path\e\\`) es lo que emiten los terminales
//! modernos para decir el cwd, y sería lo elegante. Pero lo emite quien quiere:
//! bash sin configurar no lo hace, y depender de ello daría un panel que sigue
//! al shell en unas máquinas y no en otras. El marcador lo instala norte en el
//! prompt del shell que ARRANCA él, así que está donde tiene que estar.

use crate::shell::Shell;

/// El prefijo del marcador que el subshell imprime en cada prompt.
///
/// OSC privado (`ESC ] 777 ; …`) porque es el rango que los multiplexores
/// dejan pasar sin interpretarlo, y con el nombre dentro para que un `77x` de
/// otro programa no se confunda con éste.
pub const CWD_MARKER_PREFIX: &str = "\x1b]777;norte-cwd;";

/// El terminador del marcador: BEL, que es el que todos los shells saben
/// escribir sin escaparse la vida.
///
/// BEL es un byte LEGAL en un nombre de fichero, así que el shell lo escapa
/// antes de imprimirlo ([`DLE`]); si no, un directorio con un BEL dentro
/// habría terminado el marcador a mitad y el panel habría seguido al shell a
/// un sitio en el que el shell no está.
pub const CWD_MARKER_END: u8 = 0x07;

/// El byte de escape del PAYLOAD del marcador (`DLE`, «data link escape»,
/// que es literalmente para lo que se inventó).
///
/// El shell lo dobla (`DLE DLE` = un `DLE` de verdad) y convierte el BEL en
/// `DLE G`. Cualquier otra pareja es un marcador MALFORMADO y se descarta
/// entero: mejor no seguir al shell que seguirlo a medias.
pub const DLE: u8 = 0x10;

/// Tope de lo que se guarda esperando el final de un marcador.
///
/// Sin él, un flujo que contenga el prefijo y JAMÁS un BEL —un binario
/// `cat`-eado, un `printf` del propio lector— dejaba de pintarse entero: cada
/// byte que el shell escribiera desde ese momento se acumulaba en la cola,
/// sin límite y sin llegar nunca a la pantalla. El shell parecía colgado y la
/// memoria de norte subía a la velocidad del pty.
///
/// Pasado el tope no era un marcador, así que se pinta como lo que es: texto.
pub const MARKER_MAX: usize = 8 * 1024;

/// El comando que cede la terminal al subshell, y que también la pide de
/// vuelta.
pub const TOGGLE_COMMAND: &str = "app.toggle-panels";

/// El nonce de la sesión: lo que distingue al shell de norte de cualquier otra
/// cosa que escriba en el mismo pty.
///
/// El flujo del subshell lo controla, en parte, quien no debería: un fichero
/// llamado `…\e]777;norte-cwd;/etc\a…` y un `ls` bastan para que el marcador
/// aparezca sin que ningún prompt lo haya impreso. Y el marcador no se pinta
/// —se OBEDECE—, así que sin autenticar era una forma de mover el panel del
/// lector a un directorio elegido por otro, justo antes de que apunte ahí un
/// copiar o un borrar.
///
/// El nonce se genera al arrancar el shell y se le teclea dentro del gancho,
/// así que el shell lo sabe y un fichero no lo puede adivinar. Vive en el
/// scrollback del propio lector, que es exactamente el sitio donde da igual:
/// quien pueda leer su terminal ya ha ganado.
///
/// ```
/// use norte_frontend::subshell::Nonce;
///
/// let a = Nonce::new();
/// let b = Nonce::new();
/// assert_ne!(a.as_str(), b.as_str(), "dos sesiones, dos nonces");
/// assert!(a.as_str().chars().all(|c| c.is_ascii_hexdigit()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nonce(String);

impl Nonce {
    /// Uno nuevo, de 128 bits en hexadecimal.
    ///
    /// El azar sale de `RandomState`, que es el que la biblioteca estándar usa
    /// para sembrar sus `HashMap` — no hay dependencia que justificar (regla 8)
    /// y no hace falta calidad criptográfica: lo que tiene que ser es
    /// imposible de adivinar para un fichero que se escribió ANTES de que esta
    /// sesión existiera.
    #[must_use]
    pub fn new() -> Self {
        use std::hash::BuildHasher as _;
        let s = std::collections::hash_map::RandomState::new();
        Self(format!(
            "{:016x}{:016x}",
            s.hash_one(0u64),
            s.hash_one(1u64)
        ))
    }

    /// El nonce como texto: hexadecimal ASCII, apto para teclear en un shell.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Nonce {
    fn default() -> Self {
        Self::new()
    }
}

/// Lo que hay que teclearle al subshell nada más arrancarlo: el gancho del
/// prompt y la función que ejecuta los `cd`.
///
/// Se manda por la ENTRADA, como si el lector lo hubiera tecleado: no se toca
/// ningún fichero de configuración suyo. Un `~/.bashrc` que norte editara sería
/// una modificación permanente por una función que se apaga al salir — y
/// sobreviviría a un norte que ya no está.
///
/// Cada línea va con un ESPACIO delante, y lo primero que se pide es que el
/// shell ignore lo que empiece por espacio: así el historial del lector no se
/// llena de fontanería suya. Solo esa primera línea queda grabada (fish ya
/// ignora el espacio de fábrica, así que ahí no queda ninguna).
///
/// TODO lo que sale de aquí es ASCII imprimible, por la regla del módulo: los
/// bytes de control del marcador se escriben como escapes de `printf` (`\033`,
/// `\a`), no como bytes.
///
/// ```
/// use norte_frontend::shell::Shell;
/// use norte_frontend::subshell::{Nonce, install};
///
/// let n = Nonce::new();
/// let texto = install(Shell::Bash, &n, std::path::Path::new("/run/user/1000/norte/b.cd"));
/// assert!(texto.contains("PROMPT_COMMAND"));
/// // Ni un solo byte de control salvo los saltos de línea que envían cada
/// // orden: lo demás lo interpretaría el editor de línea, no el shell.
/// assert!(texto.bytes().all(|b| b == b'\n' || (0x20..0x7f).contains(&b)));
/// ```
#[must_use]
pub fn install(shell: Shell, nonce: &Nonce, buzon: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let (pre, n) = (marker_format_prefix(), nonce.as_str());
    // La ruta del buzón entra ESCAPADA en octal, como el destino de un `cd`
    // entraba antes: es una ruta del sistema y puede llevar cualquier byte,
    // incluidas comillas. Lo que se manda por el pty no puede llevar ni un
    // byte de control (lo interpretaría el editor de línea), así que va como
    // formato de `printf` y el shell la reconstruye.
    let mut buzon_esc = String::new();
    for b in buzon.as_os_str().as_bytes() {
        use std::fmt::Write as _;
        // El `write!` a un `String` no puede fallar; el `let _` lo dice sin
        // gastar un `expect` (regla 6).
        let _ = write!(buzon_esc, "\\{b:03o}");
    }
    // El cuerpo del gancho: escapa el DLE, luego el BEL, e imprime.
    // `%s` y no `$PWD` interpolado en el formato: un directorio que se llame
    // `%d` no es un especificador de formato, es un directorio.
    match shell {
        Shell::Bash | Shell::Zsh => {
            let cuerpo = concat!(
                "local p=${PWD//$'\\020'/$'\\020\\020'}; ",
                "p=${p//$'\\a'/$'\\020'G}; "
            );
            // El `cd` va DENTRO del gancho y antes de anunciar el directorio:
            // así el marcador dice dónde se ha quedado el shell, no dónde
            // estaba. Una función y un registro en vez de dos, que además
            // borra la invariante de orden que hacía falta cuando eran dos.
            //
            // `${d%_}` quita el centinela: `$(<f)` se come los saltos de línea
            // del final y un directorio PUEDE acabar en uno.
            let mover = format!(
                "local f d; f=$(printf '{buzon_esc}'); \
                 if [ -s \"$f\" ]; then d=$(<\"$f\"); : > \"$f\"; cd -- \"${{d%_}}\" || true; fi; "
            );
            let hook =
                format!(" __norte_cwd() {{ {mover}{cuerpo}printf '{pre}{n};%s\\a' \"$p\"; }}\n");
            match shell {
                Shell::Zsh => format!(
                    " setopt hist_ignore_space 2>/dev/null\n{hook} \
                     precmd_functions+=(__norte_cwd)\n"
                ),
                // `PROMPT_COMMAND` se ACUMULA con lo que hubiera: el prompt del
                // lector es suyo, y sustituirlo le quitaría el git-status que
                // tenga puesto. Y desde bash 5.1 puede ser un ARRAY —
                // `PROMPT_COMMAND=(__vte_prompt_command)` es lo que trae GNOME
                // Terminal—, donde la concatenación de cadenas pisa el elemento
                // 0 y se lleva por delante el resto en silencio.
                _ => format!(
                    " HISTCONTROL=ignorespace:${{HISTCONTROL}}\n{hook} \
                     if [[ ${{PROMPT_COMMAND@a}} == *a* ]]; \
                     then PROMPT_COMMAND+=(__norte_cwd); \
                     else PROMPT_COMMAND=\"__norte_cwd${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}\"; fi\n"
                ),
            }
        }
        // fish no tiene `PROMPT_COMMAND`: el gancho es un evento, que además
        // es lo que fish documenta para esto y no toca `fish_prompt`, que es
        // del lector. `string collect` conserva los saltos de línea de dentro
        // de un nombre, que la sustitución de comandos partiría en argumentos.
        //
        // Una sola función, igual que en los otros dos: el `cd` va dentro y
        // antes del anuncio. Cuando eran dos, el orden en que se definían era
        // una invariante que había que recordar —en fish el gancho se registra
        // al definirlo, así que al revés el primer marcador salía con la línea
        // del `cd` todavía sin consumir—. Ya no hay orden que recordar.
        Shell::Fish => format!(
            " function __norte_cwd --on-event fish_prompt; \
             set -l f (printf '{buzon_esc}' | string collect); \
             if test -s \"$f\"; \
             set -l d (cat -- \"$f\" | string collect); \
             printf '' > \"$f\"; \
             cd -- (string sub -s 1 -e -1 -- \"$d\") 2>/dev/null; end; \
             set -l p (string replace -a -- \\x10 \\x10\\x10 $PWD | \
             string replace -a -- \\a \\x10G | string collect); \
             printf '{pre}{n};%s\\a' \"$p\"; end\n"
        ),
    }
}

/// El prefijo del marcador escrito como lo entiende `printf`, sin un solo byte
/// de control: `\033]777;norte-cwd;`.
fn marker_format_prefix() -> String {
    let mut s = String::from("\\033");
    s.push_str(&CWD_MARKER_PREFIX[1..]);
    s
}

/// Lo que devuelve [`scan_cwd`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// El flujo SIN los marcadores: lo que hay que pintar.
    pub visible: Vec<u8>,
    /// El último cwd anunciado y AUTENTICADO, en bytes (regla 1).
    pub cwd: Option<Vec<u8>>,
    /// Un marcador partido entre dos lecturas, esperando su final.
    pub tail: Vec<u8>,
}

/// Saca del flujo del subshell los marcadores completos y devuelve el ÚLTIMO
/// cwd anunciado, junto con lo que hay que PINTAR (el flujo sin ellos).
///
/// Un marcador cuyo nonce no sea el de `nonce` se DESCARTA —no lo imprimió el
/// gancho de esta sesión— pero se saca del flujo igual: pintarlo sería
/// enseñarle al lector la secuencia de escape que alguien le coló.
///
/// Devuelve también la cola sin terminar: un marcador puede partirse entre dos
/// lecturas del pty —lo normal cuando llega en el mismo bloque que un prompt
/// largo— y quien llama la vuelve a poner delante del siguiente trozo. Sin
/// eso, un cwd se pierde cada vez que el buffer cae en medio. La cola está
/// ACOTADA por [`MARKER_MAX`].
///
/// Los bytes del cwd viajan TAL CUAL: un directorio no tiene por qué ser UTF-8
/// (regla 1), y lo que sale de aquí es lo que el shell imprimió.
///
/// ```
/// use norte_frontend::subshell::{scan_cwd, Nonce, CWD_MARKER_PREFIX};
///
/// let n = Nonce::new();
/// let flujo = format!("hola{CWD_MARKER_PREFIX}{};/tmp\x07adios", n.as_str());
/// let s = scan_cwd(flujo.as_bytes(), &n);
/// assert_eq!(s.visible, b"holaadios");
/// assert_eq!(s.cwd.as_deref(), Some(&b"/tmp"[..]));
/// assert!(s.tail.is_empty());
/// ```
#[must_use]
pub fn scan_cwd(bytes: &[u8], nonce: &Nonce) -> Scan {
    let pre = CWD_MARKER_PREFIX.as_bytes();
    let mut out = Scan {
        visible: Vec::with_capacity(bytes.len()),
        ..Scan::default()
    };
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i..].starts_with(pre) {
            // Un prefijo A MEDIAS al final del trozo es cola, no texto: si se
            // pintara, el marcador saldría por pantalla la vez que el buffer
            // cayera dentro de él.
            if pre.starts_with(&bytes[i..]) {
                out.tail = bytes[i..].to_vec();
                return out;
            }
            out.visible.push(bytes[i]);
            i += 1;
            continue;
        }
        let desde = i + pre.len();
        match bytes[desde..].iter().position(|b| *b == CWD_MARKER_END) {
            Some(fin) => {
                if let Some(dir) = payload_cwd(&bytes[desde..desde + fin], nonce) {
                    out.cwd = Some(dir);
                }
                i = desde + fin + 1;
            }
            // Marcador empezado y sin terminar: cola, mientras quepa. Pasado
            // el tope no era un marcador — se pinta y se sigue.
            None if bytes.len() - i <= MARKER_MAX => {
                out.tail = bytes[i..].to_vec();
                return out;
            }
            None => {
                out.visible.extend_from_slice(&bytes[i..]);
                return out;
            }
        }
    }
    out
}

/// Lo que hay que CONTESTARLE al shell si preguntó algo que un terminal
/// contesta.
///
/// Del otro lado de ese pty, el terminal es norte. Un shell moderno no da por
/// hecho lo que tiene delante: lo PREGUNTA y ESPERA la respuesta. fish 4
/// manda `ESC [ c` (Device Attributes) y `ESC [ ? u` (¿soportas el protocolo
/// de teclado de kitty?) antes de pintar su primer prompt, y sin respuesta se
/// queda ahí — el lector veía un shell colgado, sin prompt y sin explicación.
/// Medido: contestando esas dos, fish arranca; sin ellas, no arranca en diez
/// segundos.
///
/// Las respuestas son deliberadamente POBRES: «un VT100 con opciones» y «no
/// hablo kitty». Prometer capacidades que luego no se cumplen es peor que no
/// prometerlas, y aquí ni siquiera hay un terminal detrás mientras los paneles
/// están delante — lo que el shell escriba entonces se guarda en un buffer, no
/// lo pinta nadie.
///
/// No se contestan la posición del cursor (`ESC [ 6 n`) ni el color de fondo
/// (`OSC 11`): fish llega a su prompt sin ellas, y contestarlas mal es
/// inventarse un dato que el programa va a usar para colocar cosas.
///
/// ```
/// use norte_frontend::subshell::terminal_reply;
///
/// assert_eq!(terminal_reply(b"hola\x1b[c"), Some(b"\x1b[?1;2c".to_vec()));
/// assert_eq!(terminal_reply(b"\x1b[0c"), Some(b"\x1b[?1;2c".to_vec()));
/// assert_eq!(terminal_reply(b"\x1b[?u"), Some(b"\x1b[?0u".to_vec()));
/// assert_eq!(terminal_reply(b"ls -la\n"), None);
/// ```
#[must_use]
pub fn terminal_reply(salida: &[u8]) -> Option<Vec<u8>> {
    let mut respuesta = Vec::new();
    for (consulta, contesta) in [
        (&b"\x1b[c"[..], &b"\x1b[?1;2c"[..]),
        (&b"\x1b[0c"[..], &b"\x1b[?1;2c"[..]),
        (&b"\x1b[?u"[..], &b"\x1b[?0u"[..]),
    ] {
        if salida
            .windows(consulta.len())
            .any(|w| w == consulta)
            // `ESC [ c` es sufijo de `ESC [ 0 c`: contestar las dos daría
            // respuesta doble a una sola pregunta.
            && !respuesta.windows(contesta.len()).any(|w| w == contesta)
        {
            respuesta.extend_from_slice(contesta);
        }
    }
    (!respuesta.is_empty()).then_some(respuesta)
}

/// El cwd de un payload `<nonce>;<ruta escapada>`, si el nonce es el nuestro y
/// los escapes están bien.
fn payload_cwd(payload: &[u8], nonce: &Nonce) -> Option<Vec<u8>> {
    let corte = payload.iter().position(|b| *b == b';')?;
    if &payload[..corte] != nonce.as_str().as_bytes() {
        return None;
    }
    let mut dir = Vec::with_capacity(payload.len() - corte);
    let cuerpo = &payload[corte + 1..];
    let mut i = 0;
    while i < cuerpo.len() {
        if cuerpo[i] != DLE {
            dir.push(cuerpo[i]);
            i += 1;
            continue;
        }
        match cuerpo.get(i + 1) {
            Some(&DLE) => dir.push(DLE),
            Some(b'G') => dir.push(CWD_MARKER_END),
            // Escape que el gancho no puede haber producido: el payload no es
            // de fiar entero, no solo ese byte.
            _ => return None,
        }
        i += 2;
    }
    // Una ruta RELATIVA no puede venir del gancho —`$PWD` es absoluto—, y
    // seguirla dejaría que `std::path::absolute` la resolviera contra el cwd
    // del proceso norte, que no tiene nada que ver.
    (dir.first() == Some(&b'/')).then_some(dir)
}

/// Los bytes que un acorde le manda a un shell, o `None` si ahí no significa
/// nada.
///
/// **Vive aquí porque la necesitan los DOS frontends**, y sobre
/// [`Chord`](crate::keymap::Chord) y no sobre el evento de ningún toolkit por
/// lo mismo: la terminal lo construye desde `crossterm` y la ventana desde lo
/// que manda el renderer, pero la tabla es una — qué byte es `Ctrl+C` no
/// depende de quién lo vio. Escribirla dos veces es tener dos sitios donde
/// `F10` deja de salir de un `htop`.
///
/// Lo que NO está aquí, y se nota: ratón y pegado con corchetes. Un shell no
/// los pide, y la alternativa en la terminal era robarle una tecla al lector.
///
/// ```
/// use norte_frontend::keymap::{Chord, KeyCode, Mods};
/// use norte_frontend::subshell::chord_a_bytes;
///
/// let ctrl_c = Chord::new(Mods { ctrl: true, ..Mods::default() }, KeyCode::Char('c'));
/// // Ctrl+C viaja como el byte 3, que es lo que hace que interrumpa.
/// assert_eq!(chord_a_bytes(ctrl_c), Some(vec![3]));
/// // Enter es CR y no LF: es lo que manda un terminal.
/// assert_eq!(chord_a_bytes(Chord::new(Mods::default(), KeyCode::Enter)), Some(b"\r".to_vec()));
/// ```
#[must_use]
pub fn chord_a_bytes(chord: crate::keymap::Chord) -> Option<Vec<u8>> {
    use crate::keymap::KeyCode;
    let (mods, code) = chord.parts();
    // Un acorde con Cmd/Super NO se le manda a un shell: no hay codificación
    // de terminal para ese modificador, así que lo que salía era la letra
    // pelada — en macOS, `cmd+w` escribía una `w` en vez de cerrar la ventana.
    // Devolviendo `None` la tecla sigue su camino y la resuelve el keymap.
    if mods.cmd {
        return None;
    }
    let cuerpo: Vec<u8> = match code {
        // Ctrl+letra es el byte de control de toda la vida: `a`→1, `c`→3.
        // Sin esto, un Ctrl+C dentro del panel no interrumpe nada.
        KeyCode::Char(c) if mods.ctrl && c.is_ascii_alphabetic() => {
            vec![(c.to_ascii_lowercase() as u8) - b'a' + 1]
        }
        // Los OTROS acordes de control, que también son bytes y no letras:
        // Ctrl+\ es SIGQUIT, Ctrl+espacio es el NUL con el que `readline` pone
        // la marca, Ctrl+[ es Escape. Sin esta rama viajaba el carácter tal
        // cual, así que Ctrl+\ mandaba una barra invertida.
        KeyCode::Char(c)
            if mods.ctrl && matches!(c, '@' | ' ' | '[' | '\\' | ']' | '^' | '_' | '?') =>
        {
            vec![match c {
                '@' | ' ' => 0,
                '?' => 0x7f,
                otro => (otro as u8) & 0x1f,
            }]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        // Las teclas de función, que un `htop` o un editor dentro del panel sí
        // usan: sin esto, F10 no salía de nada. Códigos xterm, que son los que
        // `terminfo` da para `xterm`/`screen`/`tmux`.
        KeyCode::F(1) => b"\x1bOP".to_vec(),
        KeyCode::F(2) => b"\x1bOQ".to_vec(),
        KeyCode::F(3) => b"\x1bOR".to_vec(),
        KeyCode::F(4) => b"\x1bOS".to_vec(),
        // El salto 16, 22 no es un error: xterm nunca los asignó.
        KeyCode::F(n @ 5..=12) => {
            let num = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n) - 5];
            format!("\x1b[{num}~").into_bytes()
        }
        KeyCode::F(_) => return None,
        KeyCode::Enter => b"\r".to_vec(),
        // Shift+Tab es `CSI Z`, que es lo que un shell espera para ir hacia
        // atrás en un completado.
        KeyCode::Tab if mods.shift => b"\x1b[Z".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
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
    };
    // Alt es ESC delante, que es lo que hace que `alt+f` mueva una palabra.
    if mods.alt {
        let mut con_esc = vec![0x1b];
        con_esc.extend_from_slice(&cuerpo);
        return Some(con_esc);
    }
    Some(cuerpo)
}

/// El acorde con el que el lector RECUPERA los paneles.
///
/// Es el mismo que se los quitó, y por eso sale del keymap y no de una
/// constante: los presets no lo atan igual (`norton` y `far` lo ponen en
/// `Ctrl+O`, otro preset puede moverlo) y un `Ctrl+O` cableado dejaría al
/// lector dentro del shell sin forma de volver — con los paneles vivos detrás
/// de una pantalla que no responde.
///
/// Solo un acorde SUELTO sirve. Una secuencia de dos —`g` `s`, digamos— no se
/// puede reconocer aquí sin meter el resolutor entero dentro del bucle del
/// pty, y sobre todo no se DEBE: la primera tecla de la secuencia se le
/// tendría que robar al shell, que es justo donde el lector la está tecleando.
/// Si el preset ata el toggle a una secuencia, esto devuelve `None` y quien
/// llama no cede la terminal, en vez de cederla sin salida.
///
/// Si el preset lo ata a DOS acordes sueltos, manda el ÚLTIMO: es el que gana
/// en el keymap efectivo, así que es el que el lector tiene en la hoja de
/// referencia. Entrar por el otro y no poder salir por él sería justo lo que
/// esta función existe para impedir, pero eso no lo puede arreglar aquí quien
/// solo ve un acorde: lo dice la ayuda.
///
/// ```
/// use norte_frontend::keymap::{Effective, Screen, parse_keymap};
/// use norte_frontend::subshell::{TOGGLE_COMMAND, detach_chord};
///
/// let src = r#"
/// [global]
/// keymap = [{ on = ["ctrl+o"], run = "app.toggle-panels" }]
/// "#;
/// let preset = parse_keymap(src).unwrap();
/// let eff =
///     Effective::build_for(&preset, &[], &[TOGGLE_COMMAND], Screen::Browse).unwrap();
/// assert!(detach_chord(&eff).is_some());
/// ```
#[must_use]
pub fn detach_chord(browse: &crate::keymap::Effective) -> Option<crate::keymap::Chord> {
    // La regla —un acorde SUELTO o no se cede el teclado— vive en
    // `Effective::lone_chord`, porque el panel de terminal (#362) la necesita
    // igual: los dos le entregan el teclado entero a otro programa y los dos
    // se quedan con un solo acorde para volver.
    browse.lone_chord(TOGGLE_COMMAND)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{Effective, Screen, parse_keymap};

    fn nonce() -> Nonce {
        Nonce::new()
    }

    /// Un marcador con el payload YA escapado, como lo imprimiría el gancho.
    fn marcador(n: &Nonce, dir: &[u8]) -> Vec<u8> {
        let mut v = CWD_MARKER_PREFIX.as_bytes().to_vec();
        v.extend_from_slice(n.as_str().as_bytes());
        v.push(b';');
        for b in dir {
            match *b {
                DLE => v.extend_from_slice(&[DLE, DLE]),
                CWD_MARKER_END => v.extend_from_slice(&[DLE, b'G']),
                otro => v.push(otro),
            }
        }
        v.push(CWD_MARKER_END);
        v
    }

    /// **Nada de lo que norte teclea lleva un byte de control.**
    ///
    /// Es LA regla del módulo, y el test que la sostiene. Antes el gancho
    /// llevaba el `ESC` y el `BEL` crudos: readline se comía el `ESC ]` como
    /// prefijo meta, así que el `PROMPT_COMMAND` que quedaba instalado
    /// imprimía `777;norte-cwd;/casa` SIN el marco OSC — el panel no seguía al
    /// shell jamás, y el lector veía ese texto en cada prompt.
    #[test]
    fn norte_jamas_teclea_un_byte_de_control() {
        let n = nonce();
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            // Una ruta de buzón con bytes hostiles: va por el mismo camino
            // de escapes octales que iba el destino de un `cd`.
            let texto = install(shell, &n, std::path::Path::new("/tmp/\x15\x01'b"));
            assert!(
                texto
                    .bytes()
                    .all(|b| b == b'\n' || (0x20..0x7f).contains(&b)),
                "{shell:?}: el gancho lleva un byte que el editor de línea ejecutaría"
            );
        }
    }

    /// **El gancho hace el `cd` ANTES de anunciar el directorio.**
    ///
    /// Es una sola función y ese orden es su invariante: si anunciara primero,
    /// el marcador diría dónde estaba el shell y no dónde se ha quedado, y el
    /// panel se quedaría un prompt por detrás de sí mismo.
    ///
    /// Antes eran dos —una para el `cd`, otra para anunciar— y el orden en que
    /// se DEFINÍAN era la invariante: en fish el gancho se registra al
    /// definirlo, así que al revés el primer marcador salía con la línea del
    /// `cd` todavía sin consumir. Con una función esa invariante desaparece.
    #[test]
    fn el_gancho_mueve_antes_de_anunciar() {
        let n = nonce();
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let texto = install(shell, &n, std::path::Path::new("/tmp/buzon"));
            let mueve = texto.find("cd --").expect("el gancho mueve");
            let anuncia = texto.find("777;norte-cwd;").expect("el gancho anuncia");
            assert!(
                mueve < anuncia,
                "{shell:?}: anuncia el directorio antes de haberse movido"
            );
            assert_eq!(
                texto.matches("--on-event fish_prompt").count()
                    + texto.matches("precmd_functions").count()
                    + texto.matches("PROMPT_COMMAND+=").count(),
                usize::from(shell != Shell::Bash) + usize::from(shell == Shell::Bash),
                "{shell:?}: un solo registro, no dos"
            );
        }
    }

    /// **El gancho no teclea ningún `cd`: lo recoge de un fichero** (#363).
    ///
    /// Teclearlo exigía saber que el editor de línea estaba en un prompt
    /// vacío, y eso no se puede saber desde fuera: la pila de buffers de zsh
    /// (`push-line`, `print -z`) y el type-ahead de los tres llegaban a
    /// «marcador recibido y nadie escribió» con una línea a medias del lector
    /// esperando. El `cd` se concatenaba y el shell EJECUTABA una orden que
    /// nadie dio.
    #[test]
    fn el_gancho_recoge_el_destino_de_un_fichero_y_no_lo_teclea() {
        let n = nonce();
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let texto = install(shell, &n, std::path::Path::new("/run/u/norte/b.cd"));
            assert!(
                !texto.contains("__norte_cd"),
                "{shell:?}: sigue existiendo la función que se tecleaba"
            );
            // La ruta del buzón va escapada en octal, como iba el destino:
            // `/` es `\057`, y su presencia dice que el gancho la lleva.
            assert!(
                texto.contains("\\057\\162\\165\\156"),
                "{shell:?}: el gancho no lleva la ruta del buzón: {texto}"
            );
        }
    }

    /// El gancho pide el marcador ENTERO, prefijo OSC incluido: si el `\033`
    /// se perdiera, `scan_cwd` no reconocería nada.
    #[test]
    fn el_gancho_imprime_el_marcador_entero() {
        let n = nonce();
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let texto = install(shell, &n, std::path::Path::new("/tmp/buzon"));
            assert!(
                texto.contains("\\033]777;norte-cwd;"),
                "{shell:?}: sin el prefijo OSC no hay marcador"
            );
            assert!(texto.contains(n.as_str()), "{shell:?}: sin nonce");
        }
    }

    /// El gancho de bash CONSERVA el `PROMPT_COMMAND` que hubiera, y sabe que
    /// desde 5.1 puede ser un ARRAY: `PROMPT_COMMAND=(__vte_prompt_command)`
    /// es lo que trae GNOME Terminal, y la asignación de cadena habría pisado
    /// el elemento 0 llevándose el resto por delante sin decir nada.
    #[test]
    fn el_gancho_de_bash_no_pisa_el_prompt_del_lector() {
        let texto = install(Shell::Bash, &nonce(), std::path::Path::new("/tmp/buzon"));
        assert!(texto.contains("${PROMPT_COMMAND:+;$PROMPT_COMMAND}"));
        assert!(texto.contains("PROMPT_COMMAND+=(__norte_cwd)"));
        assert!(texto.contains("@a"), "sin la prueba de array");
    }

    /// El marcador se saca del flujo y NO se pinta: si se pintara, el lector
    /// vería la secuencia de escape en su shell cada vez que aparece un
    /// prompt.
    #[test]
    fn el_marcador_no_se_pinta() {
        let n = nonce();
        let mut flujo = b"$ ls".to_vec();
        flujo.extend_from_slice(&marcador(&n, b"/casa"));
        flujo.extend_from_slice(b"$ ");
        let s = scan_cwd(&flujo, &n);
        assert_eq!(s.visible, b"$ ls$ ");
        assert_eq!(s.cwd.as_deref(), Some(&b"/casa"[..]));
        assert!(s.tail.is_empty());
    }

    /// **Un marcador partido entre dos lecturas no se pierde ni se pinta.**
    ///
    /// Es el caso normal, no el raro: el pty entrega lo que hay, y un prompt
    /// largo cae a mitad del marcador constantemente. Sin la cola, el cwd se
    /// perdía y el escape salía por pantalla.
    #[test]
    fn un_marcador_partido_se_reconstruye() {
        let n = nonce();
        let mut entero = b"antes".to_vec();
        entero.extend_from_slice(&marcador(&n, b"/casa/mia"));
        entero.extend_from_slice("despu\u{e9}s".as_bytes());
        for corte in 1..entero.len() {
            let a = scan_cwd(&entero[..corte], &n);
            let mut segundo = a.tail;
            segundo.extend_from_slice(&entero[corte..]);
            let b = scan_cwd(&segundo, &n);
            let mut visible = a.visible;
            visible.extend_from_slice(&b.visible);
            assert_eq!(visible, b"antesdespu\xc3\xa9s", "corte {corte}");
            assert_eq!(
                b.cwd.or(a.cwd).as_deref(),
                Some(&b"/casa/mia"[..]),
                "corte {corte}"
            );
            assert!(b.tail.is_empty(), "corte {corte}");
        }
    }

    /// El cwd viaja en BYTES: un directorio no tiene por qué ser texto
    /// (regla 1), y pasarlo por `String` cambiaría cuál es.
    #[test]
    fn un_cwd_que_no_es_utf8_sobrevive() {
        let n = nonce();
        let s = scan_cwd(&marcador(&n, b"/casa/a\xffb"), &n);
        assert_eq!(s.cwd.as_deref(), Some(&b"/casa/a\xffb"[..]));
    }

    /// **Un BEL dentro del nombre no corta el marcador.**
    ///
    /// BEL es un byte legal en un nombre de fichero y es el terminador que
    /// norte eligió: sin escaparlo, `/tmp/a\x07b` se anunciaba como `/tmp/a` y
    /// el panel seguía al shell a un directorio en el que el shell no estaba —
    /// en silencio, si `/tmp/a` existía.
    #[test]
    fn un_bel_en_el_nombre_no_trunca_el_marcador() {
        let n = nonce();
        for dir in [&b"/tmp/a\x07b"[..], &b"/tmp/\x10\x07\x10"[..]] {
            let s = scan_cwd(&marcador(&n, dir), &n);
            assert_eq!(s.cwd.as_deref(), Some(dir), "{dir:?}");
        }
    }

    /// **Un marcador que no lleva el nonce de esta sesión no mueve nada.**
    ///
    /// El flujo del pty lo controla en parte quien no debería: basta un
    /// fichero con el marcador dentro y un `cat`. Sin autenticar, eso movía el
    /// panel del lector a un directorio elegido por otro justo antes de que
    /// apunte ahí un copiar o un borrar.
    #[test]
    fn un_marcador_falsificado_no_mueve_el_panel() {
        let n = nonce();
        let otro = Nonce("0".repeat(32));
        let s = scan_cwd(&marcador(&otro, b"/etc"), &n);
        assert_eq!(s.cwd, None, "el nonce no era el nuestro");
        // Y NO se pinta: enseñarle al lector la secuencia que le colaron sería
        // la otra mitad del problema.
        assert!(s.visible.is_empty());
    }

    /// Una ruta RELATIVA no viene de `$PWD`, y seguirla la resolvería contra
    /// el cwd del proceso norte — otro directorio distinto, elegido por nadie.
    #[test]
    fn una_ruta_relativa_no_se_sigue() {
        let n = nonce();
        assert_eq!(scan_cwd(&marcador(&n, b"etc"), &n).cwd, None);
        assert_eq!(scan_cwd(&marcador(&n, b""), &n).cwd, None);
    }

    /// **Un marcador que nunca termina no se traga el flujo entero.**
    ///
    /// Sin tope, el primer `\e]777;norte-cwd;` sin BEL detrás —un binario
    /// `cat`-eado— dejaba de pintar TODO lo que el shell escribiera desde ese
    /// momento, acumulándolo sin límite: el shell parecía colgado y la memoria
    /// subía a la velocidad del pty.
    #[test]
    fn un_marcador_sin_final_no_se_come_la_pantalla() {
        let n = nonce();
        let mut flujo = CWD_MARKER_PREFIX.as_bytes().to_vec();
        flujo.extend(std::iter::repeat_n(b'x', MARKER_MAX + 10));
        let s = scan_cwd(&flujo, &n);
        assert!(s.tail.is_empty(), "pasado el tope no se retiene nada");
        assert_eq!(
            s.visible.len(),
            flujo.len(),
            "y se pinta como el texto que es"
        );
    }

    /// Solo el ÚLTIMO cuenta: entre dos lecturas puede haber pasado más de un
    /// prompt, y el sitio donde está el shell es el del final.
    #[test]
    fn manda_el_ultimo_marcador() {
        let n = nonce();
        let mut flujo = marcador(&n, b"/uno");
        flujo.push(b'x');
        flujo.extend_from_slice(&marcador(&n, b"/dos"));
        let s = scan_cwd(&flujo, &n);
        assert_eq!(s.visible, b"x");
        assert_eq!(s.cwd.as_deref(), Some(&b"/dos"[..]));
    }

    /// **La ruta del BUZÓN no deja escapar una orden, y la defensa NO son las
    /// comillas.**
    ///
    /// Antes esto probaba el `cd` que norte tecleaba. Desde #363 no se teclea
    /// ninguno, pero la ruta del buzón viaja por el mismo camino y dentro del
    /// gancho, así que la propiedad es la misma y hace falta igual: es una
    /// ruta del sistema y puede llevar cualquier byte.
    ///
    /// Los nombres hostiles corrientes los pararía el entrecomillado. Los tres
    /// del final no: son bytes que readline EJECUTA (`0x15` borra la línea,
    /// `0x01` salta al principio, `0x7f` borra hacia atrás), así que nunca
    /// llegan al parser que la comilla protege. La defensa es que la ruta
    /// viaja en octal.
    #[test]
    fn la_ruta_del_buzon_no_deja_escapar_una_orden() {
        // El CORPUS canónico, no una lista inventada aquí (convención del
        // repo): las tres fixtures `readline_*` entraron por esto, y una lista
        // local habría dejado el fallo fuera del sitio donde el resto de norte
        // lo busca.
        let mut nombres: Vec<Vec<u8>> = norte_testkit::corpus::hostile_names()
            .into_iter()
            .map(|n| {
                let mut ruta = b"/tmp/".to_vec();
                ruta.extend_from_slice(&n.bytes);
                ruta
            })
            .collect();
        nombres.extend(
            [
                &b"/tmp/; rm -rf ~"[..],
                &b"/tmp/$(whoami)"[..],
                &b"/tmp/`id`"[..],
            ]
            .into_iter()
            .map(<[u8]>::to_vec),
        );
        let n = nonce();
        for nombre in &nombres {
            use std::os::unix::ffi::OsStrExt as _;
            let buzon = std::path::Path::new(std::ffi::OsStr::from_bytes(nombre));
            for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
                let texto = install(shell, &n, buzon);
                assert!(
                    texto
                        .bytes()
                        .all(|b| b == b'\n' || (0x20..0x7f).contains(&b)),
                    "{nombre:?} viajó con un byte que el editor de línea ejecuta"
                );
                // Y los bytes de la ruta están TODOS ahí, en octal y en orden.
                let mut esperado = String::new();
                for b in nombre {
                    use std::fmt::Write as _;
                    write!(esperado, "\\{b:03o}").expect("String no falla");
                }
                assert!(texto.contains(&esperado), "{nombre:?} en {shell:?}");
            }
        }
    }

    fn browse(src: &str) -> Effective {
        let preset = parse_keymap(src).expect("el fixture parsea");
        Effective::build_for(&preset, &[], &[TOGGLE_COMMAND], Screen::Browse)
            .expect("el fixture construye")
    }

    /// Una SECUENCIA no vale como acorde de vuelta, y decirlo aquí es lo que
    /// evita que el frontend ceda la terminal sin salida: la primera tecla de
    /// la secuencia es del shell, no de norte.
    #[test]
    fn una_secuencia_no_sirve_para_volver() {
        let eff = browse(
            r#"
[global]
keymap = [{ on = ["g", "s"], run = "app.toggle-panels" }]
"#,
        );
        assert_eq!(detach_chord(&eff), None);
    }

    /// Y un preset que no lo ata en absoluto tampoco: no hay tecla, así que
    /// no hay nada que ceder.
    #[test]
    fn sin_binding_no_hay_acorde() {
        let eff = browse("[global]\nkeymap = []\n");
        assert_eq!(detach_chord(&eff), None);
    }

    /// El acorde sale del KEYMAP, no de una constante: los presets no lo atan
    /// igual, y `Ctrl+O` cableado dejaría al lector encerrado en el shell del
    /// preset que lo mueva.
    #[test]
    fn el_acorde_sale_del_keymap_rebindeado() {
        let eff = browse(
            r#"
[global]
keymap = [{ on = ["ctrl+u"], run = "app.toggle-panels" }]
"#,
        );
        let acorde = detach_chord(&eff).expect("hay acorde");
        assert_eq!(
            acorde,
            crate::keymap::parse_chord("ctrl+u").expect("acorde")
        );
    }

    /// Con DOS acordes sueltos manda el último, que es el que gana en el
    /// efectivo: entrar por uno y no poder salir por él sería justo lo que
    /// `detach_chord` existe para impedir.
    #[test]
    fn con_dos_acordes_manda_el_que_gana_en_el_efectivo() {
        let eff = browse(
            r#"
[global]
keymap = [
    { on = ["ctrl+o"], run = "app.toggle-panels" },
    { on = ["ctrl+u"], run = "app.toggle-panels" },
]
"#,
        );
        let acorde = detach_chord(&eff).expect("hay acorde");
        assert_eq!(
            acorde,
            crate::keymap::parse_chord("ctrl+u").expect("acorde")
        );
    }
}
