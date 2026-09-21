//! Un anillo de líneas de log en memoria, para que un frontend las enseñe.
//!
//! El log existe desde #255 y va a un fichero. Eso sirve para investigar
//! DESPUÉS, y no sirve para lo que pasa mientras miras: una conexión que falla
//! en 240 ms deja en pantalla un «permiso denegado» que no dice nada, mientras
//! el motivo exacto —«no se pudo resolver el secreto», «autenticación
//! rechazada»— se escribe en un fichero que hay que ir a buscar a otra
//! terminal. Esto es la otra mitad: las mismas líneas, en memoria, para
//! pintarlas donde ya está el lector.
//!
//! Vive en esta crate y no en la TUI porque la ventana necesita exactamente lo
//! mismo, y porque el montaje del subscriber ya es de aquí.
//!
//! # El cap de seguridad no es negociable
//!
//! [`crate::logging`] documenta que `suppaftp` loguea `PASS <password>` a nivel
//! TRACE del crate `log`, y por eso el filtro del fichero lleva una directiva
//! `suppaftp=info` que gana a cualquier `RUST_LOG`. Este anillo lleva la MISMA
//! cota, y aquí importa más: su nivel se sube en caliente desde la interfaz, o
//! sea que sin la cota bastaría con que alguien pidiera DEBUG en el panel para
//! que una contraseña de FTP apareciera en pantalla. La cota está en
//! `bajo_cota`, y la prueba que la fija es la más importante del módulo.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tracing::{Level, Metadata};
use tracing_subscriber::layer::{Context, Filter};

/// Cuántas líneas guarda el anillo si nadie dice otra cosa.
///
/// 2000: suficiente para que quepa entera la sesión que estás depurando, poco
/// para que la memoria no se note. Cuando se llena tira las viejas y lo DICE
/// ([`LogRing::dropped`]): un panel que descarta en silencio miente sobre lo
/// que hubo.
pub const RING_DEFAULT: usize = 2000;

/// Los targets cuyo nivel puede subir el lector desde el panel.
///
/// **Lista blanca, y esto se corrigió tras una revisión.** La primera versión
/// era una lista NEGRA con un solo nombre, `suppaftp`, porque es el que #43
/// documentó como emisor de `PASS <contraseña>` en TRACE. Pero lo que protegía
/// a todo lo demás era el filtro GLOBAL a INFO, y este módulo lo quitó del
/// camino del anillo justo para poder subir el nivel en caliente. Con la lista
/// negra, una tecla ponía en TRACE **todo el espacio de direcciones**: la TUI
/// embebe el core, así que ahí dentro están `russh`, `rustls`, `hyper`,
/// `reqwest` y `opendal`, que a ese nivel escriben cabeceras y búferes de red.
///
/// El argumento vale igual al revés: nadie abre este panel para leer tramas de
/// hyper. Lo que se quiere ver es lo que hace norte. Así que sube de nivel lo
/// NUESTRO, y lo de terceros se queda en INFO pase lo que pase — que es lo que
/// hacía el filtro global que quitamos.
///
/// Son nombres de CRATE, y se comparan como tales: ver [`es_nuestro`].
const NUESTRO: &[&str] = &["norte", "ntc"];

/// El target cuyo TRACE lleva contraseñas (regla 10, #43). Redundante con la
/// lista blanca —`suppaftp` no empieza por `norte`— y se queda como segundo
/// cinturón: es la única cota que está documentada con un CVE detrás, y
/// perderla al refactorizar la lista blanca sería silencioso.
const TARGET_CON_SECRETOS: &str = "suppaftp";

/// ¿Es este target NUESTRO? Por SEGMENTO de crate, nunca por prefijo crudo.
///
/// Un `starts_with` sobre la cadena entera —que es lo que había— aceptaba
/// `nortex` y `ntcp`: una dependencia futura con un nombre así habría entrado
/// en TRACE, en un anillo cuyo nivel sube cualquier cliente local con una
/// tecla y que se lee en pantalla. Y esta lista blanca es la ÚNICA cota que
/// mantiene fuera el `PASS <contraseña>` de `suppaftp` (#43, regla 10), así
/// que ensancharla por descuido es exactamente el fallo que no se ve.
///
/// La forma de un target de verdad es `norte_core::connect`,
/// `norte_vfs_local`, `ntc`: nombre de CRATE con guiones bajos, y detrás la
/// ruta de módulo tras `::`. Así que se compara contra el primer segmento, y
/// solo vale si es el nombre exacto (`norte`, `ntc`, el binario) o si continúa
/// con `_` (`norte_core`, `ntc_algo`). `nortex` no continúa con `_` y queda
/// fuera, que es el punto.
fn es_nuestro(target: &str) -> bool {
    let raiz = target.split("::").next().unwrap_or(target);
    NUESTRO.iter().any(|nuestro| {
        raiz == *nuestro
            || raiz
                .strip_prefix(*nuestro)
                .is_some_and(|resto| resto.starts_with('_'))
    })
}

/// ¿Puede esta línea entrar en el anillo por encima de INFO?
///
/// La cota va aquí y no en el filtro configurable a propósito: lo configurable
/// se cambia desde la interfaz y esto no debe poder cambiarse desde ninguna
/// parte.
fn bajo_cota(target: &str, level: Level) -> bool {
    if level <= Level::INFO {
        // INFO y peores pasan siempre: es lo que el fichero registra por
        // defecto, y es el nivel al que el anillo arranca.
        return true;
    }
    // El segundo cinturón sigue siendo un `starts_with` crudo, y eso es
    // deliberado: en una lista NEGRA lo ancho es lo seguro, así que un
    // `suppaftp_algo` que no existe hoy ya estaría cubierto. En la lista
    // BLANCA es al revés, y por eso ésa va por segmento ([`es_nuestro`]).
    !target.starts_with(TARGET_CON_SECRETOS) && es_nuestro(target)
}

pub use crate::logline::{LogLevel, LogLine};

/// Lo que había después de un cursor, y lo que ese cursor se perdió.
///
/// Existe para [`LogRing::since`], que a su vez existe para que un frontend
/// pueda sondear sin repintar dos mil líneas por vuelta (ver
/// [`LogRing::pushed`]): el cliente guarda `next` y en la siguiente vuelta
/// pide desde ahí. `lost` es lo que hace ese sondeo honesto — sin él, un
/// cliente lento que se queda atrás del anillo vería un salto en el
/// contenido y no una explicación.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tail {
    /// Las líneas posteriores al cursor, de la más vieja a la más nueva.
    pub lines: Vec<LogLine>,
    /// El cursor para la siguiente llamada.
    pub next: u64,
    /// Cuántas líneas cayeron del anillo antes de que este cursor las viera.
    pub lost: u64,
}

/// De `tracing` al tipo que pinta el frontend.
fn nivel_de(l: Level) -> LogLevel {
    match l {
        Level::ERROR => LogLevel::Error,
        Level::WARN => LogLevel::Warn,
        Level::INFO => LogLevel::Info,
        Level::DEBUG => LogLevel::Debug,
        _ => LogLevel::Trace,
    }
}

/// Estado del anillo.
#[derive(Debug)]
struct Ring {
    lines: VecDeque<LogLine>,
    cap: usize,
}

/// Anillo compartido entre la capa de `tracing` y quien lo pinta.
///
/// `Clone` reparte el MISMO anillo (es un `Arc`): la capa escribe y el frontend
/// lee sin coordinarse.
#[derive(Debug, Clone)]
pub struct LogRing {
    ring: Arc<Mutex<Ring>>,
    /// Nivel mínimo que se guarda, cambiable en caliente desde la interfaz.
    /// Un `u8` y no el `Level` porque tiene que ser atómico.
    nivel: Arc<AtomicU8>,
    /// Cuántas se han descartado por llenarse.
    dropped: Arc<AtomicU64>,
    /// Cuántas líneas han ENTRADO en total, desde siempre.
    ///
    /// Un contador que solo sube, para poder preguntar «¿ha cambiado algo?»
    /// sin clonar el anillo. La ventana lo necesita porque su panel no se
    /// repinta por frame como el de la TUI: tiene que sondear, y sondear con
    /// [`LogRing::snapshot`] clonaría dos mil líneas por vuelta para casi
    /// siempre descubrir que no hay nada nuevo.
    ///
    /// No vale la longitud: con el anillo lleno se queda fija en el tope y
    /// deja de moverse justo cuando más está pasando.
    pushed: Arc<AtomicU64>,
}

/// `Level` no es representable como número en la API pública de `tracing`, así
/// que se codifica aquí. Orden creciente de verbosidad.
fn nivel_a_u8(l: Level) -> u8 {
    match l {
        Level::ERROR => 0,
        Level::WARN => 1,
        Level::INFO => 2,
        Level::DEBUG => 3,
        _ => 4,
    }
}

fn u8_a_nivel(n: u8) -> Level {
    match n {
        0 => Level::ERROR,
        1 => Level::WARN,
        2 => Level::INFO,
        3 => Level::DEBUG,
        _ => Level::TRACE,
    }
}

impl LogRing {
    /// Un anillo de `cap` líneas, guardando desde INFO.
    ///
    /// Arranca en INFO y no en DEBUG porque el coste de un nivel verboso se
    /// paga aunque nadie mire: lo sube quien abre el panel y lo pide.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        Self {
            ring: Arc::new(Mutex::new(Ring {
                lines: VecDeque::with_capacity(cap.min(RING_DEFAULT)),
                cap: cap.max(1),
            })),
            nivel: Arc::new(AtomicU8::new(nivel_a_u8(Level::INFO))),
            dropped: Arc::new(AtomicU64::new(0)),
            pushed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// El nivel que se está guardando ahora mismo.
    ///
    /// En [`LogLevel`] y no en `tracing::Level` porque quien lo pregunta y lo
    /// cambia es un frontend, y un frontend de presentación no compila
    /// `tracing` (ver [`crate::logline`]).
    #[must_use]
    pub fn level(&self) -> LogLevel {
        nivel_de(u8_a_nivel(self.nivel.load(Ordering::Relaxed)))
    }

    /// Sube el nivel a `l` si hace falta, y NUNCA lo baja.
    ///
    /// El invariante vive aquí y no en quien llama, y eso se corrigió tras una
    /// revisión: estaba documentado en `LogPanel::show_level`, que devolvía el
    /// nivel mínimo a capturar y confiaba en que el llamante lo comparase y
    /// subiera. `#[must_use]` obliga a ATAR el valor, no a hacer nada con él —
    /// y en cuanto la ventana fuera el segundo llamante, copiaría un `let _ =`
    /// y su panel filtraría a DEBUG unas líneas que nadie capturó.
    ///
    /// No baja a propósito: ir a DEBUG, volver a WARN y pedir DEBUG otra vez
    /// tiene que enseñar lo de en medio. Quien quiera bajarlo de verdad usa
    /// [`Self::set_level`], y hoy solo lo hace cerrar el panel.
    pub fn raise_to(&self, l: LogLevel) {
        if self.level() < l {
            self.set_level(l);
        }
    }

    /// Fija el nivel EN CALIENTE, hacia arriba o hacia abajo.
    ///
    /// Lo que ya se descartó no vuelve: subir a DEBUG enseña los DEBUG de ahora
    /// en adelante, no los de antes. Quien lo pinta tiene que decirlo, porque un
    /// panel que se llena a medias tras pedir más detalle parece roto.
    ///
    /// Para el camino normal —«enséñame más»— usa [`Self::raise_to`]. Bajar es
    /// una decisión aparte, y hoy solo la toma cerrar el panel: sin ella, una
    /// pulsación dejaba el proceso capturando TRACE el resto de la sesión.
    pub fn set_level(&self, l: LogLevel) {
        let tracing_level = match l {
            LogLevel::Error => Level::ERROR,
            LogLevel::Warn => Level::WARN,
            LogLevel::Info => Level::INFO,
            LogLevel::Debug => Level::DEBUG,
            LogLevel::Trace => Level::TRACE,
        };
        self.nivel
            .store(nivel_a_u8(tracing_level), Ordering::Relaxed);
        // El nivel estático que `tracing` cachea por callsite sale de
        // `max_level_hint`, así que cambiarlo sin invalidar esa caché dejaría
        // los `debug!` cortados por el atajo barato aunque el anillo ya los
        // quiera. Las dos cosas van juntas o ninguna sirve.
        tracing::callsite::rebuild_interest_cache();
    }

    /// Cuántas líneas se han tirado por falta de sitio.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Cuántas líneas han entrado en total, para detectar cambios barato.
    ///
    /// Solo sube. Comparar dos lecturas dice si hay algo nuevo sin tomar el
    /// candado del anillo ni clonar nada.
    #[must_use]
    pub fn pushed(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }

    /// Cuántas líneas cabe guardar aquí dentro.
    ///
    /// Es la historia MÁS PROFUNDA que se puede pedir, y por eso viaja en la
    /// respuesta de `log.tail` (ADR 0092): quien pinta el registro puede decir
    /// «esto es todo lo que hay» en vez de insinuar que hay más. Cota
    /// SUPERIOR y no promesa — quien sirve el anillo recorta además lo que
    /// entrega en una vuelta, así que una sola llamada con este tamaño puede
    /// volver corta.
    ///
    /// No es [`Self::pushed`] ni la longitud de ahora: las dos se mueven, y
    /// ésta es la única de las tres que dice dónde está el fondo.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner).cap
    }

    /// Copia de las líneas, de la más vieja a la más nueva.
    ///
    /// Una copia y no un préstamo: el candado no puede quedarse tomado
    /// mientras se pinta un frame, porque quien escribe es cualquier hilo del
    /// runtime y bloquearlo por pintar convertiría el panel en un freno.
    #[must_use]
    pub fn snapshot(&self) -> Vec<LogLine> {
        let r = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        r.lines.iter().cloned().collect()
    }

    /// Lo que hay después de `cursor`, hasta `max` líneas.
    ///
    /// `cursor` no es un índice en `lines`: es la posición de
    /// [`Self::pushed`] la última vez que quien pregunta miró. Eso es lo que
    /// hace posible decir cuánto se perdió — un índice en el `VecDeque` ya no
    /// significa nada en cuanto una línea vieja sale por el otro lado.
    ///
    /// La aritmética: `pushed` solo sube y `lines.len()` es lo que sobrevive,
    /// así que la línea más vieja que queda tiene posición
    /// `base = pushed - len`. Un cursor por debajo de `base` se perdió
    /// `base - cursor` líneas, y es justo lo que [`Tail::lost`] cuenta — la
    /// alternativa, callarlo, es la misma mentira que
    /// [`Self::dropped`] existe para no contar. Un cursor por ENCIMA de
    /// `pushed` —un daemon que se reinició bajo un cliente que conservó su
    /// cursor de antes— no es un pánico ni un hueco: se trata como si fuera
    /// `pushed`, sin nada nuevo y sin nada perdido, porque no hay manera de
    /// saber qué había ahí y afirmar un hueco sería mentir en la otra
    /// dirección.
    ///
    /// `base`, `pushed` y la copia de `lines` se leen bajo el MISMO candado:
    /// leer `pushed` fuera de él permitiría que un escritor concurrente
    /// metiera líneas entre una lectura y la otra, y `lost` saldría mal —
    /// intermitente, que en este módulo es un bicho y no ruido.
    ///
    /// # Ejemplos
    ///
    /// ```
    /// use norte_config::logring::{LogRing, ring_layer};
    /// use tracing_subscriber::prelude::*;
    ///
    /// let anillo = LogRing::new(10);
    /// let sub = tracing_subscriber::registry().with(ring_layer(&anillo));
    /// tracing::subscriber::with_default(sub, || {
    ///     tracing::info!("conectando");
    /// });
    ///
    /// let tail = anillo.since(0, 10);
    /// assert_eq!(tail.lines.len(), 1);
    /// assert_eq!(tail.next, 1, "la próxima llamada pide desde aquí");
    /// assert_eq!(tail.lost, 0, "nada se perdió: el cursor no iba rancio");
    /// ```
    #[must_use]
    pub fn since(&self, cursor: u64, max: usize) -> Tail {
        // `pushed` y `r.lines` bajo el MISMO candado (ver el rustdoc): leerlos
        // por separado dejaría una ventana para que `push` metiera una línea
        // entre las dos lecturas y `base` desencajara.
        let r = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        let pushed = self.pushed.load(Ordering::Relaxed);
        // Invariante: `pushed` nunca decrece y `lines.len()` es lo que
        // sobrevivió de él, así que `pushed >= lines.len()` siempre — la resta
        // no puede desbordar por abajo.
        let base = pushed - r.lines.len() as u64;
        let cursor = cursor.min(pushed);
        let lost = base.saturating_sub(cursor);
        // `cursor` ya está acotado a `pushed`, y `base <= pushed`, así que
        // `cursor.max(base) >= base` siempre — la resta tampoco desborda.
        let start = cursor.max(base) - base;
        // `start` no cabe siempre en `usize` en un objetivo de 32 bits; el
        // `unwrap_or(usize::MAX)` es seguro porque el vector real jamás
        // supera `usize::MAX` elementos, así que un `start` que no cabe ya
        // es mayor que `r.lines.len()` — saltárselo entero da la misma lista
        // vacía que saltarse el `start` real habría dado.
        let lines: Vec<LogLine> = r
            .lines
            .iter()
            .skip(usize::try_from(start).unwrap_or(usize::MAX))
            .take(max)
            .cloned()
            .collect();
        let next = base + start + lines.len() as u64;
        Tail { lines, next, lost }
    }

    /// ¿Cuántas líneas de nivel `l` o peor retiene el anillo?
    ///
    /// Sin clonar nada, que es el punto: la barra de paneles lo pregunta en
    /// CADA frame para poner la cifra en el botón del registro, y contestarlo
    /// con [`Self::snapshot`] clonaba dos mil líneas —con sus dos `String`—
    /// diez veces por segundo, disputándole el candado al hilo que escribe.
    /// Contar es la misma pasada que preguntar si hay alguna: el anillo está
    /// acotado.
    #[must_use]
    pub fn count_at_or_above(&self, l: LogLevel) -> usize {
        let r = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        r.lines.iter().filter(|linea| linea.level <= l).count()
    }

    /// Mete una línea, tirando la más vieja si no cabe.
    fn push(&self, line: LogLine) {
        // `into_inner` y no descartar: dentro hay un `VecDeque` de datos, sin
        // ningún invariante que un pánico pudiera haber roto a medias. Antes,
        // un candado envenenado dejaba el panel enseñando «nada que enseñar»
        // para siempre — que es exactamente la mentira que este módulo dice no
        // querer contar.
        let mut r = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        if r.lines.len() == r.cap {
            r.lines.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        r.lines.push_back(line);
        self.pushed.fetch_add(1, Ordering::Relaxed);
    }
}

/// El filtro del anillo: su nivel configurable MÁS la cota de seguridad.
///
/// Es un `Filter` POR CAPA y no el filtro global del registro, y ahí está el
/// asunto entero: con un filtro global a INFO, los `DEBUG` no se emiten y
/// ningún panel puede enseñarlos después — filtrar en la ventana lo que nunca
/// se registró es imposible. Con el filtro por capa, el fichero conserva su
/// nivel y el anillo tiene el suyo.
pub struct RingFilter(LogRing);

impl<S> Filter<S> for RingFilter {
    fn enabled(&self, meta: &Metadata<'_>, _: &Context<'_, S>) -> bool {
        bajo_cota(meta.target(), *meta.level())
            && nivel_a_u8(*meta.level()) <= self.0.nivel.load(Ordering::Relaxed)
    }

    /// El tope estático que ve todo el proceso.
    ///
    /// Sin esto, `Filtered` devuelve `None` = «sin tope», y entonces el nivel
    /// máximo del proceso entero pasa a ser TRACE: cada `debug!` de cada crate
    /// —incluidos `hyper` y `russh` durante una transferencia— deja de cortarse
    /// por la comprobación barata y recorre la cadena de filtros. Es una
    /// regresión silenciosa de rendimiento que trajo el paso a filtros por
    /// capa, y va emparejada con la invalidación de caché de
    /// [`LogRing::set_level`].
    fn max_level_hint(&self) -> Option<tracing_subscriber::filter::LevelFilter> {
        Some(match self.0.level() {
            LogLevel::Error => tracing_subscriber::filter::LevelFilter::ERROR,
            LogLevel::Warn => tracing_subscriber::filter::LevelFilter::WARN,
            LogLevel::Info => tracing_subscriber::filter::LevelFilter::INFO,
            LogLevel::Debug => tracing_subscriber::filter::LevelFilter::DEBUG,
            LogLevel::Trace => tracing_subscriber::filter::LevelFilter::TRACE,
        })
    }

    fn callsite_enabled(&self, meta: &'static Metadata<'static>) -> tracing::subscriber::Interest {
        // `sometimes` y no `always`/`never`: el nivel cambia en caliente, así
        // que la respuesta de este callsite no se puede cachear. Cuesta una
        // comparación por evento y es lo que hace posible subir a DEBUG sin
        // reiniciar.
        if bajo_cota(meta.target(), *meta.level()) {
            tracing::subscriber::Interest::sometimes()
        } else {
            tracing::subscriber::Interest::never()
        }
    }
}

/// La capa que escribe en el anillo.
pub struct RingLayer(LogRing);

impl<S> tracing_subscriber::Layer<S> for RingLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut visitor = Aplanador::default();
        event.record(&mut visitor);
        let meta = event.metadata();
        self.0.push(LogLine {
            epoch_ms: ahora_ms(),
            level: nivel_de(*meta.level()),
            target: meta.target().to_string(),
            message: visitor.texto(),
        });
    }
}

/// La capa del anillo, YA con su cota puesta.
///
/// Devuelve una capa compuesta y no la pareja (capa, filtro): con la pareja, el
/// rustdoc prometía que no se podía instalar la capa sin su cota y el tipo no
/// lo impedía —bastaba con tirar el filtro—. Una regla 10 que depende de que el
/// llamante no se equivoque no es una regla.
#[must_use]
pub fn ring_layer<S>(ring: &LogRing) -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use tracing_subscriber::Layer as _;
    RingLayer(ring.clone()).with_filter(RingFilter(ring.clone()))
}

/// Aplana el mensaje y los campos de un evento a una línea.
///
/// El campo `message` va primero y sin nombre (es la frase); el resto va
/// detrás como `clave=valor`, que es lo mismo que hace el formato del fichero,
/// para que las dos superficies digan lo mismo.
#[derive(Default)]
struct Aplanador {
    mensaje: String,
    campos: String,
}

/// Tope de una línea guardada.
///
/// El anillo acotaba LÍNEAS y no bytes, y el mensaje no tiene tope: hay sitios
/// que formatean con `Debug` un valor ajeno —el error de un proveedor de IA,
/// por ejemplo, que es texto que decide un servidor remoto y viaja en un WARN,
/// dentro de lo que se captura por defecto—. Dos mil de esos son cientos de
/// megas residentes, y clonados en cada frame. Se corta y se DICE.
const MAX_LINEA: usize = 2048;

/// Lo que se añade a una línea cortada.
const CORTADA: &str = "… (cortada)";

impl Aplanador {
    fn texto(self) -> String {
        let entero = if self.campos.is_empty() {
            self.mensaje
        } else if self.mensaje.is_empty() {
            self.campos
        } else {
            format!("{} {}", self.mensaje, self.campos)
        };
        cortar(entero)
    }
}

/// Corta a [`MAX_LINEA`] por un límite de CARÁCTER, y lo marca.
///
/// Por carácter y no por byte: cortar a mitad de una secuencia UTF-8 daría un
/// `String` inválido (pánico) o, peor, unos bytes que el enmascarado de la
/// terminal ya no reconocería como lo que eran.
fn cortar(mut s: String) -> String {
    if s.len() <= MAX_LINEA {
        return s;
    }
    let corte = (0..=MAX_LINEA)
        .rev()
        .find(|i| s.is_char_boundary(*i))
        .unwrap_or(0);
    s.truncate(corte);
    s.push_str(CORTADA);
    s
}

impl tracing::field::Visit for Aplanador {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        if field.name() == "message" {
            let _ = write!(self.mensaje, "{value:?}");
        } else {
            if !self.campos.is_empty() {
                self.campos.push(' ');
            }
            let _ = write!(self.campos, "{}={value:?}", field.name());
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write as _;
        if field.name() == "message" {
            self.mensaje.push_str(value);
        } else {
            if !self.campos.is_empty() {
                self.campos.push(' ');
            }
            let _ = write!(self.campos, "{}={value}", field.name());
        }
    }
}

/// Milisegundos desde la época. Un reloj hacia atrás da 0, no un pánico: una
/// línea de log con hora rara es mejor que un frontend caído.
fn ahora_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linea(level: Level, target: &str, msg: &str) -> LogLine {
        LogLine {
            epoch_ms: 0,
            level: nivel_de(level),
            target: target.to_string(),
            message: msg.to_string(),
        }
    }

    /// La cifra del botón del registro: cuenta lo de ese nivel O PEOR, y
    /// nada más. Un `WARN` cuenta para `Warn`, un `ERROR` también, un `INFO`
    /// no.
    #[test]
    fn cuenta_las_lineas_de_un_nivel_o_peor() {
        let ring = LogRing::new(8);
        assert_eq!(ring.count_at_or_above(LogLevel::Warn), 0);
        ring.push(linea(Level::INFO, "a", "hola"));
        ring.push(linea(Level::WARN, "a", "ojo"));
        ring.push(linea(Level::ERROR, "a", "mal"));
        assert_eq!(ring.count_at_or_above(LogLevel::Warn), 2);
        assert_eq!(ring.count_at_or_above(LogLevel::Error), 1);
    }

    /// LA prueba del módulo (regla 10, #43): `suppaftp` loguea `PASS
    /// <password>` a TRACE, y el nivel de este anillo se sube desde la
    /// INTERFAZ. Sin la cota, pedir DEBUG en el panel pondría una contraseña
    /// de FTP en la pantalla.
    #[test]
    fn la_cota_de_suppaftp_no_la_levanta_ni_pedir_trace() {
        for nivel in [Level::TRACE, Level::DEBUG] {
            assert!(
                !bajo_cota("suppaftp", nivel),
                "{nivel} de suppaftp entró en el anillo"
            );
            assert!(
                !bajo_cota("suppaftp::command", nivel),
                "un módulo hijo de suppaftp se coló en {nivel}"
            );
        }
        // Lo que sí pasa: sus INFO y peores, y lo NUESTRO a cualquier nivel.
        assert!(bajo_cota("suppaftp", Level::INFO));
        assert!(bajo_cota("suppaftp", Level::WARN));
        assert!(bajo_cota("norte_core::connect", Level::TRACE));
    }

    /// Lo de TERCEROS no sube de INFO por mucho que el lector pida TRACE, y
    /// esto es el arreglo de un BLOCKER: la TUI embebe el core, así que en el
    /// mismo proceso están `russh`, `rustls`, `hyper` y `opendal`, que a ese
    /// nivel escriben cabeceras y búferes de red. Antes lo tapaba el filtro
    /// global a INFO; este módulo lo quitó del camino para poder subir el nivel
    /// en caliente, y sin lista blanca una tecla lo abría entero.
    #[test]
    fn lo_de_terceros_no_sube_de_info_aunque_se_pida_trace() {
        for target in [
            "russh::client",
            "russh_sftp::protocol",
            "rustls::conn",
            "hyper::proto::h1",
            "reqwest::async_impl",
            "opendal::services::s3",
            "h2::codec",
        ] {
            for nivel in [Level::DEBUG, Level::TRACE] {
                assert!(
                    !bajo_cota(target, nivel),
                    "«{target}» entró en el anillo en {nivel}"
                );
            }
            // Sus avisos y errores SÍ: son los que explican un fallo.
            assert!(bajo_cota(target, Level::INFO), "{target}");
            assert!(bajo_cota(target, Level::WARN), "{target}");
            assert!(bajo_cota(target, Level::ERROR), "{target}");
        }
    }

    /// Y lo nuestro sí sube, que es para lo que existe el panel.
    #[test]
    fn lo_nuestro_sube_hasta_trace() {
        for target in [
            "norte_core::connect",
            "norte_tui::navigate",
            "norte_vfs_local",
            "ntc",
        ] {
            assert!(
                bajo_cota(target, Level::TRACE),
                "«{target}» es nuestro y no pudo subir"
            );
        }
        // Y el binario a secas, con y sin ruta de módulo detrás: `norte` es un
        // crate de verdad, no solo un prefijo.
        for target in ["norte", "norte::daemon", "ntc::app"] {
            assert!(
                bajo_cota(target, Level::TRACE),
                "«{target}» es nuestro y no pudo subir"
            );
        }
    }

    /// La lista blanca casa por SEGMENTO de crate, no por prefijo crudo.
    ///
    /// Con `starts_with` sobre la cadena entera, una dependencia futura
    /// llamada `nortex` o `ntcp` habría entrado en TRACE en un anillo que
    /// cualquier cliente local sube con una tecla y lee en pantalla. Esta lista
    /// es la ÚNICA cota que deja fuera el `PASS <contraseña>` de `suppaftp`
    /// (#43, regla 10), así que las negativas están aquí para que el próximo
    /// refactor no la ensanche en silencio.
    #[test]
    fn un_crate_que_solo_empieza_igual_no_es_nuestro() {
        for target in [
            "nortex",
            "nortex::x",
            "nortexyz::client",
            "ntcp",
            "ntcp::session",
            "norteño",
        ] {
            for nivel in [Level::DEBUG, Level::TRACE] {
                assert!(
                    !bajo_cota(target, nivel),
                    "«{target}» no es nuestro y entró en el anillo en {nivel}"
                );
            }
            // Y sus INFO y peores siguen entrando, como los de cualquier
            // tercero: son los que explican un fallo.
            assert!(bajo_cota(target, Level::INFO), "{target}");
        }
    }

    /// El anillo tira las viejas y lo CUENTA: un panel que descarta en
    /// silencio miente sobre lo que hubo.
    #[test]
    fn al_llenarse_tira_las_viejas_y_lo_dice() {
        let r = LogRing::new(3);
        for i in 0..5 {
            r.push(linea(Level::INFO, "t", &format!("linea {i}")));
        }
        let v = r.snapshot();
        assert_eq!(v.len(), 3, "el anillo creció por encima de su tope");
        assert_eq!(v[0].message, "linea 2", "no tiró las MÁS VIEJAS");
        assert_eq!(v[2].message, "linea 4", "perdió la más reciente");
        assert_eq!(r.dropped(), 2);
    }

    /// El nivel se cambia en caliente y el anillo lo dice: es lo que permite
    /// pedir DEBUG desde el panel sin reiniciar.
    #[test]
    fn el_nivel_se_cambia_en_caliente() {
        let r = LogRing::new(10);
        assert_eq!(r.level(), LogLevel::Info, "arranca en INFO, no en DEBUG");
        r.set_level(LogLevel::Debug);
        assert_eq!(r.level(), LogLevel::Debug);
        // Y el clon comparte el mismo estado: la capa y quien pinta son dos
        // manos sobre el mismo anillo.
        let otro = r.clone();
        otro.set_level(LogLevel::Warn);
        assert_eq!(r.level(), LogLevel::Warn);
    }

    /// Un `cap` de cero no es un anillo que no guarda nada: es un error de
    /// configuración que dejaría el panel vacío para siempre sin decir por qué.
    #[test]
    fn un_tope_de_cero_se_corrige_a_uno() {
        let r = LogRing::new(0);
        r.push(linea(Level::INFO, "t", "algo"));
        assert_eq!(r.snapshot().len(), 1);
    }

    /// La capa montada de verdad recoge lo que pasa el filtro y NADA más.
    ///
    /// No usa el subscriber global (que es de una sola vez por proceso y lo
    /// comparten todos los tests): se monta uno local con
    /// `with_default`, que es lo mismo que hace la suite de `logging`.
    #[test]
    fn la_capa_montada_recoge_y_respeta_la_cota() {
        use tracing_subscriber::prelude::*;

        let r = LogRing::new(50);
        let sub = tracing_subscriber::registry().with(ring_layer(&r));

        tracing::subscriber::with_default(sub, || {
            tracing::info!(scheme = "s3", "conectando");
            tracing::debug!("esto no cabe todavía");
            // La contraseña que la cota existe para no dejar pasar (#43).
            tracing::trace!(target: "suppaftp", "PASS hunter2");
        });

        let v = r.snapshot();
        assert_eq!(v.len(), 1, "entró algo que no debía: {v:?}");
        assert_eq!(v[0].level, LogLevel::Info);
        assert_eq!(v[0].message, "conectando scheme=s3");
        assert!(v[0].target.starts_with("norte_config"), "{}", v[0].target);

        // Ahora se pide DEBUG desde la interfaz: entran los DEBUG y la
        // contraseña SIGUE fuera, que es el punto entero de la cota.
        r.set_level(LogLevel::Debug);
        let sub = tracing_subscriber::registry().with(ring_layer(&r));
        tracing::subscriber::with_default(sub, || {
            tracing::debug!("ahora sí");
            tracing::trace!(target: "suppaftp::command", "PASS hunter2");
            tracing::debug!(target: "suppaftp", "PASS hunter2");
        });
        let v = r.snapshot();
        assert!(
            v.iter().any(|l| l.message == "ahora sí"),
            "subir el nivel no trajo los DEBUG: {v:?}"
        );
        assert!(
            !v.iter().any(|l| l.message.contains("hunter2")),
            "UNA CONTRASEÑA ENTRÓ EN EL ANILLO: {v:?}"
        );
    }

    /// La cota aguanta por el camino REAL, que no es el que probaban los otros
    /// tests.
    ///
    /// `suppaftp` no emite eventos de `tracing`: emite `log::trace!`. El puente
    /// `tracing-log` despacha ese registro con el `target` estático `"log"` y
    /// deja el verdadero como campo, así que un `tracing::trace!(target:
    /// "suppaftp", …)` —lo que probaban los otros— NO recorre el mismo camino.
    /// La cota sobrevive porque el puente consulta `enabled` antes, con los
    /// metadatos verdaderos; eso es un detalle de implementación de terceros
    /// del que depende la regla 10, y por eso se fija aquí.
    ///
    /// Corolario para quien venga después: una comprobación defensiva sobre
    /// `meta.target()` DENTRO de `on_event` no cazaría nada, porque para
    /// entonces el target ya es `"log"`. Sería teatro.
    #[test]
    fn la_contrasena_no_entra_ni_por_el_puente_de_log() {
        use tracing_subscriber::prelude::*;

        // El puente es un global de proceso; instalarlo dos veces es error y no
        // pasa nada por ello (otro test del binario pudo hacerlo antes).
        let _ = tracing_log::LogTracer::init();
        log::set_max_level(log::LevelFilter::Trace);

        let r = LogRing::new(50);
        r.set_level(LogLevel::Trace);
        let sub = tracing_subscriber::registry().with(ring_layer(&r));
        tracing::subscriber::with_default(sub, || {
            log::trace!(target: "suppaftp", "PASS hunter2");
            log::trace!(target: "suppaftp::command", "PASS hunter2");
            // Y un tercero cualquiera al mismo nivel: tampoco entra.
            log::trace!(target: "russh::session", "session_write_encrypted, buf = [1, 2, 3]");
            // Lo que sí pasa: un aviso de un tercero, que explica fallos.
            log::warn!(target: "russh::session", "reconectando");
        });

        let v = r.snapshot();
        assert!(
            !v.iter().any(|l| l.message.contains("hunter2")),
            "UNA CONTRASEÑA ENTRÓ EN EL ANILLO POR EL PUENTE: {v:?}"
        );
        assert!(
            !v.iter().any(|l| l.message.contains("session_write")),
            "el TRACE de un tercero entró en el anillo: {v:?}"
        );
        assert!(
            v.iter().any(|l| l.message.contains("reconectando")),
            "el aviso de un tercero SÍ tiene que entrar: {v:?}"
        );
    }

    /// El caso normal: pides desde donde te quedaste y te dan lo nuevo.
    #[test]
    fn desde_un_cursor_llegan_solo_las_nuevas() {
        let anillo = LogRing::new(10);
        for i in 0..4 {
            anillo.push(linea(Level::INFO, "norte_core", &format!("l{i}")));
        }
        let t = anillo.since(2, 100);
        assert_eq!(t.lines.len(), 2);
        assert_eq!(t.lines[0].message, "l2");
        assert_eq!(t.next, 4);
        assert_eq!(t.lost, 0);
    }

    /// Un cursor de antes del desbordamiento DICE cuántas se perdió. Un hueco
    /// silencioso miente sobre lo que hubo, que es el motivo de que `dropped`
    /// exista.
    #[test]
    fn un_cursor_rancio_dice_cuantas_se_perdio() {
        let anillo = LogRing::new(3);
        for i in 0..7 {
            anillo.push(linea(Level::INFO, "norte_core", &format!("l{i}")));
        }
        // El anillo guarda l4,l5,l6: base = 7 - 3 = 4.
        let t = anillo.since(1, 100);
        assert_eq!(t.lost, 3, "se perdió l1, l2 y l3");
        assert_eq!(t.lines.len(), 3);
        assert_eq!(t.lines[0].message, "l4");
        assert_eq!(t.next, 7);
    }

    /// `max` acota la respuesta y el cursor avanza SOLO lo entregado: pedir de
    /// nuevo continúa donde se cortó, sin saltarse nada.
    #[test]
    fn max_acota_y_el_cursor_no_se_adelanta() {
        let anillo = LogRing::new(10);
        for i in 0..5 {
            anillo.push(linea(Level::INFO, "norte_core", &format!("l{i}")));
        }
        let t = anillo.since(0, 2);
        assert_eq!(t.lines.len(), 2);
        assert_eq!(t.next, 2);
        let t2 = anillo.since(t.next, 2);
        assert_eq!(t2.lines[0].message, "l2");
    }

    /// La capacidad es la que se pidió y NO se mueve con lo que entra: es lo
    /// que un lector remoto necesita para saber dónde está el fondo de la
    /// historia (ADR 0092).
    #[test]
    fn la_capacidad_dice_el_fondo_y_no_la_ocupacion() {
        let anillo = LogRing::new(3);
        assert_eq!(anillo.capacity(), 3, "vacío ya sabe cuánto le cabe");
        for i in 0..7 {
            anillo.push(linea(Level::INFO, "norte_core", &format!("l{i}")));
        }
        assert_eq!(anillo.capacity(), 3, "lleno y desbordado, la misma");
        // Un anillo de cero líneas no existe: `new` lo sube a una, y la
        // capacidad tiene que decir lo que hay, no lo que se pidió.
        assert_eq!(LogRing::new(0).capacity(), 1);
    }

    /// Un cursor del futuro —un daemon reiniciado bajo un cliente que guardó el
    /// suyo— no es un pánico ni un hueco: no hay nada nuevo y no se perdió nada.
    #[test]
    fn un_cursor_del_futuro_no_inventa_nada() {
        let anillo = LogRing::new(10);
        anillo.push(linea(Level::INFO, "norte_core", "l0"));
        let t = anillo.since(99, 100);
        assert!(t.lines.is_empty());
        assert_eq!(t.next, 1);
        assert_eq!(t.lost, 0);
    }

    /// El mensaje va delante y los campos detrás, como en el fichero: las dos
    /// superficies tienen que decir lo mismo para que una sirva de referencia
    /// de la otra.
    #[test]
    fn el_aplanador_pone_el_mensaje_delante() {
        let con = |mensaje: &str, campos: &str| {
            Aplanador {
                mensaje: mensaje.to_string(),
                campos: campos.to_string(),
            }
            .texto()
        };
        assert_eq!(
            con("conectando", "scheme=s3 host=un-bucket"),
            "conectando scheme=s3 host=un-bucket"
        );
        // Sin campos, solo la frase; sin frase, solo los campos.
        assert_eq!(con("hola", ""), "hola");
        assert_eq!(con("", "a=1"), "a=1");
    }
}
