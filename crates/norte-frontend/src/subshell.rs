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
/// let texto = install(Shell::Bash, &n);
/// assert!(texto.contains("PROMPT_COMMAND"));
/// // Ni un solo byte de control salvo los saltos de línea que envían cada
/// // orden: lo demás lo interpretaría el editor de línea, no el shell.
/// assert!(texto.bytes().all(|b| b == b'\n' || (0x20..0x7f).contains(&b)));
/// ```
#[must_use]
pub fn install(shell: Shell, nonce: &Nonce) -> String {
    let (pre, n) = (marker_format_prefix(), nonce.as_str());
    // El cuerpo del gancho: escapa el DLE, luego el BEL, e imprime.
    // `%s` y no `$PWD` interpolado en el formato: un directorio que se llame
    // `%d` no es un especificador de formato, es un directorio.
    match shell {
        Shell::Bash | Shell::Zsh => {
            let cuerpo = concat!(
                "local p=${PWD//$'\\020'/$'\\020\\020'}; ",
                "p=${p//$'\\a'/$'\\020'G}; "
            );
            let hook = format!(" __norte_cwd() {{ {cuerpo}printf '{pre}{n};%s\\a' \"$p\"; }}\n");
            // `printf \"$1\"`: el argumento es el FORMATO, y lo que norte manda
            // ahí son escapes octales y nada más. El `_` final es un centinela:
            // `$(...)` se come los saltos de línea del final, y un directorio
            // PUEDE acabar en uno.
            let cd = " __norte_cd() { local d; d=$(printf \"$1\"); cd -- \"${d%_}\"; }\n";
            match shell {
                Shell::Zsh => format!(
                    " setopt hist_ignore_space 2>/dev/null\n{hook}{cd} \
                     precmd_functions+=(__norte_cwd)\n"
                ),
                // `PROMPT_COMMAND` se ACUMULA con lo que hubiera: el prompt del
                // lector es suyo, y sustituirlo le quitaría el git-status que
                // tenga puesto. Y desde bash 5.1 puede ser un ARRAY —
                // `PROMPT_COMMAND=(__vte_prompt_command)` es lo que trae GNOME
                // Terminal—, donde la concatenación de cadenas pisa el elemento
                // 0 y se lleva por delante el resto en silencio.
                _ => format!(
                    " HISTCONTROL=ignorespace:${{HISTCONTROL}}\n{hook}{cd} \
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
        // `__norte_cd` se define ANTES del gancho, y el orden es la invariante
        // de `el_gancho_se_registra_cuando_el_cd_ya_existe`: en fish el gancho
        // se registra al definirlo, así que con el orden contrario el primer
        // marcador podía salir —y dar permiso para teclear el `cd`— con la
        // línea que define `__norte_cd` todavía sin consumir.
        Shell::Fish => format!(
            " function __norte_cd; \
             set -l d (printf \"$argv[1]\" | string collect); \
             cd -- (string sub -s 1 -e -1 -- \"$d\"); end\n \
             function __norte_cwd --on-event fish_prompt; \
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

/// La orden que lleva al subshell a `dir`, en BYTES listos para el pty.
///
/// `None` si el shell no es uno de los que norte sabe preparar (no se le
/// instaló `__norte_cd`, así que teclear el `cd` sería un error de sintaxis
/// impreso en la cara del lector en cada pulsación) o si `dir` lleva un NUL,
/// que ningún nombre de fichero puede tener y que el editor de línea se
/// tragaría en silencio, dejando al shell en una ruta un byte más corta que la
/// que norte cree.
///
/// Todo lo que sale de aquí es ASCII IMPRIMIBLE: la ruta viaja como escapes
/// octales dentro del formato de un `printf`. Ese es el punto entero — ver la
/// regla del módulo. Entrecomillar no valía: los bytes de control no llegan al
/// parser del shell, los ejecuta el editor de línea antes.
///
/// El `\137` final (`_`) es un centinela que la función instalada quita: la
/// sustitución de comandos se come los saltos de línea finales, y un directorio
/// puede acabar en uno.
///
/// ```
/// use norte_frontend::shell::Shell;
/// use norte_frontend::subshell::cd_command;
///
/// let cmd = cd_command(Shell::Bash, b"/tmp").unwrap();
/// assert_eq!(cmd, b" __norte_cd '\\057\\164\\155\\160\\137'\n");
/// // Un nombre que empieza por un byte que readline ejecutaría sale como
/// // dígitos octales, igual que cualquier otro.
/// let malo = cd_command(Shell::Bash, b"/\x15id #").unwrap();
/// assert!(malo.iter().all(|b| *b == b'\n' || (0x20..0x7f).contains(b)));
/// assert!(cd_command(Shell::Bash, b"/tmp/a\0b").is_none());
/// ```
#[must_use]
pub fn cd_command(shell: Shell, dir: &[u8]) -> Option<Vec<u8>> {
    // El shell entra por TIPO y no por nombre, y eso es la mitad de la
    // garantía: `Shell::parse` devuelve `None` para un `nu` o un `elvish`, así
    // que a un shell que norte no sabe preparar no se le teclea nada. Antes se
    // le mandaba el `cd` igual, y cada pulsación le imprimía un error de
    // sintaxis en la cara al lector.
    let (Shell::Bash | Shell::Zsh | Shell::Fish) = shell;
    if dir.contains(&0) {
        return None;
    }
    let mut out = b" __norte_cd '".to_vec();
    for b in dir.iter().chain(std::iter::once(&b'_')) {
        out.extend_from_slice(format!("\\{b:03o}").as_bytes());
    }
    out.extend_from_slice(b"'\n");
    Some(out)
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
    browse
        .bindings_all_seq()
        .into_iter()
        .filter(|(seq, cmd, _)| seq.len() == 1 && *cmd == TOGGLE_COMMAND)
        .map(|(seq, _, _)| seq[0])
        .next_back()
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
            let texto = install(shell, &n);
            assert!(
                texto
                    .bytes()
                    .all(|b| b == b'\n' || (0x20..0x7f).contains(&b)),
                "{shell:?}: el gancho lleva un byte que el editor de línea ejecutaría"
            );
            let cd = cd_command(shell, b"/tmp/\x15\x01\x7f\x1b").expect("shell conocido");
            assert!(
                cd.iter().all(|b| *b == b'\n' || (0x20..0x7f).contains(b)),
                "{shell:?}: el cd lleva un byte que el editor de línea ejecutaría"
            );
        }
    }

    /// **El gancho se registra cuando `__norte_cd` ya existe**, en los tres.
    ///
    /// El marcador es lo que autoriza a teclear el `cd`, y la fontanería entera
    /// se manda de un tirón: en cuanto el gancho está puesto, el primer prompt
    /// lo imprime, y eso puede pasar con las líneas de detrás todavía sin
    /// consumir. Si `__norte_cd` fuera una de ellas, el permiso llegaría antes
    /// que la función. Hoy no se cae —la cola del tty es FIFO, así que el `cd`
    /// se lee después de la definición—, y lo que se perdería si algo vaciara
    /// la cola es un `Unknown command` en la cara del lector, no una orden mal
    /// dirigida. Aun así el orden correcto es gratis, y bash y zsh ya lo tenían
    /// por accidente: registran en la ÚLTIMA línea.
    #[test]
    fn el_gancho_se_registra_cuando_el_cd_ya_existe() {
        let n = nonce();
        for (shell, registro) in [
            (Shell::Bash, "PROMPT_COMMAND"),
            (Shell::Zsh, "precmd_functions"),
            // En fish el gancho SE REGISTRA al definirlo: `--on-event` es el
            // registro, no hay una línea aparte que lo ate.
            (Shell::Fish, "--on-event fish_prompt"),
        ] {
            let texto = install(shell, &n);
            let cd = texto.find("__norte_cd").expect("define el cd");
            let puesto = texto.find(registro).expect("registra el gancho");
            assert!(
                cd < puesto,
                "{shell:?}: el gancho queda puesto antes de que `__norte_cd` exista"
            );
        }
    }

    /// El gancho pide el marcador ENTERO, prefijo OSC incluido: si el `\033`
    /// se perdiera, `scan_cwd` no reconocería nada.
    #[test]
    fn el_gancho_imprime_el_marcador_entero() {
        let n = nonce();
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let texto = install(shell, &n);
            assert!(
                texto.contains("\\033]777;norte-cwd;"),
                "{shell:?}: sin el prefijo OSC no hay marcador"
            );
            assert!(texto.contains(n.as_str()), "{shell:?}: sin nonce");
            assert!(texto.contains("__norte_cd"), "{shell:?}: sin la función cd");
        }
    }

    /// El gancho de bash CONSERVA el `PROMPT_COMMAND` que hubiera, y sabe que
    /// desde 5.1 puede ser un ARRAY: `PROMPT_COMMAND=(__vte_prompt_command)`
    /// es lo que trae GNOME Terminal, y la asignación de cadena habría pisado
    /// el elemento 0 llevándose el resto por delante sin decir nada.
    #[test]
    fn el_gancho_de_bash_no_pisa_el_prompt_del_lector() {
        let texto = install(Shell::Bash, &nonce());
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

    /// **El `cd` no deja escapar una orden, y la defensa NO son las comillas.**
    ///
    /// Los cinco primeros nombres los paraba ya el entrecomillado. Los tres
    /// últimos no: son bytes que readline EJECUTA (`0x15` borra la línea,
    /// `0x01` salta al principio, `0x7f` borra hacia atrás), así que nunca
    /// llegaban al parser que la comilla protege. `/tmp/\x15id #` ejecutaba
    /// `id`. La defensa es que la ruta viaja en octal.
    #[test]
    fn el_cd_no_deja_escapar_una_orden() {
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
        for nombre in &nombres {
            let nombre = nombre.as_slice();
            let cmd = cd_command(Shell::Bash, nombre).expect("shell conocido");
            assert!(
                cmd.iter().all(|b| *b == b'\n' || (0x20..0x7f).contains(b)),
                "{nombre:?} viajó con un byte que el editor de línea ejecuta"
            );
            // Y los bytes de la ruta están TODOS ahí, en octal y en orden.
            let mut esperado = String::new();
            for b in nombre.iter().chain(std::iter::once(&b'_')) {
                use std::fmt::Write as _;
                write!(esperado, "\\{b:03o}").expect("String no falla");
            }
            assert_eq!(
                cmd,
                format!(" __norte_cd '{esperado}'\n").into_bytes(),
                "{nombre:?}"
            );
        }
    }

    /// Un NUL no puede estar en un nombre de fichero, pero `cd_command` es
    /// pública y toma bytes: si llegara, readline se lo tragaría en silencio y
    /// el shell acabaría en una ruta un byte más corta que la que norte cree.
    #[test]
    fn un_nul_no_se_manda() {
        assert_eq!(cd_command(Shell::Bash, b"/tmp/a\0b"), None);
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
