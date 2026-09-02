//! El panel de registro de la ventana (#326): su proyección y sus mandos.
//!
//! Parte de `controller`: son métodos de `Estado`, movidos aquí sin tocarlos
//! (ADR 0086). El único escritor sigue siendo el actor.
//!
//! El ESTADO —nivel, filtro, seguimiento del final, la regla de los dos
//! niveles— vive en `norte_frontend::logpanel`, que es el mismo código que usa
//! la TUI; las líneas salen del anillo de `norte_config::logring`. Aquí queda
//! lo que es de esta ventana: cómo se proyecta al puente y qué hace cada mando.

// Estos módulos son el mismo `impl Estado` partido en trozos, así que usan
// los mismos imports que el padre. Enumerarlos aquí sería una lista de
// cuarenta líneas por fichero, en 32 ficheros, que se desincroniza en cuanto
// el padre importa algo — `super::*` la sigue sola.
#[allow(clippy::wildcard_imports)]
use super::*;

use norte_config::logline::{LogLevel, LogLine};
use norte_frontend::logpanel::LogSource;

/// El kind que ocupa un hueco de registro. El mismo que la TUI.
pub(super) const KIND: &str = "log";

/// Cuántas líneas se le piden al daemon en cada vuelta.
///
/// El daemon recorta a 1000, así que esto es una petición y no un contrato.
/// Quinientas porque una vuelta que no quepa NO pierde nada —lo que sobra
/// sigue después del cursor y lo recoge la vuelta siguiente, medio segundo
/// después— y porque el panel enseña como mucho una pantalla: pedir el anillo
/// entero cada vez sería pagar dos mil líneas por cada una que se pinta.
const MAX_REMOTO: u32 = 500;

/// Techo de líneas del daemon que se guardan en memoria.
///
/// El anillo local ya tiene el suyo; éste es el mismo cuidado para el remoto,
/// porque aquí las líneas se ACUMULAN vuelta a vuelta y sin tope un panel
/// abierto toda una tarde crecería sin fin. Del mismo orden que el anillo por
/// defecto: lo que se puede recorrer hacia atrás.
const MAX_LINEAS_REMOTAS: usize = 2000;

/// Cuántas líneas salta una página cuando el renderer no dice su alto.
const PAGINA: isize = 10;

/// Cada cuánto se mira si el registro ha cambiado.
///
/// El panel promete que SIGUE lo que llega, y esa promesa hay que cumplirla:
/// la TUI la cumple porque repinta por frame, y esta ventana solo repinta
/// cuando alguien hace algo — así que sin sondeo el panel se quedaba congelado
/// entre pulsaciones mientras decía «pegado al final».
///
/// Medio segundo: un registro se lee, no se cronometra. Y el sondeo es BARATO
/// —un `AtomicU64`, sin tocar el candado del anillo— así que lo que cuesta de
/// verdad es la foto, y esa solo se manda cuando hay algo nuevo.
const SONDEO: std::time::Duration = std::time::Duration::from_millis(500);

/// Techo de filas que un renderer puede declarar visibles.
///
/// Generoso para una pantalla de verdad —un monitor 4K con letra pequeña no
/// llega— y acotado porque el número viene de la webview: sin techo, un `rows`
/// enorme convierte cada foto en el anillo entero.
const MAX_FILAS_REGISTRO: usize = 512;

/// Qué se sabe del registro del DAEMON.
///
/// Tres valores y no un `bool`, porque «todavía no ha contestado» y «ha dicho
/// que no tiene registro» se enseñan distinto: lo primero no dice nada
/// —el selector simplemente no está—, y lo segundo es una frase que el panel
/// tiene que poner en pantalla. Colapsarlos haría que un panel recién abierto
/// afirmara una carencia que nadie ha comprobado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Servicio {
    /// Nunca ha contestado: no se sabe.
    #[default]
    SinRespuesta,
    /// Sirve su registro: hay una segunda fuente de verdad.
    Sirve,
    /// Dijo que no tiene registro que servir.
    SinAnillo,
}

/// La mitad remota del panel de registro (#328).
#[derive(Debug, Default)]
pub(super) struct RegistroRemoto {
    /// Lo que el daemon lleva entregado, ya en forma de presentación y de lo
    /// más viejo a lo más nuevo.
    ///
    /// Se acumula y no se repide entero en cada vuelta: el sondeo tira del
    /// anillo remoto con un cursor, así que cada respuesta trae solo lo nuevo.
    pub(super) lineas: Vec<LogLine>,
    /// Por dónde iba. `None` = todavía no se ha preguntado, que es «dame lo
    /// que haya» y NO es lo mismo que cero: contra un anillo que ya dio la
    /// vuelta, un cero reportaría un `lost` falso en el primer sondeo.
    pub(super) cursor: Option<u64>,
    /// Hay una petición en vuelo.
    ///
    /// Sin esto, un daemon que tardara más de medio segundo en contestar
    /// acumularía una petición por tic para siempre.
    pub(super) en_vuelo: bool,
    /// Qué se sabe de si sirve su registro.
    pub(super) servicio: Servicio,
    /// El nivel que contestó tener puesto, en forma de wire.
    ///
    /// Suyo y no nuestro: es global a todos sus clientes y solo sube, así que
    /// lo que se pidió y lo que hay puesto no tienen por qué coincidir.
    pub(super) nivel: Option<String>,
    /// Cuántas líneas se cayeron por detrás de este cursor. Se acumulan: un
    /// hueco silencioso miente sobre lo que pasó.
    pub(super) perdidas: u64,
}

impl RegistroRemoto {
    /// Empieza de cero, conservando lo que se sabe del daemon.
    ///
    /// Las líneas y el cursor son de ESTA apertura; que el daemon sirva o no
    /// su registro es un hecho sobre el daemon, y olvidarlo escondería el
    /// selector medio segundo cada vez que se reabre el panel.
    pub(super) fn reiniciar(&mut self) {
        self.lineas.clear();
        self.cursor = None;
        self.en_vuelo = false;
        self.perdidas = 0;
    }

    /// ¿Tiene sentido volver a preguntarle algo a este daemon?
    ///
    /// A uno que ya dijo que no tiene registro, no: la negativa no puede
    /// cambiar mientras ese daemon viva —sale de una feature de compilación o
    /// de un montaje que le falló al arrancar—, así que seguir preguntando son
    /// RPC para siempre por una respuesta que no puede ser otra. Es asimétrico
    /// a propósito: lo POSITIVO sí hay que seguir pidiéndolo, porque el
    /// registro crece.
    ///
    /// Lo comparten el sondeo de `log.tail` y la petición de `log.level`, y
    /// eso es el arreglo de una asimetría: el nivel se pedía solo por la
    /// fuente, así que contra un daemon que ya había contestado `Unsupported`
    /// la ventana mandaba un RPC muerto por cada pulsación de nivel. La TUI ya
    /// lo hacía bien (`RegistroRemoto::debe_pedir`); ahora es la misma regla en
    /// las dos.
    ///
    /// No hay comparación de versiones aquí ni en ninguna parte: un daemon más
    /// viejo ni siquiera completa el `initialize`.
    pub(super) const fn debe_pedir(&self) -> bool {
        !matches!(self.servicio, Servicio::SinAnillo)
    }
}

/// Una línea del cable a la forma que el panel pinta.
///
/// Los dos tipos se llaman igual y conviven en este fichero a propósito:
/// `methods::LogLine` es el CABLE y `logline::LogLine` la presentación, y
/// mezclarlos es la manera de acabar mandando una `String` de nivel a un
/// filtro que compara verbosidades.
///
/// Un nivel que no se reconozca cae en `Info` en vez de tirar la línea: el
/// protocolo dice que un valor desconocido tiene que poder LLEGAR, y perder el
/// mensaje entero por no entender su etiqueta es peor que enseñarlo con la
/// etiqueta corriente. Con el vocabulario cerrado de hoy no ocurre.
fn linea_de_wire(l: norte_proto::methods::LogLine) -> LogLine {
    LogLine {
        epoch_ms: l.epoch_ms,
        level: LogLevel::from_wire(&l.level).unwrap_or(LogLevel::Info),
        target: l.target,
        message: l.message,
    }
}

impl Estado {
    /// La proyección del panel: solo la VENTANA visible.
    ///
    /// Como el listado, y por lo mismo: un anillo de dos mil líneas mandado
    /// entero en cada parche es el derroche que la decisión D7 existe para
    /// evitar, y el registro se mueve más que un directorio.
    pub(super) fn panel_de_registro(&self, slot: u32) -> crate::dto::LogSlotView {
        let lineas = self
            .log_ring
            .as_ref()
            .map(norte_config::logring::LogRing::snapshot)
            .unwrap_or_default();
        let fuente = self.fuente_efectiva();
        // Prestadas, no clonadas: `merge` devuelve referencias a propósito —el
        // anillo ya clonó una vez en su `snapshot`— y el panel pinta como
        // mucho una pantalla.
        let mezcla = norte_frontend::logpanel::merge(&lineas, &self.log_remoto.lineas, fuente);
        let visibles: Vec<_> = mezcla
            .into_iter()
            .filter(|(l, _)| self.log_panel.matches(l))
            .collect();
        let desde = self.log_panel.window_start(visibles.len(), self.log_filas);
        let ventana = visibles
            .iter()
            .skip(desde)
            .take(self.log_filas)
            .map(|(l, s)| Self::linea_de_registro(l, *s));
        crate::dto::LogSlotView {
            slot_id: slot,
            lines: ventana.collect(),
            // El que se ENSEÑA, siempre, en todas las fuentes — y por tanto el
            // que los botones controlan.
            //
            // Enseñar aquí el que el daemon contestó era un error de dos
            // cabezas: el filtro de `visibles` sigue siendo el del panel, así
            // que con el daemon a `trace` y el panel a `info` la cabecera
            // marcaba `trace` mientras cada línea `debug` del daemon llegaba
            // por el cable y se tiraba en silencio —justo el «no hay líneas de
            // DEBUG es indistinguible de no capturarlas» que el rustdoc de
            // `LogTailResult::level` existe para impedir—; y pulsar `info` no
            // movía la marca, porque el daemon nunca baja, así que el mando se
            // leía como muerto. El nivel del daemon se dice en `capturing`,
            // que es el sitio que ya significa «se recoge más de lo que se ve».
            level: self.log_panel.level().wire().to_owned(),
            // El filtro lo TECLEA el lector, así que se pinta como cualquier
            // otro texto de fuera: enmascarado y acotado.
            filter: clamp_display(
                norte_frontend::display_name(self.log_panel.filter().as_bytes()).0,
            ),
            following: self.log_panel.following(),
            total: visibles.len() as u64,
            first_visible: desde as u64,
            dropped_note: self.nota_de_descartes(fuente),
            capturing: self.nota_de_captura(fuente),
            source: clamp_display(norte_i18n::t_in(
                self.lang,
                match fuente {
                    // De ESTE proceso, y decirlo es el punto: la ventana
                    // arranca su propio daemon (#300), así que aquí NO está lo
                    // del daemon —los providers, el journal, la política—, que
                    // es la mitad interesante. Callarlo haría que el panel
                    // pareciera roto: alguien lo abre mientras una conexión
                    // falla, no ve la línea que lo explica, y concluye que el
                    // panel no funciona en vez de que está mirando otro sitio.
                    LogSource::Window if self.log_ring.is_some() => "log-source-window",
                    // No es «no se registra nada»: el proceso sigue
                    // escribiendo a su fichero. Lo que falta es el anillo en
                    // memoria, que es lo que este panel lee — y decir lo
                    // primero sería una respuesta más tranquilizadora que la
                    // verdad. La TUI ya tenía la frase exacta.
                    LogSource::Window => "log-no-ring",
                    LogSource::Daemon => "log-source-daemon",
                    LogSource::Both => "log-source-both",
                },
            )),
            source_mode: match fuente {
                LogSource::Window => "window",
                LogSource::Daemon => "daemon",
                LogSource::Both => "both",
            }
            .to_owned(),
            sources_available: self.log_remoto.servicio == Servicio::Sirve,
            source_note: self.nota_de_fuente(fuente),
        }
    }

    /// La fuente que de verdad se está enseñando.
    ///
    /// La preferencia se guarda tal cual (`LogPanel::source`), pero una fuente
    /// que no existe no se puede enseñar, y el panel informa de lo que hay y no
    /// de lo que se pidió. Se colapsa en las DOS direcciones, que son la misma
    /// regla vista desde cada orilla:
    ///
    /// - sin un anillo al otro lado (un daemon sin la feature `logging`, o el
    ///   caso embebido) todo cae a `Window`;
    /// - sin anillo en ESTE proceso —nadie montó la capa— no hay nada local que
    ///   mezclar, así que todo cae a `Daemon`. Sin esto, un `Both` sobre un
    ///   proceso sin anillo se anunciaba como «de la ventana y del daemon»
    ///   siendo la lista entera del daemon.
    ///
    /// Con los dos anillos ausentes queda `Window`, que es donde vive la frase
    /// de #326: no hay registro EN MEMORIA que leer, y eso no es lo mismo que
    /// «no se registra nada».
    fn fuente_efectiva(&self) -> LogSource {
        match (
            self.log_remoto.servicio == Servicio::Sirve,
            self.log_ring.is_some(),
        ) {
            (true, true) => self.log_panel.source(),
            (true, false) => LogSource::Daemon,
            (false, _) => LogSource::Window,
        }
    }

    /// Lo que hay que decir sobre la fuente. Vacío = nada que decir.
    ///
    /// Dos frases excluyentes, y las dos existen para que el panel no mienta
    /// por omisión. Que el daemon no tiene registro que servir, o el lector
    /// creería que la mitad interesante simplemente no ocurre. Y **de quién es
    /// el nivel**, siempre que el daemon sea una de las fuentes que se leen —
    /// no solo cuando es la única: en `Both`, que es lo que trae el panel al
    /// abrirse, pulsar «traza» sube un anillo GLOBAL al daemon, compartido con
    /// todos sus clientes, que no vuelve a bajar y que cerrar este panel no
    /// baja. Callarlo en el camino corriente dejaba esa decisión sin anunciar.
    fn nota_de_fuente(&self, fuente: LogSource) -> String {
        let clave = if self.log_remoto.servicio == Servicio::SinAnillo {
            "log-source-unsupported"
        } else if fuente == LogSource::Window {
            return String::new();
        } else {
            "log-source-daemon-level"
        };
        clamp_display(norte_i18n::t_in(self.lang, clave))
    }

    /// Qué anillo está guardando MÁS de lo que se enseña, y cuál.
    ///
    /// Solo cuando se captura de más: decir «capturando info» sobre un panel
    /// que enseña info sería ruido, y el ruido es lo que hace que se deje de
    /// leer la línea que sí importa.
    ///
    /// Con una sola fuente la frase no nombra el anillo —no hay otro con el
    /// que confundirlo—; con las dos, cada parte dice de quién habla. Que aquí
    /// aparezca el nivel del daemon es lo que hace legible la regla entera: el
    /// suyo es global a sus clientes y solo sube, así que puede estar muy por
    /// encima del que este panel enseña, y ese hueco es exactamente lo que
    /// esta frase existe para no callar.
    fn nota_de_captura(&self, fuente: LogSource) -> String {
        let ensena = self.log_panel.level();
        let local = self
            .log_ring
            .as_ref()
            .map(norte_config::logring::LogRing::level)
            .filter(|cap| *cap > ensena);
        let remoto = self
            .log_remoto
            .nivel
            .as_deref()
            .and_then(LogLevel::from_wire)
            .filter(|cap| *cap > ensena);
        let frase =
            |clave, cap: LogLevel| norte_i18n::ta_in(self.lang, clave, &[("level", cap.wire())]);
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
        clamp_display(partes.join(" · "))
    }

    /// Las líneas que se han perdido, por anillo y DICIENDO de cuál.
    ///
    /// Dos números y no uno, porque no significan lo mismo y no viven lo
    /// mismo: el del anillo local cuenta lo que ha evacuado desde que arrancó
    /// el proceso y no se reinicia nunca; el del daemon cuenta lo que ESTA
    /// apertura del panel se perdió, y vuelve a cero al reabrirlo. Sumarlos
    /// daba un número que no era ninguna de las dos cosas.
    ///
    /// De cada anillo solo se habla si se está leyendo: avisar de un hueco en
    /// un registro que no está en pantalla es una alarma sobre nada.
    fn nota_de_descartes(&self, fuente: LogSource) -> String {
        let mut partes: Vec<String> = Vec::new();
        let locales = self
            .log_ring
            .as_ref()
            .map_or(0, norte_config::logring::LogRing::dropped);
        if locales > 0 && fuente != LogSource::Daemon {
            partes.push(norte_i18n::ta_in(
                self.lang,
                // Sin nombrar el anillo cuando es el único que se lee: es la
                // misma frase que la TUI, que nunca tiene dos.
                if fuente == LogSource::Window {
                    "log-dropped"
                } else {
                    "log-dropped-window"
                },
                &[("n", &locales.to_string())],
            ));
        }
        if self.log_remoto.perdidas > 0 && fuente != LogSource::Window {
            partes.push(norte_i18n::ta_in(
                self.lang,
                "log-missed-daemon",
                &[("n", &self.log_remoto.perdidas.to_string())],
            ));
        }
        clamp_display(partes.join(" · "))
    }

    /// Una línea, saneada.
    ///
    /// El mensaje pasa por `display_name` como cualquier texto que se pinta, y
    /// aquí con un motivo propio: un mensaje de registro puede llevar dentro el
    /// nombre de un fichero que alguien eligió, y un `U+202E` ahí reordena la
    /// línea entera del panel.
    fn linea_de_registro(l: &LogLine, origen: LogSource) -> crate::dto::LogLineView {
        let (target, t_hostil) = norte_frontend::display_name(l.target.as_bytes());
        let (mensaje, m_hostil) = norte_frontend::display_name(l.message.as_bytes());
        crate::dto::LogLineView {
            time: norte_frontend::format::hora_utc(l.epoch_ms),
            level: l.level.wire().to_owned(),
            target: clamp_display(target),
            message: clamp_display(mensaje),
            hostile: t_hostil || m_hostil,
            // `Both` no le pasa a una línea: `merge` marca cada una con el
            // proceso del que salió, que es lo único que aquí significa algo.
            source: if origen == LogSource::Daemon {
                "daemon"
            } else {
                "window"
            }
            .to_owned(),
        }
    }

    /// Enseñar hasta este nivel.
    ///
    /// **Sube el del ANILLO si hace falta, y nunca lo baja.** Es la regla que
    /// `LogPanel::show_level` devuelve y que hay que atar: filtrar en la
    /// pantalla lo que nunca se registró es imposible, así que pedir DEBUG
    /// tiene que hacer que el anillo empiece a capturarlo. Y bajar a ERROR no
    /// deja de capturar, porque entonces volver a subir enseñaría un agujero
    /// del tamaño del rato que se estuvo en ERROR.
    /// Y, cuando la fuente incluye al daemon, **se lo pide TAMBIÉN a él**
    /// (#328). Su anillo es suyo: este cliente no aplica niveles, porque la
    /// cota que impide que ahí dentro aparezca una contraseña vive en el
    /// proceso que tiene el anillo. Lo que quede puesto lo contesta él, y
    /// puede no ser lo que se pidió — es global a todos sus clientes.
    ///
    /// Se pide por la PREFERENCIA y no por la fuente efectiva: quien ha
    /// elegido leer el daemon está pidiendo el nivel del daemon aunque ahora
    /// mismo no haya contestado todavía, y la respuesta a esta llamada es
    /// justamente una de las dos formas de averiguar si sabe de registro.
    pub(super) fn nivel_de_registro(
        &mut self,
        nivel: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(nivel) = LogLevel::from_wire(nivel) else {
            // Vocabulario CERRADO: uno que no se conoce no cae en `Info`, que
            // dejaría el panel enseñando otra cosa de la que se pidió.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-log-level-unknown".to_owned(),
                },
                Vec::new(),
            );
        };
        self.log_panel.show_level(nivel);
        if let Some(anillo) = &self.log_ring {
            anillo.raise_to(nivel);
        }
        // Y solo si hay alguien a quien pedírselo: a un daemon que ya dijo que
        // no tiene anillo, subirle el nivel es un RPC por pulsación cuya
        // respuesta ya se conoce. Es la misma condición que corta el sondeo
        // (ver `RegistroRemoto::debe_pedir`), y la TUI ya la aplicaba aquí.
        if self.log_panel.source() != LogSource::Window && self.log_remoto.debe_pedir() {
            let backend = Arc::clone(backend);
            let buzon = buzon.clone();
            let epoca = self.log_epoca;
            let pedido = nivel.wire().to_owned();
            tokio::spawn(async move {
                let r = backend.log_level(pedido).await;
                let _ = buzon.send(Mensaje::RegistroNivel(epoca, Box::new(r))).await;
            });
        }
        self.repintar_registro()
    }

    /// Recorre la fuente del registro (#328).
    ///
    /// Sin una segunda fuente no hace nada y no se pinta: cambiar entre tres
    /// vistas de un mismo anillo sería un mando que promete algo que no
    /// existe. Aun así se acepta la acción en vez de rechazarla — el renderer
    /// solo la manda cuando el selector está en pantalla, y un `Unavailable`
    /// aquí sería un aviso sobre una pulsación que nadie pudo dar.
    ///
    /// La guarda es de AQUÍ y no del renderer, y eso se corrigió: dejarla en
    /// `sources_available` bastaba para que no se viera nada raro —la fuente
    /// efectiva colapsa a `Window` de todos modos—, pero la PREFERENCIA se
    /// movía por debajo de un lector que no puede verla moverse, y reaparecía
    /// puesta en otra cosa el día que sí hubiera daemon. Es lo mismo que hace
    /// la TUI, que tampoco recorre sin daemon que sirva.
    pub(super) fn fuente_de_registro(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.log_remoto.servicio == Servicio::Sirve {
            self.log_panel.cycle_source();
        }
        self.repintar_registro()
    }

    /// El filtro de texto sobre módulo y mensaje.
    pub(super) fn filtro_de_registro(
        &mut self,
        texto: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if texto.len() > MAX_NOMBRE {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-name-too-long".to_owned(),
                },
                Vec::new(),
            );
        }
        self.log_panel.set_filter(texto);
        self.repintar_registro()
    }

    /// Sube o baja por el registro, despegándose del final.
    ///
    /// Despegarse es la mitad del panel: uno que salta siempre al final no se
    /// puede leer mientras algo escribe, que es justo cuando hace falta.
    pub(super) fn desplazar_registro(
        &mut self,
        delta: i64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let lineas = self
            .log_ring
            .as_ref()
            .map(norte_config::logring::LogRing::snapshot)
            .unwrap_or_default();
        // Sobre la lista MEZCLADA, que es la que se ve: contar solo las
        // locales dejaría el tope corto y una página no llegaría al final.
        let visibles = norte_frontend::logpanel::merge(
            &lineas,
            &self.log_remoto.lineas,
            self.fuente_efectiva(),
        )
        .into_iter()
        .filter(|(l, _)| self.log_panel.matches(l))
        .count();
        let delta = isize::try_from(delta).unwrap_or(PAGINA);
        if delta < 0 {
            self.log_panel.scroll_up(delta.unsigned_abs(), visibles);
        } else {
            self.log_panel.scroll_down(delta.unsigned_abs(), visibles);
        }
        self.repintar_registro()
    }

    /// Vuelve a pegarse al final.
    pub(super) fn seguir_registro(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.log_panel.follow();
        self.repintar_registro()
    }

    /// Cuántas filas caben, del frame que el renderer acaba de pintar.
    ///
    /// La pone él y no se adivina aquí: en la TUI, adivinar el alto hizo que
    /// cada página se saltara dos líneas y la primera cuatro, y lo que ninguna
    /// de las dos ventanas enseñaba no se podía leer de ninguna manera.
    pub(super) fn filas_de_registro(
        &mut self,
        filas: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Con suelo y con TECHO. El suelo, para que una página mueva algo; el
        // techo, porque este número lo manda la webview y sin él un `rows` de
        // cuatro mil millones haría que cada foto llevara el anillo entero —
        // dos mil líneas por acción, que es justo lo que la decisión D7 y el
        // rustdoc de `LogSlotView` existen para impedir—. El camino del
        // listado ya se acota igual.
        let filas = (filas as usize).clamp(1, MAX_FILAS_REGISTRO);
        if filas == self.log_filas {
            // Sin cambio no hay parche: el renderer manda esto por frame, y
            // contestar a todos gastaría un número de secuencia por frame.
            return (self.aplicada(), Vec::new());
        }
        self.log_filas = filas;
        self.log_panel.set_viewport_rows(filas);
        self.repintar_registro()
    }

    /// Repinta el registro, si hay algún hueco enseñándolo.
    ///
    /// Va como FOTO y no como parche, por lo mismo que el cursor del panel de
    /// procesos: no hay un `ViewChange` para un hueco que no es un listado, y
    /// añadir uno por esto sería contrato nuevo para lo que son teclas
    /// sueltas, no un desplazamiento continuo.
    ///
    /// Sin ningún hueco de registro no se manda nada —el panel se cierra y una
    /// acción en vuelo aterriza después—: una foto de más gasta un número de
    /// secuencia para pintar lo mismo.
    fn repintar_registro(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.huecos_de_registro().is_empty() {
            return (self.aplicada(), Vec::new());
        }
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Programa el siguiente sondeo del registro, si hay panel abierto.
    ///
    /// Se rearma solo mientras el panel siga en pantalla y se apaga cuando se
    /// cierra: un temporizador que sobreviviera al panel estaría despertando
    /// al actor cada medio segundo para no pintar nada.
    ///
    /// `epoca` distingue una apertura de la siguiente: abrir, cerrar y volver
    /// a abrir dejaría dos temporizadores vivos sobre el mismo panel, y el
    /// viejo seguiría rearmándose para siempre.
    pub(super) fn sondear_registro(&self, buzon: &mpsc::Sender<Mensaje>) {
        if self.huecos_de_registro().is_empty() {
            return;
        }
        let buzon = buzon.clone();
        let epoca = self.log_epoca;
        tokio::spawn(async move {
            tokio::time::sleep(SONDEO).await;
            let _ = buzon.send(Mensaje::RegistroTic(epoca)).await;
        });
    }

    /// El sondeo llegó: se repinta SOLO si el anillo tiene algo nuevo.
    ///
    /// El contador de entradas es un `AtomicU64` que solo sube, así que la
    /// comprobación no toca el candado ni clona nada. Sin ella, esto sería una
    /// foto entera de la pantalla dos veces por segundo para pintar lo mismo.
    pub(super) fn tic_de_registro(
        &mut self,
        epoca: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if epoca != self.log_epoca {
            // De una apertura anterior: se deja morir sin rearmar.
            return Vec::new();
        }
        self.sondear_registro(buzon);
        // Y de paso se tira del registro del daemon (#328), colgado de ESTE
        // temporizador y no de uno propio: dos relojes sobre el mismo panel
        // son dos cosas que apagar al cerrarlo, y la segunda es la que se
        // olvida. La respuesta vuelve por el buzón, así que el actor sigue
        // siendo el único que escribe.
        self.pedir_registro_remoto(backend, buzon);
        let ahora = self
            .log_ring
            .as_ref()
            .map_or(0, norte_config::logring::LogRing::pushed);
        if ahora == self.log_visto {
            return Vec::new();
        }
        self.log_visto = ahora;
        // Solo lo que SIGUE el final se refresca solo. Quien se ha despegado
        // está leyendo algo concreto, y moverle la lista debajo es peor que no
        // enseñarle lo nuevo — que además va a seguir ahí cuando vuelva.
        if !self.log_panel.following() {
            return Vec::new();
        }
        let (_, salidas) = self.repintar_registro();
        salidas
    }

    /// Tira del registro del DAEMON desde donde se quedó (#328).
    ///
    /// Se pregunta SIEMPRE que el panel esté abierto, incluso con la fuente
    /// puesta en «esta ventana»: es la única forma de saber si hay una segunda
    /// fuente que ofrecer, y por tanto de decidir si el selector se pinta. Es
    /// una llamada cada medio segundo mientras alguien mira el registro; un
    /// panel cerrado no cuesta nada, que es donde está la mayor parte del
    /// tiempo.
    ///
    /// La época viaja con la petición: entre pedir y contestar caben un cierre
    /// y una apertura, y la respuesta de la sesión anterior tiene que morir en
    /// vez de aterrizar en el panel nuevo.
    pub(super) fn pedir_registro_remoto(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if self.huecos_de_registro().is_empty()
            // Con una en vuelo no se encola otra: un daemon que tardara más de
            // medio segundo acumularía una petición por tic para siempre.
            || self.log_remoto.en_vuelo
            // Y a un daemon que ya ha dicho que no tiene registro no se le
            // vuelve a preguntar. La negativa NO puede cambiar mientras ese
            // daemon viva: sale de una feature de compilación o de un montaje
            // que falló al arrancar. Seguir sondeando eran dos RPC por segundo,
            // para siempre, por una respuesta que no puede ser otra.
            //
            // Es asimétrico a propósito. Lo POSITIVO sí hay que seguir
            // pidiéndolo —el registro crece— y por eso `Sirve` no corta nada.
            || !self.log_remoto.debe_pedir()
        {
            return;
        }
        self.log_remoto.en_vuelo = true;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let epoca = self.log_epoca;
        let cursor = self.log_remoto.cursor;
        tokio::spawn(async move {
            let r = backend.log_tail(cursor, MAX_REMOTO).await;
            let _ = buzon
                .send(Mensaje::RegistroRemoto(epoca, Box::new(r)))
                .await;
        });
    }

    /// Aterriza lo que el daemon contestó a `log.tail`.
    pub(super) fn aterrizar_registro_remoto(
        &mut self,
        epoca: u64,
        res: Result<norte_proto::methods::LogTailResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if epoca != self.log_epoca {
            // De una apertura anterior. Ni sus líneas ni su cursor valen ya, y
            // `en_vuelo` es de la apertura de AHORA: tocarlo desde aquí
            // desarmaría el guard de una petición que sigue viva.
            return Vec::new();
        }
        self.log_remoto.en_vuelo = false;
        let antes = (self.log_remoto.servicio, self.log_remoto.nivel.clone());
        let nuevas = match res {
            Ok(r) => {
                self.log_remoto.servicio = Servicio::Sirve;
                self.log_remoto.nivel = Some(r.level);
                self.log_remoto.cursor = Some(r.next);
                self.log_remoto.perdidas = self.log_remoto.perdidas.saturating_add(r.lost);
                let n = r.lines.len();
                self.log_remoto
                    .lineas
                    .extend(r.lines.into_iter().map(linea_de_wire));
                // El tope se aplica por delante: lo viejo es lo que se tira,
                // igual que en el anillo, y cuenta como perdido — que es lo que
                // impide que el recorte deje un hueco callado.
                let sobra = self
                    .log_remoto
                    .lineas
                    .len()
                    .saturating_sub(MAX_LINEAS_REMOTAS);
                if sobra > 0 {
                    self.log_remoto.lineas.drain(..sobra);
                    self.log_remoto.perdidas = self
                        .log_remoto
                        .perdidas
                        .saturating_add(sobra.try_into().unwrap_or(u64::MAX));
                }
                n
            }
            // La ÚNICA degradación alcanzable: un daemon de la misma versión
            // sin la feature `logging`. No hay comparación de versiones en
            // ningún sitio — uno más viejo ni siquiera completa el
            // `initialize`, así que jamás llega hasta aquí.
            Err(Error::Unsupported) => {
                self.log_remoto.servicio = Servicio::SinAnillo;
                0
            }
            // Un fallo cualquiera —la conexión se cayó, el daemon está
            // ocupado— NO es «este daemon no tiene registro»: decirlo sería
            // acusar de una carencia permanente a algo que se arregla solo en
            // la vuelta siguiente. Se calla y se reintenta al medio segundo.
            Err(_) => 0,
        };
        let cambia_el_estado = antes != (self.log_remoto.servicio, self.log_remoto.nivel.clone());
        self.repintar_si_hace_falta(nuevas > 0, cambia_el_estado)
    }

    /// Aterriza el nivel que el daemon dejó puesto de verdad.
    pub(super) fn aterrizar_nivel_remoto(
        &mut self,
        epoca: u64,
        res: Result<String, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if epoca != self.log_epoca {
            return Vec::new();
        }
        let antes = (self.log_remoto.servicio, self.log_remoto.nivel.clone());
        match res {
            Ok(nivel) => {
                self.log_remoto.servicio = Servicio::Sirve;
                self.log_remoto.nivel = Some(nivel);
            }
            Err(Error::Unsupported) => self.log_remoto.servicio = Servicio::SinAnillo,
            Err(_) => {}
        }
        let cambia = antes != (self.log_remoto.servicio, self.log_remoto.nivel.clone());
        self.repintar_si_hace_falta(false, cambia)
    }

    /// La regla de repintado que comparten las dos respuestas del daemon.
    ///
    /// Las líneas nuevas solo refrescan al que SIGUE el final —moverle la
    /// lista debajo a quien se ha despegado es peor que no enseñarle lo
    /// nuevo—, pero un cambio de ESTADO (apareció una segunda fuente, el
    /// daemon dijo que no tiene registro, cambió su nivel) se pinta siempre:
    /// no mueve la lista y es justo lo que hay que decir.
    fn repintar_si_hace_falta(
        &mut self,
        hay_lineas: bool,
        cambia_el_estado: bool,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if cambia_el_estado || (hay_lineas && self.log_panel.following()) {
            let (_, salidas) = self.repintar_registro();
            return salidas;
        }
        Vec::new()
    }

    /// Los huecos que ahora mismo pintan el registro.
    pub(super) fn huecos_de_registro(&self) -> Vec<u32> {
        self.reparto
            .placements
            .iter()
            .filter(|(s, _)| !self.huecos.contains_key(&s.0))
            .filter(|(s, _)| kind_de(&self.arbol, *s).is_some_and(|k| k.as_str() == KIND))
            .map(|(s, _)| s.0)
            .collect()
    }
}
