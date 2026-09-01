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

/// El kind que ocupa un hueco de registro. El mismo que la TUI.
pub(super) const KIND: &str = "log";

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
        let (visibles, desde) = self.log_panel.view(&lineas, self.log_filas);
        let ventana = visibles
            .iter()
            .skip(desde)
            .take(self.log_filas)
            .map(|l| Self::linea_de_registro(l));
        crate::dto::LogSlotView {
            slot_id: slot,
            lines: ventana.collect(),
            level: self.log_panel.level().wire().to_owned(),
            // El filtro lo TECLEA el lector, así que se pinta como cualquier
            // otro texto de fuera: enmascarado y acotado.
            filter: clamp_display(
                norte_frontend::display_name(self.log_panel.filter().as_bytes()).0,
            ),
            following: self.log_panel.following(),
            total: visibles.len() as u64,
            first_visible: desde as u64,
            dropped_note: match self
                .log_ring
                .as_ref()
                .map_or(0, norte_config::logring::LogRing::dropped)
            {
                0 => String::new(),
                n => clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "log-dropped",
                    &[("n", &n.to_string())],
                )),
            },
            // Solo cuando se CAPTURA más de lo que se enseña: decir «capturando
            // info» sobre un panel que enseña info sería ruido, y el ruido es
            // lo que hace que se deje de leer la línea que sí importa.
            capturing: match self
                .log_ring
                .as_ref()
                .map(norte_config::logring::LogRing::level)
            {
                Some(cap) if cap > self.log_panel.level() => clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "log-capturing",
                    &[("level", cap.wire())],
                )),
                _ => String::new(),
            },
            source: clamp_display(norte_i18n::t_in(
                self.lang,
                if self.log_ring.is_some() {
                    // De ESTE proceso, y decirlo es el punto: la ventana
                    // arranca su propio daemon (#300), así que aquí NO está lo
                    // del daemon —los providers, el journal, la política—, que
                    // es la mitad interesante. Callarlo haría que el panel
                    // pareciera roto: alguien lo abre mientras una conexión
                    // falla, no ve la línea que lo explica, y concluye que el
                    // panel no funciona en vez de que está mirando otro sitio.
                    "log-source-window"
                } else {
                    // No es «no se registra nada»: el proceso sigue
                    // escribiendo a su fichero. Lo que falta es el anillo en
                    // memoria, que es lo que este panel lee — y decir lo
                    // primero sería una respuesta más tranquilizadora que la
                    // verdad. La TUI ya tenía la frase exacta.
                    "log-no-ring"
                },
            )),
        }
    }

    /// Una línea, saneada.
    ///
    /// El mensaje pasa por `display_name` como cualquier texto que se pinta, y
    /// aquí con un motivo propio: un mensaje de registro puede llevar dentro el
    /// nombre de un fichero que alguien eligió, y un `U+202E` ahí reordena la
    /// línea entera del panel.
    fn linea_de_registro(l: &LogLine) -> crate::dto::LogLineView {
        let (target, t_hostil) = norte_frontend::display_name(l.target.as_bytes());
        let (mensaje, m_hostil) = norte_frontend::display_name(l.message.as_bytes());
        crate::dto::LogLineView {
            time: norte_frontend::format::hora_utc(l.epoch_ms),
            level: l.level.wire().to_owned(),
            target: clamp_display(target),
            message: clamp_display(mensaje),
            hostile: t_hostil || m_hostil,
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
    pub(super) fn nivel_de_registro(
        &mut self,
        nivel: &str,
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
        let visibles = self.log_panel.visible_count(&lineas);
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
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if epoca != self.log_epoca {
            // De una apertura anterior: se deja morir sin rearmar.
            return Vec::new();
        }
        self.sondear_registro(buzon);
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
