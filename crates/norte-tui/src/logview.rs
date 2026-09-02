//! El panel de registro en la TUI: el kind, las teclas y la mitad remota.
//!
//! El estado (nivel, filtro, seguimiento del final, la fuente) vive en
//! [`norte_frontend::logpanel`] porque la ventana necesita el mismo, y las
//! líneas vienen del anillo de `norte_config::logring`. Aquí queda lo que es de
//! esta terminal: qué tecla hace qué, y cómo se junta lo de este proceso con lo
//! del daemon.
//!
//! # Por qué hay una mitad remota (#328)
//!
//! `ntc --socket <ruta>` habla con un daemon que es OTRO proceso: los
//! providers, el journal, la política y el motivo por el que una conexión
//! falló están al otro lado del socket, y este anillo solo tiene las líneas
//! de la propia terminal. Un panel que no lo dijera pareciría roto — alguien
//! lo abre justo cuando una conexión falla, no ve la línea que lo explica, y
//! concluye que el registro no funciona en vez de que está mirando otro sitio.
//!
//! La ventana resolvió esto primero (`norte_ui_host::controller::logpanel`) y
//! esto es la MISMA respuesta a propósito: una decisión que un frontend toma y
//! el otro no diverge en silencio (ADR 0077).

use norte_config::logline::{LogLevel, LogLine};
use norte_frontend::logpanel::LogSource;
use norte_i18n::{t, ta};

/// El kind que ocupa un hueco de registro.
pub const KIND: &str = "log";

/// Cuántas líneas se le piden al daemon en cada vuelta.
///
/// El daemon recorta a 1000, así que esto es una petición y no un contrato.
/// Quinientas porque una vuelta que no quepa NO pierde nada —lo que sobra
/// sigue después del cursor y lo recoge la vuelta siguiente— y porque el panel
/// enseña como mucho una pantalla.
pub const MAX_REMOTO: u32 = 500;

/// Techo de líneas del daemon que se guardan en memoria.
///
/// El anillo local ya tiene el suyo; éste es el mismo cuidado para el remoto,
/// porque aquí las líneas se ACUMULAN vuelta a vuelta y sin tope un panel
/// abierto toda una tarde crecería sin fin.
const MAX_LINEAS_REMOTAS: usize = 2000;

/// Qué se sabe del registro del DAEMON.
///
/// Tres valores y no un `bool`, porque «todavía no ha contestado» y «ha dicho
/// que no tiene registro» se enseñan distinto: lo primero no dice nada, y lo
/// segundo es una frase que el panel tiene que poner en pantalla. Colapsarlos
/// haría que un panel recién abierto afirmara una carencia que nadie ha
/// comprobado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Servicio {
    /// Nunca ha contestado: no se sabe.
    #[default]
    SinRespuesta,
    /// Sirve su registro: hay una segunda fuente de verdad.
    Sirve,
    /// Dijo que no tiene registro que servir.
    SinAnillo,
}

/// La mitad remota del panel de registro (#328).
///
/// Lo que NO está aquí es la petición en vuelo: en esta terminal eso es
/// `InFlight::log_tail`, del bucle de eventos, que es quien lanza y cosecha
/// todo lo que va al backend. La ventana lo lleva dentro porque allí el actor
/// es el único que escribe.
#[derive(Debug, Default)]
pub struct RegistroRemoto {
    /// ¿Hay un daemon del que hablar?
    ///
    /// Lo pone `main` una sola vez, de `Backend::is_remote`, y no cambia: el
    /// backend no cambia de brazo en vida del proceso. En `false` —un `ntc`
    /// corriente, que es el arranque POR DEFECTO— este panel es exactamente el
    /// de #326: un proceso, un anillo, y ni una palabra sobre un daemon.
    ///
    /// Sin este campo el panel mentía, y de dos maneras seguidas: el brazo
    /// embebido contesta `Unsupported` a `log.tail` con toda la razón —su
    /// anillo es el que este panel ya está leyendo—, así que el borde pasaba
    /// por «de este proceso (el daemon registra aparte)» durante el primer
    /// sondeo y se quedaba en «este daemon no sirve su registro» después. No
    /// hay ningún daemon. La frase estaba escrita para la otra degradación —un
    /// daemon de verdad compilado sin la feature `logging`— y aquí se la estaba
    /// poniendo a nadie.
    pub hay_daemon: bool,
    /// Lo que el daemon lleva entregado, de lo más viejo a lo más nuevo.
    ///
    /// Se acumula y no se repide entero en cada vuelta: el sondeo tira del
    /// anillo remoto con un cursor, así que cada respuesta trae solo lo nuevo.
    pub lineas: Vec<LogLine>,
    /// Por dónde iba. `None` = todavía no se ha preguntado, que es «dame lo
    /// que haya» y NO es lo mismo que cero: contra un anillo que ya dio la
    /// vuelta, un cero reportaría un `lost` falso en el primer sondeo.
    pub cursor: Option<u64>,
    /// Qué se sabe de si sirve su registro.
    pub servicio: Servicio,
    /// El nivel que contestó tener puesto, en forma de wire.
    ///
    /// Suyo y no nuestro: es global a todos sus clientes y solo sube, así que
    /// lo que se pidió y lo que hay puesto no tienen por qué coincidir.
    pub nivel: Option<String>,
    /// Cuántas líneas se cayeron por detrás de este cursor. Se acumulan: un
    /// hueco silencioso miente sobre lo que pasó.
    pub perdidas: u64,
    /// Qué apertura del panel es ésta.
    ///
    /// Entre pedir y contestar caben un cierre y una apertura, y la respuesta
    /// de la sesión anterior tiene que morir en vez de aterrizar —con su
    /// cursor— en el panel nuevo.
    pub epoca: u64,
    /// Un nivel que hay que pedirle al daemon, puesto por la tecla y drenado
    /// por el bucle.
    ///
    /// Mismo patrón que `App::places_wants_drives` y por lo mismo: `log.level`
    /// es I/O y `App` no tiene backend. Lo último pulsado gana — pedirle dos
    /// niveles seguidos a un anillo que solo sube es pedirle el mayor.
    pub pide_nivel: Option<LogLevel>,
}

impl RegistroRemoto {
    /// Empieza de cero, conservando lo que se sabe del daemon.
    ///
    /// Las líneas y el cursor son de ESTA apertura; que el daemon sirva o no
    /// su registro es un hecho sobre el daemon, y olvidarlo escondería la
    /// segunda fuente cada vez que se reabre el panel.
    ///
    /// Eso hace que un veredicto `SinAnillo` dure lo que dure el proceso, y es
    /// deliberado, no un descuido: ese estado sale de una feature de
    /// COMPILACIÓN del binario que hay al otro lado del socket (o de un montaje
    /// que le falló al arrancar), así que no puede cambiar bajo un daemon vivo.
    /// Lo que sí cambia —que un daemon se reinicie compilado de otra manera— es
    /// una conexión nueva, y ésa trae su propia sesión. Revisado y aparcado a
    /// propósito, para que no haya que volver a discutirlo.
    pub fn reiniciar(&mut self) {
        self.lineas.clear();
        self.cursor = None;
        self.perdidas = 0;
        self.pide_nivel = None;
        self.epoca = self.epoca.wrapping_add(1);
    }

    /// ¿Tiene sentido volver a preguntarle?
    ///
    /// A un daemon que ya dijo que no tiene registro NO se le vuelve a
    /// preguntar: la negativa no puede cambiar mientras ese daemon viva —sale
    /// de una feature de compilación o de un montaje que falló al arrancar—, y
    /// seguir sondeando serían dos RPC por segundo para siempre por una
    /// respuesta que no puede ser otra. Es asimétrico a propósito: lo POSITIVO
    /// sí hay que seguir pidiéndolo, porque el registro crece.
    ///
    /// No hay comparación de versiones en ningún sitio, y no la hay porque un
    /// daemon más viejo ni siquiera completa el `initialize`.
    ///
    /// Y a un daemon que no existe tampoco: sin `hay_daemon` no se pregunta
    /// nunca — ver ese campo.
    #[must_use]
    pub const fn debe_pedir(&self) -> bool {
        self.hay_daemon && !matches!(self.servicio, Servicio::SinAnillo)
    }
}

/// Una línea del cable a la forma que el panel pinta.
///
/// Un nivel que no se reconozca cae en `Info` en vez de tirar la línea: el
/// protocolo dice que un valor desconocido tiene que poder LLEGAR, y perder el
/// mensaje entero por no entender su etiqueta es peor que enseñarlo con la
/// etiqueta corriente.
fn linea_de_wire(l: norte_proto::methods::LogLine) -> LogLine {
    LogLine {
        epoch_ms: l.epoch_ms,
        level: LogLevel::from_wire(&l.level).unwrap_or(LogLevel::Info),
        target: l.target,
        message: l.message,
    }
}

/// La fuente que de verdad se está enseñando.
///
/// La preferencia se guarda tal cual (`LogPanel::source`), pero una fuente que
/// no existe no se puede enseñar, y el panel informa de lo que hay y no de lo
/// que se pidió. Se colapsa en las DOS direcciones, que son la misma regla
/// vista desde cada orilla:
///
/// - sin un anillo al otro lado (el caso embebido, o un daemon sin la feature
///   `logging`) todo cae a `Window`;
/// - sin anillo en ESTE proceso —nadie montó la capa— no hay nada local que
///   mezclar, así que todo cae a `Daemon`.
///
/// Con los dos anillos ausentes queda `Window`, que es donde vive la frase de
/// «sin registro instalado en este proceso»: no hay registro EN MEMORIA que
/// leer, y eso no es lo mismo que «no se registra nada».
#[must_use]
pub fn fuente_efectiva(app: &crate::app::App) -> LogSource {
    match (
        app.log_remote.servicio == Servicio::Sirve,
        app.log_ring.is_some(),
    ) {
        (true, true) => app.log_panel.source(),
        (true, false) => LogSource::Daemon,
        (false, _) => LogSource::Window,
    }
}

/// El anillo local, ya clonado. Vacío si no hay ninguno instalado.
///
/// Aparte de [`visibles`] porque el préstamo lo tiene que sostener quien
/// pinta: `merge` devuelve referencias a propósito, y el anillo ya clonó una
/// vez en su `snapshot`.
#[must_use]
pub fn instantanea(app: &crate::app::App) -> Vec<LogLine> {
    app.log_ring
        .as_ref()
        .map(norte_config::logring::LogRing::snapshot)
        .unwrap_or_default()
}

/// Lo que el panel enseña: las dos fuentes mezcladas y ya filtradas, cada
/// línea con el proceso del que salió.
#[must_use]
pub fn visibles<'a>(
    app: &'a crate::app::App,
    locales: &'a [LogLine],
) -> Vec<(&'a LogLine, LogSource)> {
    norte_frontend::logpanel::merge(locales, &app.log_remote.lineas, fuente_efectiva(app))
        .into_iter()
        .filter(|(l, _)| app.log_panel.matches(l))
        .collect()
}

/// Cómo se llama lo que se está enseñando. `None` = no hay nada que decir.
///
/// **Sin daemon no hay segmento**, y la ausencia ES la respuesta: un `ntc`
/// corriente tiene un proceso y un anillo, así que no hay dos cosas que
/// distinguir y cualquier frase sobre el origen sería contestar una pregunta
/// que nadie se ha hecho. Es el mismo razonamiento con el que la ventana
/// esconde su selector cuando no hay una segunda fuente, y deja el panel
/// exactamente como lo dejó #326.
///
/// Un daemon que ha dicho que no tiene registro que servir se dice AQUÍ y no
/// en una frase aparte, y es una diferencia con la ventana que tiene motivo: el
/// borde de un panel de terminal es una línea, no una fila de etiquetas que
/// pueda crecer, y las dos frases juntas —«de este proceso (el daemon registra
/// aparte)» y «este daemon no sirve su registro»— dicen lo mismo dos veces y
/// no caben. La segunda gana porque explica POR QUÉ no hay más que esto.
#[must_use]
pub fn etiqueta_de_fuente(app: &crate::app::App, fuente: LogSource) -> Option<String> {
    if !app.log_remote.hay_daemon {
        return None;
    }
    if app.log_remote.servicio == Servicio::SinAnillo {
        return Some(t("log-source-unsupported"));
    }
    Some(t(match fuente {
        // De ESTE proceso, y decirlo es el punto: con `--socket`, aquí NO está
        // lo del daemon —los providers, el journal, la política—, que es la
        // mitad interesante.
        LogSource::Window if app.log_ring.is_some() => "log-source-window",
        // No es «no se registra nada»: el proceso sigue escribiendo a su
        // fichero. Lo que falta es el anillo en memoria, que es lo que este
        // panel lee.
        LogSource::Window => "log-no-ring",
        LogSource::Daemon => "log-source-daemon",
        LogSource::Both => "log-source-both",
    }))
}

/// De quién es el nivel que se acaba de subir. Vacío = de nadie más que de
/// este proceso, y entonces no hay nada que anunciar.
///
/// **Siempre que el daemon sea una de las fuentes que se leen**, no solo
/// cuando es la única: en `Both`, que es lo que trae el panel al abrirse,
/// pulsar `t` sube un anillo GLOBAL del daemon, compartido con todos sus
/// clientes, que no vuelve a bajar y que cerrar este panel no baja. Callarlo
/// en el camino corriente dejaría esa decisión sin anunciar.
///
/// Va a la barra de estado y no al borde del panel, y aquí las dos ventanas se
/// separan: la de la ventana es una fila de etiquetas que crece, y el borde de
/// un panel de terminal es UNA línea que `ratatui` recorta en silencio — con
/// esta frase puesta ahí, a 120 columnas ya no cabía la nota de captura, que es
/// la que dice el nivel del daemon. Y la barra de estado es además el sitio
/// donde esta terminal explica lo que acaba de hacer una tecla, que es
/// exactamente lo que esto es.
///
/// Por la fuente EFECTIVA y no por la preferencia: se anuncia lo que de verdad
/// se ha subido. La petición sí sale por la preferencia —ver
/// [`aplicar_accion`]—, porque es una de las dos formas de averiguar si ese
/// daemon sabe de registro.
#[must_use]
pub fn aviso_de_nivel(app: &crate::app::App) -> String {
    if fuente_efectiva(app) == LogSource::Window {
        String::new()
    } else {
        t("log-source-daemon-level")
    }
}

/// Qué anillo está guardando MÁS de lo que se enseña, y cuál.
///
/// Solo cuando se captura de más: decir «capturando info» sobre un panel que
/// enseña info sería ruido, y el ruido es lo que hace que se deje de leer la
/// línea que sí importa.
///
/// Con una sola fuente la frase no nombra el anillo —no hay otro con el que
/// confundirlo—; con las dos, cada parte dice de quién habla. Que aquí aparezca
/// el nivel del daemon es lo que hace legible la regla entera: el suyo es
/// global a sus clientes y solo sube, así que puede estar muy por encima del
/// que este panel enseña, y ese hueco es exactamente lo que esta frase existe
/// para no callar.
#[must_use]
pub fn nota_de_captura(app: &crate::app::App, fuente: LogSource) -> String {
    let ensena = app.log_panel.level();
    let local = app
        .log_ring
        .as_ref()
        .map(norte_config::logring::LogRing::level)
        .filter(|cap| *cap > ensena);
    let remoto = app
        .log_remote
        .nivel
        .as_deref()
        .and_then(LogLevel::from_wire)
        .filter(|cap| *cap > ensena);
    let frase = |clave, cap: LogLevel| ta(clave, &[("level", cap.label().trim())]);
    let partes: Vec<String> = match fuente {
        LogSource::Window => local
            .map(|c| frase("log-capturing", c))
            .into_iter()
            .collect(),
        LogSource::Daemon => remoto
            .map(|c| frase("log-capturing-daemon", c))
            .into_iter()
            .collect(),
        LogSource::Both => local
            .map(|c| frase("log-capturing-window", c))
            .into_iter()
            .chain(remoto.map(|c| frase("log-capturing-daemon", c)))
            .collect(),
    };
    partes.join(" · ")
}

/// Las líneas que se han perdido, por anillo y DICIENDO de cuál.
///
/// Dos números y no uno, porque no significan lo mismo y no viven lo mismo: el
/// del anillo local cuenta lo que ha evacuado desde que arrancó el proceso y no
/// se reinicia nunca; el del daemon cuenta lo que ESTA apertura del panel se
/// perdió, y vuelve a cero al reabrirlo. Sumarlos daría un número que no es
/// ninguna de las dos cosas.
///
/// De cada anillo solo se habla si se está leyendo: avisar de un hueco en un
/// registro que no está en pantalla es una alarma sobre nada.
#[must_use]
pub fn nota_de_descartes(app: &crate::app::App, fuente: LogSource) -> String {
    let mut partes: Vec<String> = Vec::new();
    let locales = app
        .log_ring
        .as_ref()
        .map_or(0, norte_config::logring::LogRing::dropped);
    if locales > 0 && fuente != LogSource::Daemon {
        partes.push(ta(
            // Sin nombrar el anillo cuando es el único que se lee.
            if fuente == LogSource::Window {
                "log-dropped"
            } else {
                "log-dropped-window"
            },
            &[("n", &locales.to_string())],
        ));
    }
    if app.log_remote.perdidas > 0 && fuente != LogSource::Window {
        partes.push(ta(
            "log-missed-daemon",
            &[("n", &app.log_remote.perdidas.to_string())],
        ));
    }
    partes.join(" · ")
}

/// Aterriza lo que el daemon contestó a `log.tail` (#328).
pub fn aterrizar_tail(
    app: &mut crate::app::App,
    epoca: u64,
    res: Result<norte_proto::methods::LogTailResult, norte_proto::Error>,
) {
    if epoca != app.log_remote.epoca {
        // De una apertura anterior: ni sus líneas ni su cursor valen ya.
        return;
    }
    match res {
        Ok(r) => {
            app.log_remote.servicio = Servicio::Sirve;
            app.log_remote.nivel = Some(r.level);
            app.log_remote.cursor = Some(r.next);
            app.log_remote.perdidas = app.log_remote.perdidas.saturating_add(r.lost);
            app.log_remote
                .lineas
                .extend(r.lines.into_iter().map(linea_de_wire));
            // El tope se aplica por delante: lo viejo es lo que se tira, igual
            // que en el anillo, y cuenta como perdido — que es lo que impide
            // que el recorte deje un hueco callado.
            let sobra = app
                .log_remote
                .lineas
                .len()
                .saturating_sub(MAX_LINEAS_REMOTAS);
            if sobra > 0 {
                app.log_remote.lineas.drain(..sobra);
                app.log_remote.perdidas = app
                    .log_remote
                    .perdidas
                    .saturating_add(sobra.try_into().unwrap_or(u64::MAX));
            }
        }
        // La ÚNICA degradación alcanzable: un daemon de la misma versión sin
        // la feature `logging` (o el brazo embebido, que no tiene una segunda
        // fuente que ofrecer). Ver `RegistroRemoto::debe_pedir`.
        Err(norte_proto::Error::Unsupported) => app.log_remote.servicio = Servicio::SinAnillo,
        // Un fallo cualquiera —la conexión se cayó, el daemon está ocupado— NO
        // es «este daemon no tiene registro»: decirlo sería acusar de una
        // carencia permanente a algo que se arregla solo en la vuelta
        // siguiente. Se calla y se reintenta.
        Err(_) => {}
    }
}

/// Aterriza el nivel que el daemon dejó puesto de verdad (#328).
pub fn aterrizar_nivel(
    app: &mut crate::app::App,
    epoca: u64,
    res: Result<String, norte_proto::Error>,
) {
    if epoca != app.log_remote.epoca {
        return;
    }
    match res {
        Ok(nivel) => {
            app.log_remote.servicio = Servicio::Sirve;
            app.log_remote.nivel = Some(nivel);
        }
        Err(norte_proto::Error::Unsupported) => app.log_remote.servicio = Servicio::SinAnillo,
        Err(_) => {}
    }
}

/// Cuántas filas avanza una página.
///
/// El ALTO ya no se adivina aquí —lo pone quien pinta, por frame
/// (`LogPanel::set_viewport_rows`)—; esto es solo cuánto salta `AvPág`.
const PAGINA: isize = 10;

/// Lo que una tecla le pide al panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogAction {
    /// Enseñar hasta este nivel.
    Level(norte_config::logline::LogLevel),
    /// Subir o bajar `n` líneas.
    Scroll(isize),
    /// Volver a pegarse al final.
    Follow,
    /// Recorrer la fuente: este proceso, el daemon, los dos (#328).
    Source,
    /// Empezar a teclear un filtro.
    StartFilter,
    /// Devolver el teclado.
    Leave,
}

/// Traduce una tecla del panel de registro.
///
/// Un `match` explícito y NO el keymap: estas teclas solo existen mientras el
/// panel tiene el teclado, son de una sola letra, y meterlas en el keymap
/// obligaría a los siete presets a declarar seis atajos que fuera de aquí no
/// significan nada. Es el mismo criterio que el selector de conexiones y el de
/// disposición, y es también por lo que `s` (la fuente, #328) no aparece en
/// ningún preset: no es un comando del catálogo, es una tecla de este panel,
/// como `e`, `w`, `i`, `d`, `t` y `/`.
#[must_use]
pub fn key(
    code: crossterm::event::KeyCode,
    mods: crossterm::event::KeyModifiers,
) -> Option<LogAction> {
    use crossterm::event::{KeyCode, KeyModifiers};
    use norte_config::logline::LogLevel;
    if !(mods.is_empty() || mods == KeyModifiers::SHIFT) {
        return None;
    }
    Some(match code {
        KeyCode::Char('e') => LogAction::Level(LogLevel::Error),
        KeyCode::Char('w') => LogAction::Level(LogLevel::Warn),
        KeyCode::Char('i') => LogAction::Level(LogLevel::Info),
        KeyCode::Char('d') => LogAction::Level(LogLevel::Debug),
        KeyCode::Char('t') => LogAction::Level(LogLevel::Trace),
        // La inicial de «source»/«fuente», y la única letra suelta que quedaba
        // libre entre las cinco de nivel.
        KeyCode::Char('s') => LogAction::Source,
        KeyCode::Char('/') => LogAction::StartFilter,
        KeyCode::Up => LogAction::Scroll(-1),
        KeyCode::Down => LogAction::Scroll(1),
        KeyCode::PageUp => LogAction::Scroll(-PAGINA),
        KeyCode::PageDown => LogAction::Scroll(PAGINA),
        // `End` es «vuelve a lo último», que es distinto de bajar mucho: tras
        // un filtro nuevo la lista cambia de largo y bajar a ciegas no acierta.
        KeyCode::End => LogAction::Follow,
        // Y `Inicio`, al principio de lo que quede: quien tiene `Fin` lo busca.
        // `isize::MIN` no, que se desbordaría al negarlo — el desplazamiento se
        // acota solo contra el tope.
        KeyCode::Home => LogAction::Scroll(isize::MIN + 1),
        KeyCode::Esc => LogAction::Leave,
        _ => return None,
    })
}

/// Aplica una tecla al panel de registro de `app`.
///
/// Subir el nivel del PANEL sube también el del ANILLO cuando hace falta: sin
/// eso, pedir DEBUG filtraría a DEBUG unas líneas que se guardaron a INFO, o
/// sea que enseñaría exactamente nada y parecería roto. Bajarlo no baja el del
/// anillo — ver la nota de [`norte_frontend::logpanel`].
pub fn apply(
    app: &mut crate::app::App,
    resolver: &mut crate::keymap::Resolver,
    mods: crossterm::event::KeyModifiers,
    code: crossterm::event::KeyCode,
) {
    use crossterm::event::{KeyCode, KeyModifiers};
    // `Ctrl+C` ANTES que nada, también con el campo de filtro abierto: es la
    // salida de emergencia, y todos los demás manejadores de este árbol la
    // comprueban primero. Estaba después y escribir un filtro dejaba al lector
    // sin forma de salir del programa.
    if mods.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit = true;
        return;
    }
    // Con el campo de filtro abierto, el resto de teclas son suyas: si no, una
    // `d` a mitad de una palabra cambiaría el nivel en vez de escribirse.
    if app.log_filter_input.is_some() {
        editar_filtro(app, mods, code);
        return;
    }
    let Some(accion) = key(code, mods) else {
        // Lo que este panel NO es suyo sigue su camino por el keymap, y esto
        // no es un detalle: sin ello el propio `layout.log` moría aquí y el
        // panel no se podía cerrar con la misma tecla que lo abrió. Un panel
        // que se queda TODAS las teclas secuestra el teclado en vez de
        // tomarlo.
        pasar_al_keymap(app, resolver, mods, code);
        return;
    };
    aplicar_accion(app, accion);
}

/// Lo que hace cada acción del panel.
///
/// Separado de [`apply`] para que se pueda probar sin montar un resolver de
/// teclas: lo que estas líneas deciden —cuándo sube el nivel del anillo y
/// cuándo no— es el invariante del panel, no la traducción de una tecla.
pub fn aplicar_accion(app: &mut crate::app::App, accion: LogAction) {
    match accion {
        LogAction::Level(l) => {
            app.log_panel.show_level(l);
            // Y el anillo captura AL MENOS eso: filtrar a DEBUG lo que se
            // guardó a INFO no enseñaría nada y parecería roto. `raise_to`
            // nunca baja — ver su rustdoc.
            if let Some(ring) = app.log_ring.as_ref() {
                ring.raise_to(l);
            }
            // Y, si el daemon es una de las fuentes, se lo pide TAMBIÉN a él
            // (#328): su anillo es suyo, y sin subirlo las líneas que se están
            // pidiendo no llegan a existir al otro lado.
            //
            // Por la PREFERENCIA y no por la fuente efectiva: quien ha elegido
            // leer el daemon está pidiendo su nivel aunque todavía no haya
            // contestado, y la respuesta a esta llamada es justamente una de
            // las dos formas de averiguar si sabe de registro.
            //
            // Y solo si hay alguien a quien pedírselo: sin daemon, o con uno
            // que ya dijo que no tiene anillo, esto sería estado muerto que
            // nadie drena.
            if app.log_panel.source() != LogSource::Window && app.log_remote.debe_pedir() {
                app.log_remote.pide_nivel = Some(l);
            }
            // Y se DICE, porque ese anillo no es de este proceso: es global a
            // todos los clientes del daemon y no vuelve a bajar. Ver
            // [`aviso_de_nivel`] para por qué va a la barra y no al borde.
            let aviso = aviso_de_nivel(app);
            if !aviso.is_empty() {
                app.message = Some(aviso);
            }
        }
        // Sin una segunda fuente no hace nada: recorrer tres vistas de un mismo
        // anillo sería un mando que promete algo que no existe, y cambiar la
        // preferencia por debajo dejaría al lector con una fuente que no pidió
        // el día que sí haya daemon. Es lo mismo que hace la ventana, donde el
        // selector directamente no se pinta.
        LogAction::Source => {
            if app.log_remote.servicio == Servicio::Sirve {
                app.log_panel.cycle_source();
            }
        }
        LogAction::Scroll(n) => {
            // Solo aquí se cuenta lo visible: hacerlo para cada tecla recorría
            // el anillo entero también al cambiar de nivel o al abrir el
            // filtro, que no desplazan nada.
            //
            // Sobre la lista MEZCLADA, que es la que se ve: contar solo las
            // locales dejaría el tope corto y una página no llegaría al final.
            let locales = instantanea(app);
            let cuantas = visibles(app, &locales).len();
            if n < 0 {
                app.log_panel.scroll_up(n.unsigned_abs(), cuantas);
            } else {
                app.log_panel
                    .scroll_down(usize::try_from(n).unwrap_or(0), cuantas);
            }
        }
        LogAction::Follow => app.log_panel.follow(),
        // El filtro se teclea en el mismo campo que el resto de entradas de
        // una línea del TUI; abrirlo es lo que hace `/`.
        // Se abre con lo que ya estaba filtrando, no en blanco: afinar un
        // filtro es lo normal, y volver a teclearlo entero, no.
        LogAction::StartFilter => {
            app.log_filter_input = Some(app.log_panel.filter().to_string());
        }
        // Suelta las teclas SIN cerrar el panel: cerrar algo que el lector solo
        // quería dejar de manejar es la respuesta equivocada, y cerrarlo ya lo
        // hace `alt+l` otra vez.
        LogAction::Leave => app.return_keys_to_panes(),
    }
}

/// Resuelve por el keymap lo que este panel no reclama, y lo despacha por el
/// mismo camino que el panel de procesos (`App::processes_command`, que ya
/// atiende los `layout.*`).
fn pasar_al_keymap(
    app: &mut crate::app::App,
    resolver: &mut crate::keymap::Resolver,
    mods: crossterm::event::KeyModifiers,
    code: crossterm::event::KeyCode,
) {
    use crate::keymap::Resolution;
    let Some(chord) = crate::keymap::chord_from_crossterm(mods, code) else {
        return; // tecla no modelada por el keymap: ignorar
    };
    let cmd = match resolver.push(chord) {
        Resolution::Run { command, .. } => command,
        Resolution::Pending(_) | Resolution::Counting(_) | Resolution::Unavailable { .. } => {
            resolver.reset();
            return;
        }
        Resolution::Reset => return,
    };
    app.log_command(&cmd);
}

/// Teclas mientras se escribe el filtro.
///
/// `Esc` cancela y deja el filtro ANTERIOR, no lo borra: cancelar es
/// «déjalo como estaba», y en un panel de log borrar el filtro por accidente
/// devuelve mil líneas encima de lo que estabas leyendo.
fn editar_filtro(
    app: &mut crate::app::App,
    mods: crossterm::event::KeyModifiers,
    code: crossterm::event::KeyCode,
) {
    use crossterm::event::{KeyCode, KeyModifiers};
    let Some(texto) = app.log_filter_input.as_mut() else {
        return;
    };
    match code {
        KeyCode::Char(c) if mods.is_empty() || mods == KeyModifiers::SHIFT => texto.push(c),
        KeyCode::Backspace => {
            texto.pop();
        }
        KeyCode::Enter => {
            let texto = app.log_filter_input.take().unwrap_or_default();
            app.log_panel.set_filter(texto);
        }
        KeyCode::Esc => app.log_filter_input = None,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use norte_config::logline::LogLevel;

    /// Las cinco letras de nivel están y son las iniciales del nivel en
    /// inglés, que es como se llaman en el propio log.
    #[test]
    fn cada_nivel_tiene_su_letra() {
        let esperado = [
            ('e', LogLevel::Error),
            ('w', LogLevel::Warn),
            ('i', LogLevel::Info),
            ('d', LogLevel::Debug),
            ('t', LogLevel::Trace),
        ];
        for (c, nivel) in esperado {
            assert_eq!(
                key(KeyCode::Char(c), KeyModifiers::empty()),
                Some(LogAction::Level(nivel)),
                "la tecla «{c}» no pide {nivel:?}"
            );
        }
    }

    /// Una tecla con Ctrl NO es de este panel: `ctrl+c` sale del programa y
    /// `ctrl+…` son atajos globales. Tragárselos aquí sería secuestrarlos.
    #[test]
    fn los_atajos_con_control_no_se_los_queda() {
        assert_eq!(key(KeyCode::Char('c'), KeyModifiers::CONTROL), None);
        assert_eq!(key(KeyCode::Char('d'), KeyModifiers::CONTROL), None);
    }

    /// Un acorde con modificador NO lo reclama este panel, y ahí estaba el
    /// fallo: `alt+l` es el comando que abre y cierra el registro, y mientras
    /// el panel tenía el teclado se lo tragaba entero — o sea que la misma
    /// tecla que lo abría no lo cerraba. `key` diciendo `None` es lo que manda
    /// la tecla al keymap; si algún día reclama un `alt+…`, este test cae.
    #[test]
    fn los_acordes_con_modificador_siguen_su_camino() {
        for (code, mods) in [
            (KeyCode::Char('l'), KeyModifiers::ALT),
            (KeyCode::Char('j'), KeyModifiers::ALT),
            (KeyCode::F(9), KeyModifiers::empty()),
        ] {
            assert_eq!(
                key(code, mods),
                None,
                "{code:?}+{mods:?} se lo quedó el panel en vez de dejarlo pasar"
            );
        }
    }

    /// El panel SUBE el nivel del anillo y NUNCA lo baja, y cerrar el panel es
    /// lo único que lo devuelve a donde estaba.
    ///
    /// Las tres mitades importan. Sin subirlo, filtrar a DEBUG lo que se guardó
    /// a INFO no enseña nada y parece roto. Sin el «nunca baja», ir a DEBUG,
    /// volver a WARN y pedir DEBUG otra vez borraría justo el rato que estabas
    /// investigando. Y sin bajarlo al cerrar, una sola pulsación de `t` deja el
    /// proceso capturando TRACE el resto de la sesión, con su coste, mucho
    /// después de que nadie mire.
    #[test]
    fn el_nivel_del_anillo_sube_no_baja_y_vuelve_al_cerrar() {
        use norte_config::logring::LogRing;
        let mut app = crate::app::testutil::app_dos_panes();
        let anillo = LogRing::new(10);
        app.log_ring = Some(anillo.clone());
        app.toggle_log(); // abre y toma el teclado

        aplicar_accion(&mut app, LogAction::Level(LogLevel::Debug));
        assert_eq!(anillo.level(), LogLevel::Debug, "pedir DEBUG no lo subió");
        aplicar_accion(&mut app, LogAction::Level(LogLevel::Warn));
        assert_eq!(
            anillo.level(),
            LogLevel::Debug,
            "bajar lo que se ENSEÑA no puede dejar de capturar"
        );
        assert_eq!(app.log_panel.level(), LogLevel::Warn);

        app.toggle_log(); // cierra
        assert_eq!(
            anillo.level(),
            LogLevel::Warn,
            "cerrar el panel tiene que devolver el anillo a lo que se enseñaba"
        );
    }

    /// La allowlist del registro NO es la de procesos: allí `dialog.confirm`
    /// cancela la tarea bajo el cursor, y aquí no hay nada que confirmar. Un
    /// `Enter` que cancela una copia desde un visor de log es justo el
    /// accidente que una allowlist existe para impedir.
    #[test]
    fn confirmar_es_inerte_en_el_registro_y_su_propia_tecla_lo_cierra() {
        let mut app = crate::app::testutil::app_dos_panes();
        app.toggle_log();
        assert!(app.log_slot().is_some(), "no se abrió");

        // Inerte: ni cierra el panel ni cambia de dueño del teclado.
        app.log_command("dialog.confirm");
        assert!(app.log_slot().is_some());
        assert_eq!(app.key_owner(), crate::app::KeyOwner::Log);

        // Y lo suyo sí: la misma tecla que lo abrió lo cierra desde dentro.
        app.log_command("layout.log");
        assert!(app.log_slot().is_none(), "no se cerró desde dentro");
    }

    /// `End` no es «baja mucho»: tras cambiar el filtro la lista cambia de
    /// largo, y volver al final tiene que ser una orden, no una apuesta.
    #[test]
    fn el_final_es_una_orden_propia() {
        assert_eq!(
            key(KeyCode::End, KeyModifiers::empty()),
            Some(LogAction::Follow)
        );
        assert_eq!(
            key(KeyCode::PageDown, KeyModifiers::empty()),
            Some(LogAction::Scroll(PAGINA))
        );
    }

    // --- La mitad remota (#328) -------------------------------------------

    /// Una línea del cable, ya en forma de presentación.
    fn wire(epoch_ms: i64, level: &str, msg: &str) -> norte_proto::methods::LogLine {
        norte_proto::methods::LogLine {
            epoch_ms,
            level: level.to_owned(),
            target: "norte_core::daemon".to_owned(),
            message: msg.to_owned(),
        }
    }

    /// Una respuesta de `log.tail` con lo justo.
    fn tail(
        lines: Vec<norte_proto::methods::LogLine>,
        next: u64,
        lost: u64,
    ) -> norte_proto::methods::LogTailResult {
        norte_proto::methods::LogTailResult {
            lines,
            next,
            lost,
            level: "info".to_owned(),
            capacity: 2000,
        }
    }

    /// Mete líneas en el anillo por donde entran de verdad: la capa de
    /// `tracing`.
    ///
    /// `LogRing::push` es privado a propósito, y no debe dejar de serlo — el
    /// filtro por el que pasa la capa es donde vive la cota de `suppaftp`, que
    /// loguea `PASS <contraseña>` a nivel TRACE. Un atajo para los tests que se
    /// saltara esa cota probaría un camino que no existe.
    fn con_lineas(anillo: &norte_config::logring::LogRing, f: impl FnOnce()) {
        use tracing_subscriber::layer::SubscriberExt as _;
        let s = tracing_subscriber::registry().with(norte_config::logring::ring_layer(anillo));
        tracing::subscriber::with_default(s, f);
    }

    /// Una app con el panel abierto, un anillo local con una línea y el daemon
    /// contestando otra. Es el montaje de `ntc --socket`: dos procesos, dos
    /// anillos.
    ///
    /// La línea del daemon se fecha UN milisegundo después de la local, leída
    /// del propio anillo: la hora la pone el reloj al registrar, así que
    /// inventarse aquí un `epoch_ms` pequeño pondría al daemon en 1970 y la
    /// mezcla saldría al revés por un motivo que no tiene nada que ver con lo
    /// que el test mira.
    fn app_con_las_dos_fuentes() -> crate::app::App {
        use norte_config::logring::LogRing;
        let mut app = crate::app::testutil::app_dos_panes();
        let anillo = LogRing::new(10);
        con_lineas(&anillo, || tracing::info!("de esta terminal"));
        let local_ms = anillo.snapshot()[0].epoch_ms;
        app.log_ring = Some(anillo);
        // Lo que `main` pone de `Backend::is_remote`: hay un segundo proceso.
        app.log_remote.hay_daemon = true;
        app.toggle_log();
        let epoca = app.log_remote.epoca;
        aterrizar_tail(
            &mut app,
            epoca,
            Ok(tail(vec![wire(local_ms + 1, "info", "del daemon")], 7, 0)),
        );
        app
    }

    /// Con un daemon aparte, el panel enseña LAS DOS fuentes, en orden de
    /// tiempo y sabiendo de cuál es cada línea.
    ///
    /// Es el agujero de #328 y la misma respuesta que la ventana (ADR 0077):
    /// con `--socket`, los providers, el journal, la política y el motivo por
    /// el que una conexión falló están en el otro proceso. Arreglarlo en un
    /// solo frontend es lo que hace que los dos diverjan en silencio.
    #[test]
    fn la_vista_mezcla_la_terminal_y_el_daemon() {
        let app = app_con_las_dos_fuentes();
        let locales = instantanea(&app);
        let filas = visibles(&app, &locales);
        let textos: Vec<&str> = filas.iter().map(|(l, _)| l.message.as_str()).collect();
        assert_eq!(
            textos,
            ["de esta terminal", "del daemon"],
            "la mezcla no llegó a las filas"
        );
        assert_eq!(filas[0].1, LogSource::Window);
        assert_eq!(filas[1].1, LogSource::Daemon);
    }

    /// La fuente EFECTIVA no es la preferencia guardada: una fuente que no
    /// existe no se puede enseñar, y el panel informa de lo que hay y no de lo
    /// que se pidió. Se colapsa en las dos direcciones.
    #[test]
    fn la_fuente_efectiva_colapsa_hacia_el_anillo_que_existe() {
        let mut app = app_con_las_dos_fuentes();
        assert_eq!(
            fuente_efectiva(&app),
            LogSource::Both,
            "con los dos anillos"
        );

        app.log_ring = None;
        assert_eq!(
            fuente_efectiva(&app),
            LogSource::Daemon,
            "sin anillo local no hay nada de esta terminal que mezclar"
        );

        app.log_remote.servicio = Servicio::SinAnillo;
        assert_eq!(
            fuente_efectiva(&app),
            LogSource::Window,
            "sin registro al otro lado no se puede enseñar el del daemon"
        );
    }

    /// El nivel que se MARCA es el que se enseña, siempre; el del daemon se
    /// dice en la nota de captura, que es el sitio que ya significa «se recoge
    /// más de lo que se ve».
    ///
    /// Marcar el del daemon fue el peor fallo del primer intento de la
    /// ventana: el filtro sigue siendo el del panel, así que con el daemon en
    /// `trace` y el panel en `info` la cabecera decía `trace` mientras cada
    /// línea `debug` cruzaba el socket y se tiraba en silencio.
    #[test]
    fn el_nivel_del_daemon_va_en_la_captura_y_no_en_el_nivel() {
        let mut app = app_con_las_dos_fuentes();
        app.log_remote.nivel = Some("trace".to_owned());
        assert_eq!(
            app.log_panel.level(),
            LogLevel::Info,
            "el nivel del panel lo mueven las teclas, no el daemon"
        );
        let nota = nota_de_captura(&app, fuente_efectiva(&app));
        assert!(
            nota.contains(LogLevel::Trace.label().trim()),
            "la captura no dice el nivel del daemon: {nota:?}"
        );
        assert!(
            nota.contains(&norte_i18n::ta(
                "log-capturing-daemon",
                &[("level", LogLevel::Trace.label().trim())]
            )),
            "la captura no dice DE QUIÉN es ese nivel: {nota:?}"
        );
    }

    /// Subir el anillo del daemon se anuncia SIEMPRE que el daemon sea una de
    /// las fuentes, no solo cuando es la única: en `Both` —que es como abre el
    /// panel— pulsar `t` sube un anillo GLOBAL del daemon que no vuelve a
    /// bajar, y callarlo dejaría esa decisión sin anunciar.
    #[test]
    fn subir_el_anillo_del_daemon_se_anuncia_tambien_en_mezcla() {
        let mut app = app_con_las_dos_fuentes();
        for fuente in [LogSource::Both, LogSource::Daemon] {
            app.log_panel.set_source(fuente);
            app.message = None;
            aplicar_accion(&mut app, LogAction::Level(LogLevel::Trace));
            assert_eq!(
                app.message.as_deref(),
                Some(norte_i18n::t("log-source-daemon-level").as_str()),
                "no se anuncia con la fuente {fuente:?}"
            );
        }
        app.log_panel.set_source(LogSource::Window);
        app.message = None;
        aplicar_accion(&mut app, LogAction::Level(LogLevel::Warn));
        assert_eq!(
            app.message, None,
            "leyendo solo esta terminal no hay ningún anillo ajeno que subir"
        );
    }

    /// Que el daemon NO sirve su registro se dice en la etiqueta de la fuente,
    /// que es la que ocupa el borde: el lector creería, si no, que la mitad
    /// interesante simplemente no ocurre.
    #[test]
    fn un_daemon_sin_registro_lo_dice_la_etiqueta_de_la_fuente() {
        let mut app = app_con_las_dos_fuentes();
        assert_eq!(
            etiqueta_de_fuente(&app, LogSource::Both),
            Some(norte_i18n::t("log-source-both"))
        );
        app.log_remote.servicio = Servicio::SinAnillo;
        assert_eq!(
            etiqueta_de_fuente(&app, fuente_efectiva(&app)),
            Some(norte_i18n::t("log-source-unsupported")),
            "un daemon sin registro tiene que decirse"
        );
    }

    /// Sin daemon —el `ntc` corriente, que es el arranque por defecto— no se
    /// pregunta nada y no se nombra a nadie.
    ///
    /// Es la avería que la revisión encontró: el brazo embebido contesta
    /// `Unsupported` a `log.tail` con toda la razón —su anillo es el que este
    /// panel ya lee—, y el panel lo leía como un hecho sobre un daemon. Con un
    /// `ntc` sin daemon ninguno, el borde pasaba por «de este proceso (el
    /// daemon registra aparte)» y se quedaba en «este daemon no sirve su
    /// registro». La respuesta correcta no es una tercera frase: es la ausencia
    /// del segmento, que es como estaba en #326.
    #[test]
    fn sin_daemon_no_se_sondea_ni_se_nombra_a_nadie() {
        let mut app = crate::app::testutil::app_dos_panes();
        app.log_ring = Some(norte_config::logring::LogRing::new(10));
        app.toggle_log();
        assert!(!app.log_remote.hay_daemon, "por defecto no hay daemon");
        assert!(
            !app.log_remote.debe_pedir(),
            "se iba a sondear a un daemon que no existe"
        );
        for fuente in [LogSource::Window, LogSource::Daemon, LogSource::Both] {
            assert_eq!(
                etiqueta_de_fuente(&app, fuente),
                None,
                "se nombró un origen con un solo anillo ({fuente:?})"
            );
        }
        // Y la fuente efectiva no puede ser otra cosa: `servicio` jamás llega a
        // `Sirve` porque nadie pregunta.
        assert_eq!(fuente_efectiva(&app), LogSource::Window);
        // Ni se anuncia el anillo de nadie al subir el nivel.
        aplicar_accion(&mut app, LogAction::Level(LogLevel::Trace));
        assert_eq!(app.message, None);
        assert_eq!(app.log_remote.pide_nivel, None);
    }

    /// El sondeo mira si el panel se VE, no si existe: uno escondido detrás de
    /// una pestaña que no es la activa sigue en el árbol, y sondearlo son dos
    /// RPC por segundo toda la sesión por algo que nadie tiene delante.
    ///
    /// La barra de paneles sigue contando el hueco escondido como abierto —eso
    /// es #329 y no se toca aquí—; lo que este test fija es que el gasto de red
    /// no depende de ese bug.
    #[test]
    fn un_panel_detras_de_una_pestana_no_se_sondea() {
        use norte_frontend::layout::{KindId, Node};
        let mut app = app_con_las_dos_fuentes();
        let id = app.log_slot().expect("el panel está abierto");
        assert_eq!(
            app.log_slot_visible(),
            Some(id),
            "se ve antes de esconderlo"
        );

        // El mismo hueco, ahora en la pestaña NO activa de unas pestañas.
        let otro = app
            .layout
            .slot_ids()
            .into_iter()
            .find(|s| *s != id)
            .expect("hay más huecos que el registro");
        app.layout = Node::Tabs {
            children: vec![
                Node::slot(otro, KindId::new("pane")),
                Node::slot(id, KindId::new(KIND)),
            ],
            active: 0,
        };
        assert_eq!(
            app.log_slot(),
            Some(id),
            "sigue existiendo, que es lo que `log_slot` contesta"
        );
        assert_eq!(
            app.log_slot_visible(),
            None,
            "un hueco detrás de otra pestaña no está en pantalla"
        );
    }

    /// Dos contadores y JAMÁS su suma: el del anillo local cuenta lo evacuado
    /// desde que arrancó el proceso, el del daemon lo que esta apertura del
    /// panel se perdió. Sumarlos daría un número que no es ninguna de las dos
    /// cosas.
    #[test]
    fn los_dos_contadores_se_dicen_por_separado() {
        use norte_config::logring::LogRing;
        let mut app = crate::app::testutil::app_dos_panes();
        let anillo = LogRing::new(1);
        con_lineas(&anillo, || {
            for _ in 0..4 {
                tracing::info!("x");
            }
        });
        app.log_ring = Some(anillo);
        app.toggle_log();
        let epoca = app.log_remote.epoca;
        aterrizar_tail(&mut app, epoca, Ok(tail(Vec::new(), 9, 5)));
        let nota = nota_de_descartes(&app, LogSource::Both);
        assert!(nota.contains('3'), "faltan las 3 locales: {nota:?}");
        assert!(nota.contains('5'), "faltan las 5 del daemon: {nota:?}");
        assert!(!nota.contains('8'), "los contadores se sumaron: {nota:?}");
        // Y de un anillo que no se está leyendo no se avisa: una alarma sobre
        // un hueco que no está en pantalla es una alarma sobre nada.
        assert!(!nota_de_descartes(&app, LogSource::Window).contains('5'));
        assert!(!nota_de_descartes(&app, LogSource::Daemon).contains('3'));
    }

    /// El cursor se encadena (`None` la primera vez, nunca cero) y las
    /// pérdidas se ACUMULAN: un hueco silencioso miente sobre lo que pasó.
    #[test]
    fn el_cursor_se_encadena_y_las_perdidas_se_acumulan() {
        let mut app = crate::app::testutil::app_dos_panes();
        app.toggle_log();
        assert_eq!(
            app.log_remote.cursor, None,
            "la primera vez no se pide cero"
        );
        let epoca = app.log_remote.epoca;
        aterrizar_tail(&mut app, epoca, Ok(tail(vec![wire(1, "warn", "a")], 4, 2)));
        assert_eq!(app.log_remote.cursor, Some(4));
        aterrizar_tail(&mut app, epoca, Ok(tail(vec![wire(2, "warn", "b")], 9, 3)));
        assert_eq!(app.log_remote.cursor, Some(9));
        assert_eq!(app.log_remote.perdidas, 5, "las pérdidas no se acumularon");
        assert_eq!(
            app.log_remote.lineas.len(),
            2,
            "la vuelta no trae solo lo nuevo"
        );
    }

    /// Un daemon que dice que no tiene registro no se vuelve a preguntar: la
    /// negativa sale de una feature de compilación y no puede cambiar mientras
    /// ese daemon viva. Un fallo CUALQUIERA no es eso, y se reintenta.
    #[test]
    fn un_daemon_sin_registro_deja_de_sondearse_y_un_fallo_no() {
        let mut app = crate::app::testutil::app_dos_panes();
        app.log_remote.hay_daemon = true;
        app.toggle_log();
        let epoca = app.log_remote.epoca;
        aterrizar_tail(
            &mut app,
            epoca,
            Err(norte_proto::Error::Io { retryable: true }),
        );
        assert_eq!(
            app.log_remote.servicio,
            Servicio::SinRespuesta,
            "un fallo pasajero no es una carencia permanente"
        );
        assert!(app.log_remote.debe_pedir(), "un fallo no apaga el sondeo");

        aterrizar_tail(&mut app, epoca, Err(norte_proto::Error::Unsupported));
        assert_eq!(app.log_remote.servicio, Servicio::SinAnillo);
        assert!(
            !app.log_remote.debe_pedir(),
            "seguir preguntando serían dos RPC por segundo para siempre"
        );
    }

    /// Entre pedir y contestar caben un cierre y una apertura, y la respuesta
    /// de la sesión anterior tiene que MORIR: aterrizar su cursor en el panel
    /// nuevo dejaría un `lost` inventado y líneas de otra lectura.
    #[test]
    fn la_respuesta_de_una_apertura_anterior_no_aterriza() {
        let mut app = crate::app::testutil::app_dos_panes();
        app.toggle_log();
        let vieja = app.log_remote.epoca;
        app.toggle_log(); // cierra: reinicia y cambia de época
        app.toggle_log(); // vuelve a abrir
        aterrizar_tail(
            &mut app,
            vieja,
            Ok(tail(vec![wire(1, "info", "de la otra vez")], 99, 7)),
        );
        assert!(
            app.log_remote.lineas.is_empty(),
            "aterrizó una respuesta vieja"
        );
        assert_eq!(app.log_remote.cursor, None);
        assert_eq!(app.log_remote.perdidas, 0);
    }

    /// Cerrar el panel suelta lo del daemon pero NO olvida que lo sirve: que
    /// haya una segunda fuente es un hecho sobre el daemon, no sobre esta
    /// apertura, y olvidarlo escondería la tecla `s` medio segundo cada vez.
    #[test]
    fn cerrar_suelta_las_lineas_pero_no_lo_que_se_sabe_del_daemon() {
        let mut app = app_con_las_dos_fuentes();
        assert_eq!(app.log_remote.servicio, Servicio::Sirve);
        app.toggle_log();
        assert!(app.log_remote.lineas.is_empty());
        assert_eq!(app.log_remote.cursor, None);
        assert_eq!(
            app.log_remote.servicio,
            Servicio::Sirve,
            "olvidar que sirve escondería el mando al reabrir"
        );
    }

    /// `s` recorre la fuente, y solo cuando hay una segunda que ofrecer:
    /// recorrer tres vistas del MISMO anillo sería un mando que promete algo
    /// que no existe.
    #[test]
    fn la_tecla_de_la_fuente_solo_significa_algo_con_daemon() {
        assert_eq!(
            key(KeyCode::Char('s'), KeyModifiers::empty()),
            Some(LogAction::Source)
        );
        let mut app = crate::app::testutil::app_dos_panes();
        app.toggle_log();
        aplicar_accion(&mut app, LogAction::Source);
        assert_eq!(
            app.log_panel.source(),
            LogSource::Both,
            "sin daemon, la preferencia no se toca"
        );

        let mut app = app_con_las_dos_fuentes();
        aplicar_accion(&mut app, LogAction::Source);
        assert_eq!(app.log_panel.source(), LogSource::Window);
        aplicar_accion(&mut app, LogAction::Source);
        assert_eq!(app.log_panel.source(), LogSource::Daemon);
        aplicar_accion(&mut app, LogAction::Source);
        assert_eq!(
            app.log_panel.source(),
            LogSource::Both,
            "no vuelve al principio"
        );
    }

    /// Pedir más detalle se lo pide TAMBIÉN al daemon cuando es una de las
    /// fuentes: su anillo es suyo, y sin subirlo las líneas que se están
    /// pidiendo no llegan a existir al otro lado. Por la PREFERENCIA y no por
    /// la fuente efectiva — quien eligió leer el daemon está pidiendo su nivel
    /// aunque todavía no haya contestado. Que HAYA daemon, en cambio, sí manda:
    /// sin él la petición sería estado muerto que nadie drena.
    #[test]
    fn subir_el_nivel_se_lo_pide_tambien_al_daemon() {
        let mut app = crate::app::testutil::app_dos_panes();
        app.log_remote.hay_daemon = true;
        app.toggle_log();
        assert_eq!(
            app.log_remote.servicio,
            Servicio::SinRespuesta,
            "todavía no ha contestado, y aun así se le pide"
        );
        aplicar_accion(&mut app, LogAction::Level(LogLevel::Debug));
        assert_eq!(
            app.log_remote.pide_nivel,
            Some(LogLevel::Debug),
            "no se le pidió al daemon"
        );

        app.log_remote.pide_nivel = None;
        app.log_panel.set_source(LogSource::Window);
        aplicar_accion(&mut app, LogAction::Level(LogLevel::Trace));
        assert_eq!(
            app.log_remote.pide_nivel, None,
            "leyendo solo esta terminal no hay por qué subirle el anillo global a nadie"
        );
    }

    /// El desplazamiento cuenta sobre la lista MEZCLADA: contando solo las
    /// locales el tope se queda corto y una página no llega al final.
    #[test]
    fn el_desplazamiento_cuenta_las_dos_fuentes() {
        let mut app = app_con_las_dos_fuentes();
        app.log_panel.set_viewport_rows(1);
        aplicar_accion(&mut app, LogAction::Scroll(-1));
        assert!(!app.log_panel.following(), "subir no despegó del final");
        let locales = instantanea(&app);
        assert_eq!(
            app.log_panel
                .window_start(visibles(&app, &locales).len(), 1),
            0,
            "con dos líneas y una fila, subir una deja la primera arriba"
        );
        aplicar_accion(&mut app, LogAction::Scroll(1));
        assert!(
            app.log_panel.following(),
            "bajar hasta el final no volvió a pegarlo: el tope contó solo el anillo local"
        );
    }

    /// El tope de líneas remotas se aplica por DELANTE y lo recortado cuenta
    /// como perdido: un panel abierto toda una tarde no puede crecer sin fin,
    /// y el recorte no puede dejar un hueco callado.
    #[test]
    fn el_tope_remoto_tira_lo_viejo_y_lo_cuenta() {
        let mut app = crate::app::testutil::app_dos_panes();
        app.toggle_log();
        let epoca = app.log_remote.epoca;
        let muchas: Vec<_> = (0..i64::try_from(MAX_LINEAS_REMOTAS).unwrap() + 3)
            .map(|i| wire(i, "info", "x"))
            .collect();
        aterrizar_tail(&mut app, epoca, Ok(tail(muchas, 1, 0)));
        assert_eq!(app.log_remote.lineas.len(), MAX_LINEAS_REMOTAS);
        assert_eq!(app.log_remote.perdidas, 3, "el recorte se calló");
        assert_eq!(
            app.log_remote.lineas[0].epoch_ms, 3,
            "se tiró lo nuevo en vez de lo viejo"
        );
    }
}
