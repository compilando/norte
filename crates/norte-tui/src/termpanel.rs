//! El panel de terminal (#362): un shell DENTRO de un hueco de la disposición.
//!
//! Es lo que Krusader tiene y norte no tenía. Lo que ya había es distinto y en
//! una cosa mejor: `app.toggle-panels` cede la terminal ENTERA a un subshell
//! vivo (ADR 0084) y `app.terminal` lanza un shell aparte. Lo que faltaba es
//! verlo A LA VEZ que los listados.
//!
//! # El reparto
//!
//! La emulación —los bytes a una rejilla de celdas— vive en `norte-term`, sin
//! pty y sin toolkit. Aquí está lo que no se puede probar sin sistema
//! operativo: el pty, el hilo lector y el pintado. Es el mismo reparto que
//! [`crate::subshell`] tiene con `norte_frontend::subshell`, y se copia el
//! reparto, no el código.
//!
//! # Quién manda en el teclado
//!
//! Éste es el único panel que consume BYTES y no comandos del catálogo, así
//! que mientras tiene las teclas se queda también con los acordes que serían
//! de norte. La salida es UN acorde suelto —el mismo `layout.terminal` que lo
//! abrió— y lo reconoce [`crate::keys`] antes de reenviar nada. Si el preset
//! lo atara a una secuencia, `Effective::lone_chord` devuelve `None` y el
//! panel NO toma las teclas: mejor un panel que se mira que uno del que no se
//! puede salir.
//!
//! # Lo que este panel NO hace todavía
//!
//! No instala el gancho del prompt, así que el listado no sigue al shell ni el
//! shell al listado: para eso está el subshell de #142, que sí lo instala. Un
//! panel que tecleara `cd` en el shell del lector tiene los problemas de #363
//! —y los tendría en un sitio donde el lector ve lo que pasa—, así que eso
//! espera a que ese agujero esté cerrado.

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};

use norte_term::{ColorTerm, Estilo, Pantalla};

/// El id del kind, que es también el sufijo de su comando.
pub const KIND: &str = "terminal";

/// El comando que abre el panel, le da el teclado y se lo quita.
///
/// Es el MISMO que sale, y por eso está aquí y no escrito a mano en los dos
/// sitios que lo buscan en el keymap: el acorde que lo corre es el único que
/// el panel no le pasa al shell.
pub const COMANDO: &str = "layout.terminal";

/// Cuánto se guarda de lo que el shell escribió y aún no se ha volcado a la
/// rejilla.
///
/// Un `find /` escribiendo sin parar mientras nadie repinta no puede comerse
/// la memoria. Se conserva la COLA: lo de más atrás ya no se vería de todas
/// formas, porque la rejilla tiene el alto que tiene.
const BUFFER_MAX: usize = 256 * 1024;

/// Lo que el hilo lector deja para el bucle de eventos.
#[derive(Default)]
struct Buzon {
    /// Bytes leídos del pty y todavía sin alimentar a la rejilla.
    pendiente: Vec<u8>,
    /// El pty se cerró: el shell se fue.
    cerrado: bool,
}

/// Un shell vivo pintado en un hueco.
pub struct TermPanel {
    /// La emulación: lo que se ve.
    pantalla: Pantalla,
    /// Por dónde se le escribe. Compartida con el hilo lector, que contesta
    /// las consultas de terminal del shell.
    escritura: Escritor,
    /// El pty, que además es quien redimensiona.
    maestro: Box<dyn portable_pty::MasterPty + Send>,
    /// El hijo, para poder matarlo y para saber si sigue vivo.
    hijo: Box<dyn portable_pty::Child + Send + Sync>,
    /// Lo que el lector ha dejado.
    buzon: Arc<Mutex<Buzon>>,
    /// El tamaño que el pty cree tener, para no anunciarlo si no ha cambiado.
    tam: (u16, u16),
}

/// La entrada del pty, compartida entre el bucle y el hilo lector.
///
/// Dos escritores y los dos legítimos, igual que en [`crate::subshell`]: las
/// teclas del lector, y las RESPUESTAS a las consultas de terminal que el
/// shell manda y por las que se PARA hasta que le contesten.
type Escritor = Arc<Mutex<Box<dyn std::io::Write + Send>>>;

impl TermPanel {
    /// Arranca un shell en `dir`, con una rejilla de `tam` (columnas, filas).
    ///
    /// # Errors
    /// Lo que falle al abrir el pty o al lanzar el shell.
    pub fn abrir(dir: &std::path::Path, tam: (u16, u16)) -> std::io::Result<Self> {
        let sistema = portable_pty::native_pty_system();
        let par = sistema
            .openpty(portable_pty::PtySize {
                rows: tam.1,
                cols: tam.0,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(std::io::Error::other)?;
        let shell = norte_frontend::shell::login_shell();
        let mut cmd = portable_pty::CommandBuilder::new(&shell);
        cmd.cwd(dir);
        // El hijo sabe que está DENTRO de norte, igual que el subshell y que
        // una suspensión: es el mismo contrato de `NORTE_LEVEL`, y el prompt
        // del lector lo lee para decirlo.
        cmd.env(
            norte_frontend::shell::LEVEL_VAR,
            norte_frontend::shell::next_norte_level(),
        );
        // **`TERM` se pone a mano, y es lo único del entorno que se toca.**
        //
        // Heredar el resto es correcto: es el entorno que el shell del lector
        // le dio a norte, y quitárselo dejaría un shell sin `PATH` ni `HOME`
        // que nadie pidió. `TERM` es distinto en especie. Para el subshell y
        // para `app.terminal` el hijo habla con la terminal DE VERDAD, así que
        // el `TERM` heredado es la verdad. Aquí el otro extremo es esta
        // rejilla, que hace SGR, CUP, los cuatro movimientos, ED, EL y esconder
        // el cursor — y nada más. Ni pantalla alterna, ni regiones de scroll,
        // ni `ESC 7`/`ESC 8`, ni insertar o borrar líneas.
        //
        // Con `xterm-256color` heredado se le dice a cada programa de dentro
        // que puede usar todo eso. Un `less` o un `vim` pintan entonces una
        // pantalla coherente que no se corresponde con su estado, y al salir
        // dejan su último fotograma puesto porque el cambio de pantalla
        // alterna no hace nada. Es una superficie en la que alguien LEE algo y
        // después teclea una orden, así que mentir aquí es peor que quedarse
        // corto: «prometer capacidades que luego no se cumplen es peor que no
        // prometerlas» ya está escrito en el subshell, sobre esto mismo.
        //
        // `vt100` es lo que esta rejilla es de verdad. Cuesta el color de un
        // `ls` —vt100 no declara ninguno— y lo recupera #366, que implementa
        // lo que falta y sube el `TERM` de una vez.
        cmd.env("TERM", "vt100");
        // Y el tamaño heredado es el de la terminal de FUERA, que no tiene
        // nada que ver con el hueco: quien pregunte por ahí se llevaría el
        // ancho equivocado. El pty ya dice el bueno por `TIOCGWINSZ`.
        cmd.env_remove("COLUMNS");
        cmd.env_remove("LINES");
        let hijo = par
            .slave
            .spawn_command(cmd)
            .map_err(std::io::Error::other)?;
        // El esclavo se SUELTA: mientras norte lo tenga abierto, cerrar el
        // shell no cierra el pty y el lector nunca vería EOF.
        drop(par.slave);
        let escritura: Escritor = Arc::new(Mutex::new(
            par.master.take_writer().map_err(std::io::Error::other)?,
        ));
        let lector = par
            .master
            .try_clone_reader()
            .map_err(std::io::Error::other)?;
        let buzon = Arc::new(Mutex::new(Buzon::default()));
        lanzar_lector(lector, Arc::clone(&buzon), Arc::clone(&escritura));
        Ok(Self {
            pantalla: Pantalla::nueva(tam.0, tam.1),
            escritura,
            maestro: par.master,
            hijo,
            buzon,
            tam,
        })
    }

    /// Vuelca a la rejilla lo que el shell haya escrito, y dice si cambió algo.
    ///
    /// Lo llama el bucle de eventos en cada vuelta. Devolver si hubo bytes es
    /// lo que evita repintar la pantalla entera cuando el shell está quieto,
    /// que es casi siempre.
    pub fn bombear(&mut self) -> bool {
        let pendiente = {
            let mut b = buzon_de(&self.buzon);
            std::mem::take(&mut b.pendiente)
        };
        if pendiente.is_empty() {
            return false;
        }
        self.pantalla.alimentar(&pendiente);
        true
    }

    /// Le manda bytes al shell.
    pub fn escribir(&mut self, bytes: &[u8]) {
        let mut e = self
            .escritura
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = e.write_all(bytes);
        let _ = e.flush();
    }

    /// Ajusta la rejilla Y el pty al tamaño del hueco.
    ///
    /// Los dos, y en ese orden importa poco pero que sean los dos importa
    /// mucho: sin avisar al pty, un programa de pantalla completa sigue
    /// pintando para el tamaño viejo y lo que se ve es basura.
    pub fn redimensionar(&mut self, tam: (u16, u16)) {
        let tam = (tam.0.max(1), tam.1.max(1));
        if tam == self.tam {
            return;
        }
        self.tam = tam;
        self.pantalla.redimensionar(tam.0, tam.1);
        let _ = self.maestro.resize(portable_pty::PtySize {
            rows: tam.1,
            cols: tam.0,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// ¿Se fue el shell?
    pub fn muerto(&mut self) -> bool {
        buzon_de(&self.buzon).cerrado || matches!(self.hijo.try_wait(), Ok(Some(_)))
    }

    /// La rejilla, para pintarla.
    #[must_use]
    pub fn pantalla(&self) -> &Pantalla {
        &self.pantalla
    }

    /// Mata el shell y lo espera.
    ///
    /// Lo llaman el cierre del hueco y el [`Drop`]. Esperar detrás del `kill`
    /// no es cortesía: sin recoger al hijo queda un zombi hasta que norte
    /// salga.
    pub fn matar(&mut self) {
        let _ = self.hijo.kill();
        let _ = self.hijo.wait();
    }
}

/// **El shell muere CON el panel, salga norte por donde salga.**
///
/// Sin esto, `matar` no lo llamaba nadie —estaba escrito y sin un solo
/// llamante— y la documentación de `App::terminal` prometía que cerrar el
/// hueco o salir de norte lo mataban. Ninguna de las dos era cierta: cerrar
/// el hueco quitaba el nodo y dejaba el shell vivo con su hilo lector, su pty
/// y su directorio abierto —un montaje ocupado seguía ocupado—, invisible y
/// sin forma de volver a él. Al salir moría por accidente, porque el maestro
/// se cierra y el núcleo manda `SIGHUP`, y lo que ignore esa señal sobrevivía
/// a norte entero.
///
/// Es exactamente el razonamiento que [`crate::subshell::Subshell`] ya había
/// escrito para su propio `Drop`, y que aquí se olvidó.
impl Drop for TermPanel {
    fn drop(&mut self) {
        self.matar();
    }
}

fn buzon_de(buzon: &Mutex<Buzon>) -> std::sync::MutexGuard<'_, Buzon> {
    buzon
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lanzar_lector(
    mut lector: Box<dyn std::io::Read + Send>,
    buzon: Arc<Mutex<Buzon>>,
    escritura: Escritor,
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
                    // esperándolo —fish 4 pregunta antes de su primer prompt—
                    // y la respuesta no puede esperar a que alguien repinte.
                    // Es la misma función que usa el subshell: una consulta de
                    // terminal se contesta igual venga de donde venga.
                    //
                    // Lo que hay que saber de esto: el disparador es el
                    // CONTENIDO, no el shell. `terminal_reply` busca la
                    // secuencia como subcadena, así que un fichero con
                    // `\x1b[c` dentro hace que norte teclee la respuesta
                    // donde esté el cursor del editor de línea. No es
                    // peligroso —las dos respuestas son constantes fijas sin
                    // CR, así que no ejecutan nada, y cualquier terminal de
                    // verdad contesta igual sin mirar quién preguntó—, pero
                    // es una escritura no pedida en un editor de línea, que es
                    // la forma estructural de #363 con una carga que no daña.
                    // Lo correcto sería que la decidiera el parser de la
                    // rejilla (un `csi_dispatch` para `c`), que es quien sabe
                    // si está dentro de una cadena OSC o DCS.
                    if let Some(r) = norte_frontend::subshell::terminal_reply(&buf[..n]) {
                        let mut e = escritura
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let _ = e.write_all(&r);
                        let _ = e.flush();
                    }
                    let mut b = buzon_de(&buzon);
                    b.pendiente.extend_from_slice(&buf[..n]);
                    // El corte cae en un byte CUALQUIERA, y eso se acepta: un
                    // escape partido por ahí pierde su `ESC [` y su cola se
                    // pinta como texto (`31m` en pantalla). Es feo y no es
                    // peligroso —la garantía de la rejilla aguanta igual, un
                    // byte de control no llega a una celda—, y solo pasa si
                    // un programa escribió 256 KiB mientras nadie repintaba.
                    // Buscar una frontera de secuencia aquí obligaría a meter
                    // el parser en el hilo lector para tirar bytes.
                    if b.pendiente.len() > BUFFER_MAX {
                        let sobra = b.pendiente.len() - BUFFER_MAX;
                        b.pendiente.drain(..sobra);
                    }
                }
            }
        }
    });
}

/// Las filas de la rejilla como spans, agrupando las celdas que comparten
/// estilo.
///
/// Se agrupa porque una fila de ochenta celdas son ochenta spans si no, y eso
/// se pinta ochenta veces por fila y por vuelta. Y porque el contenido es
/// AJENO: lo que sale de aquí ya no lleva ningún byte de control —lo garantiza
/// la rejilla, no este código—, así que no hay que enmascarar nada encima.
#[must_use]
pub fn filas<'a>(p: &Pantalla) -> Vec<ratatui::text::Line<'a>> {
    use ratatui::text::{Line, Span};
    let (ancho, alto) = p.tamano();
    (0..alto)
        .map(|f| {
            let mut spans: Vec<Span<'a>> = Vec::new();
            let mut texto = String::new();
            let mut estilo: Option<Estilo> = None;
            for c in (0..ancho).filter_map(|c| p.celda(f, c)) {
                if c.estela {
                    continue;
                }
                if estilo != Some(c.estilo) {
                    if let Some(e) = estilo.take() {
                        spans.push(Span::styled(std::mem::take(&mut texto), estilo_de(e)));
                    }
                    estilo = Some(c.estilo);
                }
                texto.push(c.c);
            }
            if let Some(e) = estilo {
                spans.push(Span::styled(texto, estilo_de(e)));
            }
            Line::from(spans)
        })
        .collect()
}

/// Un [`Estilo`] de terminal traducido al de `ratatui`.
///
/// Un color INDEXADO pasa tal cual (`Color::Indexed`): en una terminal lo
/// resuelve la paleta que el lector tiene puesta en su emulador, que es
/// exactamente lo que haría si el programa corriera fuera de norte.
///
/// **Por eso aquí no entra el tema de norte, ni como argumento.** Esto es
/// contenido de OTRO programa, no cromo nuestro, y un tema que le cambiara los
/// colores a un `ls --color` estaría mintiendo sobre lo que ese programa dijo.
/// Lo nuestro es el marco, y el marco lo pinta quien lo dibuja.
fn estilo_de(e: Estilo) -> ratatui::style::Style {
    use ratatui::style::{Modifier, Style};
    let mut s = Style::default();
    if let Some(c) = color_de(e.fg) {
        s = s.fg(c);
    }
    if let Some(c) = color_de(e.bg) {
        s = s.bg(c);
    }
    let mut m = Modifier::empty();
    if e.negrita {
        m |= Modifier::BOLD;
    }
    if e.tenue {
        m |= Modifier::DIM;
    }
    if e.cursiva {
        m |= Modifier::ITALIC;
    }
    if e.subrayado {
        m |= Modifier::UNDERLINED;
    }
    if e.inverso {
        m |= Modifier::REVERSED;
    }
    if e.tachado {
        m |= Modifier::CROSSED_OUT;
    }
    s.add_modifier(m)
}

fn color_de(c: ColorTerm) -> Option<ratatui::style::Color> {
    use ratatui::style::Color;
    match c {
        ColorTerm::PorDefecto => None,
        ColorTerm::Indexado(n) => Some(Color::Indexed(n)),
        ColorTerm::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// Dónde va el cursor dentro del área de contenido, si hay que pintarlo.
///
/// Devuelve `None` cuando el shell lo escondió (`CSI ?25l`, que es lo que hace
/// cualquier programa de pantalla completa) o cuando el panel no tiene las
/// teclas: un cursor parpadeando en un panel que no las tiene dice que el
/// teclado está ahí, y no lo está.
#[must_use]
pub fn cursor_en(
    p: &Pantalla,
    area: ratatui::layout::Rect,
    con_teclado: bool,
) -> Option<(u16, u16)> {
    if !con_teclado || !p.cursor_visible() {
        return None;
    }
    let (fila, col) = p.cursor();
    let (ancho, alto) = p.tamano();
    // La columna puede valer tanto como el ancho —el estado «pendiente de
    // salto»—, y ahí el cursor se pinta en la última celda: es donde un
    // terminal de verdad lo deja.
    let col = col.min(ancho.saturating_sub(1));
    (fila < alto).then(|| (area.x + col, area.y + fila))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El estilo de terminal llega a `ratatui` con los atributos puestos, y un
    /// índice sigue siendo un índice: resolverlo aquí le quitaría al emulador
    /// del lector su propia paleta.
    #[test]
    fn un_estilo_de_terminal_cruza_entero() {
        let e = Estilo {
            fg: ColorTerm::Indexado(4),
            bg: ColorTerm::Rgb(1, 2, 3),
            negrita: true,
            subrayado: true,
            ..Estilo::default()
        };
        let s = estilo_de(e);
        assert_eq!(s.fg, Some(ratatui::style::Color::Indexed(4)));
        assert_eq!(s.bg, Some(ratatui::style::Color::Rgb(1, 2, 3)));
        assert!(s.add_modifier.contains(ratatui::style::Modifier::BOLD));
        assert!(
            s.add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
    }

    /// Las celdas seguidas con el mismo estilo son UN span: ochenta spans por
    /// fila es lo que hace que un `make` en el panel se note en el resto.
    #[test]
    fn las_celdas_iguales_se_agrupan_en_un_span() {
        let mut p = Pantalla::nueva(10, 1);
        p.alimentar(b"aaa\x1b[31mbbb");
        let filas = filas(&p);
        let spans = &filas[0].spans;
        // Tres y no dos: detrás de `bbb` quedan cuatro celdas sin escribir, y
        // ésas llevan el estilo POR DEFECTO, no el rojo. Agruparlas con lo
        // rojo pintaría el fondo del resto de la línea del color de la última
        // orden, que es el fallo clásico de un emulador escrito a ojo.
        assert_eq!(spans.len(), 3, "dos estilos y el relleno: {spans:?}");
        assert_eq!(spans[0].content, "aaa");
        assert_eq!(spans[1].content, "bbb");
        assert_eq!(spans[2].content, "    ");
        assert_eq!(spans[2].style, ratatui::style::Style::default());
    }

    /// Sin teclado no se pinta cursor: sería decir que el teclado está aquí.
    #[test]
    fn el_cursor_solo_se_pinta_con_el_teclado_dentro() {
        let p = Pantalla::nueva(10, 3);
        let area = ratatui::layout::Rect::new(5, 2, 10, 3);
        assert_eq!(cursor_en(&p, area, false), None);
        assert_eq!(cursor_en(&p, area, true), Some((5, 2)));
    }

    /// Y tampoco cuando el shell lo esconde, que es lo que hace cualquier
    /// programa de pantalla completa mientras pinta.
    #[test]
    fn un_cursor_escondido_no_se_pinta() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"\x1b[?25l");
        let area = ratatui::layout::Rect::new(0, 0, 10, 3);
        assert_eq!(cursor_en(&p, area, true), None);
    }
}
