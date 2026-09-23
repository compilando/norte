//! Un shell VIVO detrás de una [`Pantalla`], para quien quiera un panel de
//! terminal.
//!
//! Está detrás de la feature `pty` y apagado de fábrica, para que el crate
//! siga siendo lo que dice su portada: una rejilla pura. Quien solo quiera
//! parsear bytes no compila nada de esto.
//!
//! # Por qué vive aquí y no en cada frontend
//!
//! Lo necesitan los dos —la terminal lo pinta con `ratatui`, la ventana manda
//! las filas por el puente— y es exactamente el mismo pty, el mismo hilo
//! lector y el mismo trozo delicado: contestar las consultas de terminal
//! ANTES de nada, porque el shell se para hasta que le contesten. Escribirlo
//! dos veces es tener dos sitios donde arreglar el mismo fallo.
//!
//! # Lo que NO decide este módulo
//!
//! **Qué programa se lanza y con qué entorno.** Eso lo trae quien llama, en
//! [`Arranque`]: resolver el shell del lector y el contrato de `NORTE_LEVEL`
//! son reglas de norte, no de un emulador, y meterlas aquí ataría este crate
//! al de presentación — justo al revés de como van las capas.
//!
//! Lo único del entorno que sí es suyo es [`TERM`], y por una razón: quien
//! sabe qué secuencias entiende esta rejilla es esta rejilla.

use std::io::{Read as _, Write as _};
use std::sync::{Arc, Mutex};

use crate::Pantalla;

/// Lo que se le dice al hijo que hay al otro lado.
///
/// `vt100` y no `xterm-256color`, que es lo que heredaría: esta rejilla hace
/// SGR, CUP, los cuatro movimientos, ED, EL y esconder el cursor, y nada más.
/// Ni pantalla alterna, ni regiones de scroll, ni insertar o borrar líneas.
///
/// Anunciar lo que no se cumple es peor que quedarse corto, y aquí tiene
/// consecuencias que se ven: con la pantalla alterna prometida y no
/// implementada, un `less` deja su último fotograma puesto al salir, y un
/// programa que se posicione dentro de una región de scroll pinta algo
/// coherente que no se corresponde con su estado. Es una superficie donde
/// alguien LEE y después teclea una orden contra lo que leyó.
///
/// Cuesta el color de un `ls` —vt100 no declara ninguno— y lo recupera #366,
/// que implementa lo que falta y sube esto de una vez.
pub const TERM: &str = "vt100";

/// Cuánto se guarda de lo que el shell escribió y aún no se ha volcado.
///
/// Un `find /` escribiendo sin parar mientras nadie repinta no puede comerse
/// la memoria. Se conserva la COLA: lo de más atrás ya no se vería de todas
/// formas, porque la rejilla tiene el alto que tiene.
const BUFFER_MAX: usize = 256 * 1024;

/// Qué shell arrancar, dónde y con qué entorno.
pub struct Arranque<'a> {
    /// El programa. Ruta ABSOLUTA: `portable_pty` busca por el `cwd` si no lo
    /// es, y el `cwd` de un gestor de ficheros es el directorio que se está
    /// mirando — un `bash` dejado ahí se ejecutaría (#302, ADR 0082).
    pub programa: &'a std::path::Path,
    /// Dónde se sienta.
    pub dir: &'a std::path::Path,
    /// Columnas y filas.
    pub tam: (u16, u16),
    /// Variables que se AÑADEN al entorno heredado.
    ///
    /// Se hereda el resto a propósito: es el entorno que el shell del lector
    /// le dio a norte, y quitárselo dejaría un shell sin `PATH` ni `HOME` que
    /// nadie pidió.
    pub env: &'a [(std::ffi::OsString, std::ffi::OsString)],
}

/// Lo que el hilo lector deja para quien repinte.
#[derive(Default)]
struct Buzon {
    /// Bytes leídos del pty y todavía sin alimentar a la rejilla.
    pendiente: Vec<u8>,
    /// El pty se cerró: el shell se fue.
    cerrado: bool,
}

/// La entrada del pty, compartida entre quien teclea y el hilo lector.
///
/// Dos escritores y los dos legítimos: las teclas, y las RESPUESTAS a las
/// consultas de terminal que el shell manda y por las que se PARA hasta que
/// le contesten (fish 4 lo hace antes de su primer prompt).
type Escritor = Arc<Mutex<Box<dyn std::io::Write + Send>>>;

/// Cómo se contesta una consulta de terminal del shell.
///
/// Se recibe de quien llama en vez de decidirse aquí porque la respuesta
/// honesta depende de lo que el emulador diga ser, y hoy esa tabla vive en
/// `norte-frontend` junto a la del subshell — que es el sitio donde alguien
/// la va a buscar.
///
/// **Ojo a lo que esto es**: una escritura NO PEDIDA en la entrada del shell,
/// disparada por el CONTENIDO que pasa por el pty. Un fichero con `\x1b[c`
/// dentro hace que se teclee la respuesta donde esté el cursor del editor de
/// línea. No ejecuta nada —las respuestas son constantes fijas sin CR— y
/// cualquier terminal de verdad hace lo mismo, pero conviene saberlo.
pub type Responder = fn(&[u8]) -> Option<Vec<u8>>;

/// Un shell vivo con su rejilla.
pub struct Shell {
    pantalla: Pantalla,
    escritura: Escritor,
    maestro: Box<dyn portable_pty::MasterPty + Send>,
    hijo: Box<dyn portable_pty::Child + Send + Sync>,
    buzon: Arc<Mutex<Buzon>>,
    tam: (u16, u16),
}

impl Shell {
    /// Arranca el shell.
    ///
    /// # Errors
    /// Lo que falle al abrir el pty o al lanzar el programa.
    pub fn abrir(a: &Arranque<'_>, responder: Responder) -> std::io::Result<Self> {
        let tam = (a.tam.0.max(1), a.tam.1.max(1));
        let sistema = portable_pty::native_pty_system();
        let par = sistema
            .openpty(portable_pty::PtySize {
                rows: tam.1,
                cols: tam.0,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(std::io::Error::other)?;
        let mut cmd = portable_pty::CommandBuilder::new(a.programa);
        cmd.cwd(a.dir);
        for (k, v) in a.env {
            cmd.env(k, v);
        }
        cmd.env("TERM", TERM);
        // El tamaño heredado es el de la terminal de FUERA, que no tiene nada
        // que ver con este hueco: quien pregunte por ahí se llevaría el ancho
        // equivocado. El pty ya dice el bueno por `TIOCGWINSZ`.
        cmd.env_remove("COLUMNS");
        cmd.env_remove("LINES");
        let hijo = par
            .slave
            .spawn_command(cmd)
            .map_err(std::io::Error::other)?;
        // El esclavo se SUELTA: mientras se tenga abierto, cerrar el shell no
        // cierra el pty y el lector nunca vería EOF.
        drop(par.slave);
        let escritura: Escritor = Arc::new(Mutex::new(
            par.master.take_writer().map_err(std::io::Error::other)?,
        ));
        let lector = par
            .master
            .try_clone_reader()
            .map_err(std::io::Error::other)?;
        let buzon = Arc::new(Mutex::new(Buzon::default()));
        lanzar_lector(
            lector,
            Arc::clone(&buzon),
            Arc::clone(&escritura),
            responder,
        );
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
    /// Devolver si hubo bytes es lo que evita repintar cuando el shell está
    /// quieto, que es casi siempre.
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
    /// Los dos, y que sean los dos importa: sin avisar al pty, un programa de
    /// pantalla completa sigue pintando para el tamaño viejo y lo que se ve es
    /// basura. No hace nada si no cambió.
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
    /// Esperar detrás del `kill` no es cortesía: sin recoger al hijo queda un
    /// zombi hasta que el proceso salga.
    pub fn matar(&mut self) {
        let _ = self.hijo.kill();
        let _ = self.hijo.wait();
    }
}

/// **El shell muere CON el panel, salga el proceso por donde salga.**
///
/// Sin esto, cerrar el hueco quitaba el nodo y dejaba el shell vivo con su
/// hilo lector, su pty y su directorio abierto —un montaje ocupado seguía
/// ocupado—, sin panel donde verlo y sin forma de volver a él. Al salir moría
/// por accidente, por el `SIGHUP` del núcleo, así que lo que ignorara esa
/// señal sobrevivía al proceso entero.
impl Drop for Shell {
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
    responder: Responder,
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
                    // esperándolo y la respuesta no puede esperar a que
                    // alguien repinte. Ver [`Responder`] para lo que esto es.
                    if let Some(r) = responder(&buf[..n]) {
                        let mut e = escritura
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let _ = e.write_all(&r);
                        let _ = e.flush();
                    }
                    let mut b = buzon_de(&buzon);
                    b.pendiente.extend_from_slice(&buf[..n]);
                    // El corte cae en un byte CUALQUIERA, y se acepta: un
                    // escape partido por ahí pierde su `ESC [` y su cola se
                    // pinta como texto. Es feo y no es peligroso —la garantía
                    // de la rejilla aguanta, un byte de control no llega a una
                    // celda—, y solo pasa si un programa escribió 256 KiB
                    // mientras nadie repintaba. Buscar una frontera de
                    // secuencia aquí obligaría a meter el parser en el hilo
                    // lector para tirar bytes.
                    if b.pendiente.len() > BUFFER_MAX {
                        let sobra = b.pendiente.len() - BUFFER_MAX;
                        b.pendiente.drain(..sobra);
                    }
                }
            }
        }
    });
}
