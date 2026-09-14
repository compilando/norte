//! Las teclas: qué verbo resuelve cada acorde, y en qué pantalla.
//!
//! Parte de `controller`: son métodos de `Estado`, movidos aquí sin
//! tocarlos (ADR 0086). El único escritor sigue siendo el actor.

// Estos módulos son el mismo `impl Estado` partido en trozos, así que usan
// los mismos imports que el padre. Enumerarlos aquí sería una lista de
// cuarenta líneas por fichero, en 32 ficheros, que se desincroniza en cuanto
// el padre importa algo — `super::*` la sigue sola.
#[allow(clippy::wildcard_imports)]
use super::*;

impl Estado {
    /// La tecla, cuando hay un CONTEXTO DE ENTRADA abierto que se la queda.
    ///
    /// `None` = no había ninguno (o el que había no la quiso) y la tecla
    /// sigue su camino normal: el resolver del listado.
    ///
    /// El ORDEN es el de quien tapa a quién. El visor es otra pantalla
    /// entera; la ayuda tapa al listado y desde ella se puede abrir la
    /// paleta, así que va antes; la paleta es un editor de texto libre; y el
    /// buscador incremental solo se queda las teclas de TEXTO.
    /// Las teclas de un diálogo: contestarlo o cancelarlo, y nada más.
    ///
    /// El TEXTO no pasa por aquí. Lo teclea el campo del renderer y llega por
    /// `dialog_input`, que es lo que permite que los bytes aprobados sean los
    /// tecleados y no una reconstrucción a partir de teclas sueltas.
    ///
    /// `Enter` elige la primera respuesta NO destructiva, así que en el
    /// diálogo de aprobación de un agente elige `deny`: aprobar una mutación
    /// que uno no pidió no puede ser lo que pasa por dejar el dedo en Enter.
    ///
    /// Una tecla que no es ninguna de las dos se COME igual: un modal que
    /// deja pasar la tecla que no entiende no es un modal.
    pub(super) fn tecla_en_dialogo(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(d) = self.dialogos.last() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let id = d.id;
        let tecleando = d.vista.input.is_some();
        // DOS REGÍMENES, el mismo par que el TUI y que el resto de campos de
        // este host. Con un campo abierto las teclas son LETRAS: resolverlas
        // por el keymap convertiría escribir un nombre de fichero en
        // contestar la pregunta, porque no hay verbo `dialog.*` para «teclea
        // una letra». Sin campo, la tecla pasa por el resolutor COMPARTIDO,
        // que es lo que hace que un preset que reata `dialog.confirm` cambie
        // esta ventana y no solo el TUI.
        let verbo = if tecleando {
            match k.key.as_str() {
                "Enter" | "enter" => Some("dialog.confirm"),
                "Escape" | "esc" => Some("dialog.cancel"),
                _ => None,
            }
        } else {
            let Ok(chord) = k.to_chord() else {
                return (self.aplicada(), Vec::new());
            };
            match self.resolver_dialogo.push(chord) {
                Resolution::Run { command, .. } => match command.as_str() {
                    "dialog.confirm" => Some("dialog.confirm"),
                    "dialog.cancel" => Some("dialog.cancel"),
                    "dialog.approve" => Some("dialog.approve"),
                    "dialog.deny" => Some("dialog.deny"),
                    // Las cuatro salidas de una colisión (#287). Cada una
                    // nombra SU respuesta: `dialog.confirm` sobre una
                    // colisión no elige ninguna, porque «confirmar» no dice
                    // cuál de las cuatro, y la que se elige por descarte es
                    // la que destruye.
                    "dialog.overwrite" => Some("dialog.overwrite"),
                    "dialog.skip" => Some("dialog.skip"),
                    "dialog.rename" => Some("dialog.rename"),
                    "dialog.newer" => Some("dialog.newer"),
                    _ => None,
                },
                _ => None,
            }
        };
        let Some(d) = self.dialogos.last() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // El VERBO elige entre las respuestas que ESTE diálogo ofrece: una
        // que no ofrece no se interpreta —no hay respuestas implícitas en una
        // superficie de decisión— y por eso `dialog.confirm` sobre una
        // aprobación no aprueba: la afirmativa de una aprobación se llama
        // `approve` a propósito, para que un renderer no las confunda.
        let elegido = match verbo {
            Some("dialog.confirm") => d.vista.choices.iter().find(|c| c.id == "confirm"),
            Some("dialog.approve") => d.vista.choices.iter().find(|c| c.id == "approve"),
            Some("dialog.deny") => d.vista.choices.iter().find(|c| c.id == "deny"),
            // Las de la colisión, cada una por su nombre. Un diálogo que no
            // las ofrece las ignora, que es la regla de siempre: aquí no hay
            // respuestas implícitas.
            Some("dialog.overwrite") => d.vista.choices.iter().find(|c| c.id == "overwrite"),
            Some("dialog.skip") => d.vista.choices.iter().find(|c| c.id == "skip"),
            Some("dialog.rename") => d.vista.choices.iter().find(|c| c.id == "rename"),
            Some("dialog.newer") => d.vista.choices.iter().find(|c| c.id == "newer"),
            Some("dialog.cancel") => d
                .vista
                .choices
                .iter()
                .find(|c| c.id == "cancel" || c.id == "deny")
                // Cerrar SIEMPRE se puede: si el diálogo no ofrece cancelar
                // ni denegar, la respuesta es la última que no destruye.
                .or_else(|| d.vista.choices.iter().rfind(|c| !c.destructive)),
            _ => None,
        }
        .map(|c| c.id.clone());
        let Some(choice) = elegido else {
            return (self.aplicada(), Vec::new());
        };
        // Sin secreto, y no es un olvido (#327): una TECLA no puede llevar una
        // contraseña. Sobre un diálogo que la pide, este camino confirma con
        // `None`, o sea de forma inerte, y la única puerta que entrega es la
        // del renderer —el botón y el Enter del propio campo—, que sí tiene el
        // valor. Es lo que se quiere: el host no guarda lo tecleado, así que
        // un acorde no puede entregar algo que el host no tiene.
        self.responder_dialogo(id, &choice, None, backend, buzon)
    }

    /// El verbo `dialog.*` de una tecla, por el resolutor COMPARTIDO (#287).
    ///
    /// Es la única puerta: las superficies modales de esta ventana atendían
    /// teclas fijas, así que un preset que reataba `dialog.up` cambiaba el TUI
    /// y no la ventana — justo la deriva que el catálogo común existe para no
    /// tener.
    ///
    /// `None` cuando la tecla no forma acorde, no está atada, o abre una
    /// secuencia todavía sin resolver. En los tres casos la superficie no hace
    /// nada, que es lo que hacía antes con una tecla que no entendía.
    pub(super) fn verbo_de_dialogo(&mut self, k: &crate::keys::KeyInput) -> Option<String> {
        let chord = k.to_chord().ok()?;
        match self.resolver_dialogo.push(chord) {
            Resolution::Run { command, .. } => Some(command),
            _ => None,
        }
    }

    /// El acorde atado a un verbo `dialog.*`, ya pintado. Vacío si ninguno.
    ///
    /// Para los PIES de las superficies modales: se pintan con lo que el
    /// keymap dice, no con un literal traducido, porque el literal deja de ser
    /// cierto en cuanto alguien reata la tecla.
    pub(super) fn acorde_de_dialogo(&self, comando: &str) -> String {
        self.resolver_dialogo
            .effective()
            .bindings()
            .into_iter()
            .find(|(_, c)| *c == comando)
            .map(|(seq, _)| norte_frontend::keymap::paint_chord(&seq))
            .unwrap_or_default()
    }

    /// ¿Hay delante una pantalla que se quedaría una tecla antes que el
    /// listado? Las MISMAS que [`Self::tecla_de_un_overlay`] atiende, salvo
    /// el menú, que es de quien pregunta.
    ///
    /// Es una segunda lista a propósito y no un desvío por aquella función:
    /// esa ATIENDE la tecla (cancela un visor en vuelo, abandona un plan), y
    /// preguntar no puede tener efectos. Quien añada un overlay allí lo añade
    /// aquí; `alt_solo_no_abre_el_menu_encima_de_un_dialogo` pinea el caso
    /// que importa.
    pub(super) fn algo_se_queda_las_teclas(&self) -> bool {
        !self.dialogos.is_empty()
            || self.asistente.is_some()
            || self.escritorio.salida.is_some()
            || self.escritorio.programa.is_some()
            || self.ayuda.is_some()
            || self.sincronizacion.is_some()
            || self.comparacion.is_some()
            || self.revision_ia.is_some()
            || self.busqueda.is_some()
            || self.selector_disposicion.is_some()
            || self.selector_columnas.is_some()
            || self.selector.is_some()
            || self.selector_perfil.is_some()
            || self.tema_elegido.is_some()
            || self.extensiones.is_some()
            || self.agencia.panel
            || self.ajustes.is_some()
            || self.visor.is_some()
            || self.paleta.is_some()
    }

    pub(super) fn tecla_de_un_overlay(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        // El DIÁLOGO va antes que todo lo demás: es la única superficie
        // modal de verdad —una pregunta que hay que contestar antes de
        // seguir— y las teclas que no atrapaba caían al listado de DEBAJO.
        // Con el prompt de un nombre abierto, `Backspace` navegaba al padre
        // mientras se tecleaba y `Enter` entraba en el directorio bajo el
        // cursor en vez de confirmar; con una confirmación de borrado
        // abierta, `Enter` navegaba la pantalla que la pregunta tapaba.
        if !self.dialogos.is_empty() {
            return Some(self.tecla_en_dialogo(k, backend, buzon));
        }
        // El asistente de primer arranque (spec 2026-09-10): detrás del
        // diálogo, que es una pregunta de seguridad, y delante de todo lo
        // demás — está preguntando qué teclas tener, así que las suyas son
        // fijas.
        if self.asistente.is_some() {
            return Some(self.tecla_en_asistente(k, backend, buzon));
        }
        // La SALIDA de un comando de extensión se queda TODAS las teclas
        // mientras está: pinta a pantalla completa, así que un modal que
        // dejara pasar la que no entiende no es un modal. `Enter` y `Escape`
        // la cierran —las dos, porque cerrar un panel de lectura con `Enter`
        // es el reflejo—; el resto no significan nada aquí y no caen a lo de
        // debajo, donde una confirmación de borrado podía estar esperando un
        // sí que el lector no ve. El momento lo elige el PLUGIN, que decide
        // cuándo contesta su comando.
        if self.escritorio.salida.is_some() {
            if matches!(k.key.as_str(), "Escape" | "esc" | "Enter" | "enter") {
                return Some(self.cerrar_salida());
            }
            return Some((self.aplicada(), Vec::new()));
        }
        // La salida de un programa (#312), por lo mismo y con las mismas
        // teclas: se lee y se cierra.
        if self.escritorio.programa.is_some() {
            if matches!(k.key.as_str(), "Escape" | "esc" | "Enter" | "enter") {
                return Some(self.cerrar_salida_de_programa());
            }
            return Some((self.aplicada(), Vec::new()));
        }
        // La AYUDA va primero, incluso antes que el visor, y no por gusto:
        // se abre ENCIMA de lo que hubiera —también encima del visor, que es
        // desde donde se pide la página del visor— y quien está arriba se
        // queda las teclas. Al revés, `F1` en el visor abría una ayuda que no
        // recibía ni una tecla y que ninguna podía cerrar.
        if self.ayuda.is_some() {
            return Some(self.tecla_en_ayuda(k, backend, buzon));
        }
        // La REVISIÓN de un plan de renombrado va antes que el resto de
        // overlays y solo por detrás del diálogo y de la ayuda: es una
        // pantalla que se lee entera antes de aprobar una mutación, y una
        // tecla que se le escapara al listado de debajo movería el cursor
        // bajo un plan que sigue esperando un sí.
        // El panel de sincronización, igual que el de diferencias: mientras
        // esté abierto se queda las teclas.
        if self.sincronizacion.is_some() {
            return Some(self.tecla_en_sincronizacion(k, backend, buzon));
        }
        // El panel de diferencias, cuando está abierto, se queda las teclas:
        // es una pantalla entera, y una flecha que se le escapara movería el
        // listado que hay debajo.
        if self.comparacion.is_some() {
            return Some(self.tecla_en_comparacion(k, backend, buzon));
        }
        if self.revision_ia.is_some() {
            return Some(self.tecla_en_revision_ia(k, backend, buzon));
        }
        if self.busqueda.is_some() {
            return Some(self.tecla_en_busqueda(k, backend, buzon));
        }
        if self.selector_disposicion.is_some() {
            return Some(self.tecla_en_disposiciones(k, backend, buzon));
        }
        if self.selector_columnas.is_some() {
            return Some(self.tecla_en_columnas(k, backend, buzon));
        }
        if self.selector.is_some() {
            return Some(self.tecla_en_selector(k, backend, buzon));
        }
        if self.selector_perfil.is_some() {
            return Some(self.tecla_en_perfiles(k, backend, buzon));
        }
        if self.tema_elegido.is_some() {
            return Some(self.tecla_en_tema(k, buzon));
        }
        if self.extensiones.is_some() {
            return Some(self.tecla_en_extensiones(k, backend, buzon));
        }
        if self.agencia.panel {
            return Some(self.tecla_en_agentes(k, backend, buzon));
        }
        if self.ajustes.is_some() {
            return Some(self.tecla_en_ajustes(k, buzon));
        }
        if self.visor.is_some() {
            return Some(self.tecla_en_visor(k, backend, buzon));
        }
        // Cualquier tecla del LISTADO cancela una lectura de visor en vuelo.
        // El usuario pulsó F3, se cansó y siguió a lo suyo: abrirle el visor
        // medio segundo después es abrir una ventana que ya nadie pidió — y
        // cambiarle el teclado de mapa sin gesto suyo. (Un segundo F3 pide su
        // propia lectura y se queda con el testigo nuevo.)
        self.visor_en_vuelo = None;
        // El menú desplegado se queda las teclas, igual que la paleta: una
        // flecha que se le escapara movería el listado de debajo.
        if self.menu.is_some() {
            return Some(self.tecla_en_menu(k, backend, buzon));
        }
        if self.paleta.is_some() {
            return Some(self.tecla_en_paleta(k, backend, buzon));
        }
        if self.hueco().pane.quick().is_some() {
            return self.tecla_en_quick(k);
        }
        // Y, cuando NADIE más la quería, `Escape` abandona un plan de
        // renombrado que siga pensando. Va la ÚLTIMA, que es la única
        // posición en la que «no la quería nadie» es verdad: por encima se
        // comía el `Escape` que cierra la paleta y el que cancela el filtro
        // rápido —una tecla haciendo dos cosas mal a la vez— y se saltaba el
        // corte del visor en vuelo.
        //
        // Y solo `Escape`, no cualquier tecla como el visor: el modelo tarda
        // de verdad y seguir navegando mientras piensa es lo normal. Lo que
        // no puede pasar es que el plan se abra encima de la pantalla medio
        // minuto después de que su dueño se haya ido a otra cosa.
        if self.ia_en_vuelo.is_some() && (k.key == "Escape" || k.key == "esc") {
            self.epoca_ia += 1;
            self.ia_en_vuelo = None;
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-plan-abandoned",
            )));
            let cambio = ViewChange::Status(self.status.clone());
            return Some((self.aplicada(), vec![self.parche(vec![cambio])]));
        }
        None
    }

    /// Una tecla: la resuelve el keymap COMPARTIDO y el host solo ejecuta.
    ///
    /// Los cuatro desenlaces son los del resolver, y ninguno se queda
    /// callado: un comando corre, un prefijo o un contador a medias se
    /// PINTAN (lo que no se ve no se puede cancelar), una tecla ligada a algo
    /// que aquí no se puede hacer lo dice, y una tecla sin binding se
    /// descarta dejando el estado limpio.
    pub(super) fn tecla(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if let Some(salida) = self.tecla_de_un_overlay(k, backend, buzon) {
            return salida;
        }
        // El visor ACOPLADO con el foco se queda las teclas del visor (#291),
        // como en la TUI: es el mismo visor en otro sitio. Lo que su keymap
        // no ata —el tabulador, un atajo global— sigue su camino normal.
        if let Some(salida) = self.tecla_en_preview(k) {
            return salida;
        }
        let Ok(chord) = k.to_chord() else {
            // Una tecla que el adaptador no entiende no se adivina.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-key-unmapped".to_owned(),
                },
                Vec::new(),
            );
        };
        match self.resolver.push(chord) {
            Resolution::Run { command, count } => {
                let veces = count.times();
                let Some(efecto) = efecto_de(&command, veces) else {
                    // En el catálogo, ligada, y este host no la hace. Se
                    // dice con la MISMA frase que el TUI.
                    let frase = norte_frontend::keymap::unavailable_message_in(
                        &command,
                        Availability::NotHere,
                        self.lang,
                    );
                    self.status.message = Some(clamp_display(frase));
                    self.status.pending = None;
                    let cambio = ViewChange::Status(self.status.clone());
                    return (
                        ActionAck::Unavailable {
                            reason_key: "cmd-not-here".to_owned(),
                        },
                        vec![self.parche(vec![cambio])],
                    );
                };
                self.status.pending = None;
                // La secuencia se cerró: el panel de continuaciones describe
                // teclas que ya no están vivas, y su propio contrato dice que
                // se tira en cuanto cambia el estado del resolver.
                self.whichkey = None;
                self.aplicar_efecto(efecto, backend, buzon)
            }
            Resolution::Pending(_) | Resolution::Counting(_) => {
                self.status.pending = Some(PendingView {
                    chords: self
                        .resolver
                        .pending()
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" "),
                    count: self.resolver.count(),
                });
                // El panel se construye AQUÍ, en la transición, y no al
                // proyectar: `build` cuesta varias cadenas y uno o dos
                // formatos Fluent por fila.
                self.whichkey = Some(norte_frontend::whichkey::WhichKeyRows::build(
                    &self.efectivo,
                    self.resolver.pending(),
                    self.resolver.count(),
                    self.lang,
                ));
                let cambios = vec![
                    ViewChange::Status(self.status.clone()),
                    ViewChange::WhichKey {
                        whichkey: self.vista_whichkey(),
                    },
                ];
                (self.aplicada(), vec![self.parche(cambios)])
            }
            Resolution::Unavailable { command, why } => {
                let frase =
                    norte_frontend::keymap::unavailable_message_in(&command, why, self.lang);
                self.status.message = Some(clamp_display(frase));
                self.status.pending = None;
                self.whichkey = None;
                let cambio = ViewChange::Status(self.status.clone());
                (
                    ActionAck::Unavailable {
                        reason_key: match why {
                            Availability::Here => "cmd-here",
                            Availability::NotBuilt { .. } => "cmd-not-built",
                            Availability::NotHere => "cmd-not-here",
                        }
                        .to_owned(),
                    },
                    vec![self.parche(vec![cambio])],
                )
            }
            Resolution::Reset => {
                let habia = self.status.pending.take().is_some();
                let panel = self.whichkey.take().is_some();
                if habia || panel {
                    let cambios = vec![
                        ViewChange::Status(self.status.clone()),
                        ViewChange::WhichKey { whichkey: None },
                    ];
                    return (self.aplicada(), vec![self.parche(cambios)]);
                }
                (self.aplicada(), Vec::new())
            }
        }
    }

    /// Las teclas mientras la paleta está abierta.
    ///
    /// Fijas a propósito: `esc` cierra, `enter` corre lo seleccionado, las
    /// flechas mueven y lo demás teclea. Es lo mismo que hace el TUI, y por
    /// el mismo motivo — el catálogo no tiene comandos para esto.
    /// Teclas del menú desplegado.
    ///
    /// FIJAS, como las de la paleta y por lo mismo: no hay verbos `dialog.*`
    /// para «menú siguiente», así que tampoco pueden salir del keymap. Las
    /// flechas recorren, `Enter` ejecuta y `Escape` cierra; cualquier otra se
    /// descarta en vez de caer al listado de debajo, que estaría actuando
    /// sobre una pantalla que el lector no está mirando.
    pub(super) fn tecla_en_menu(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(m) = self.menu.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => self.olvidar_menu(),
            "ArrowLeft" | "left" => m.cycle_menu(-1),
            "ArrowRight" | "right" => m.cycle_menu(1),
            "ArrowUp" | "up" => m.cycle_item(-1),
            "ArrowDown" | "down" => m.cycle_item(1),
            "Enter" | "enter" => {
                let elegido = m.selected();
                return self.ejecutar_del_menu(elegido, backend, buzon);
            }
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Menu {
            menu: self.vista_menu(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Cierra el menú y corre lo elegido.
    ///
    /// El cierre viaja en su PROPIO parche y ANTES del efecto, por lo mismo
    /// que la paleta: el comando puede abrir otra pantalla, y hacerlo por
    /// detrás del menú lo dejaría comiéndose las teclas de la que acaba de
    /// abrirse.
    pub(super) fn ejecutar_del_menu(
        &mut self,
        elegido: Option<&'static str>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.olvidar_menu();
        let cierre = self.parche(vec![ViewChange::Menu {
            menu: self.vista_menu(),
        }]);
        let Some(cmd) = elegido else {
            return (self.aplicada(), vec![cierre]);
        };
        // Por el MISMO camino que una tecla: un menú es otra puerta al
        // catálogo, no un segundo despachador.
        let (ack, mut resto) = match efecto_de(cmd, 1) {
            Some(efecto) => self.aplicar_efecto(efecto, backend, buzon),
            None => self.no_implementado(cmd),
        };
        let mut envios = vec![cierre];
        envios.append(&mut resto);
        (ack, envios)
    }

    pub(super) fn tecla_en_paleta(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(p) = self.paleta.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let alto = 10;
        match k.key.as_str() {
            "Escape" | "esc" => {
                self.paleta = None;
            }
            "Enter" | "enter" => {
                let elegido = p.selected();
                self.paleta = None;
                if let Some(cmd) = elegido {
                    norte_frontend::session::note_palette_recent(&mut self.paleta_recientes, &cmd);
                    // El cierre viaja en su PROPIO parche y antes que el
                    // efecto. Sin él, un renderer que aplica parches —que es
                    // lo que hace el de referencia— recibía el cambio del
                    // comando y ninguno de la paleta, y la seguía pintando
                    // encima del listado hasta la siguiente foto.
                    let cierre = self.parche(vec![ViewChange::Palette { palette: None }]);
                    // Se ejecuta por el MISMO camino que una tecla: la
                    // paleta es otra puerta al catálogo, no un segundo
                    // despachador.
                    let (ack, mut resto) = match efecto_de(&cmd, 1) {
                        Some(efecto) => self.aplicar_efecto(efecto, backend, buzon),
                        // Una fila de PLUGIN no está en el catálogo de
                        // comandos y no puede estarlo: la aporta un tercero
                        // en tiempo de ejecución.
                        None if cmd.starts_with("plugin:") => {
                            self.ejecutar_de_plugin(&cmd, backend, buzon)
                        }
                        // Una fila de RENAMER (C3, ADR 0095): pide el plan y lo
                        // mete en la MISMA revisión que el de la IA.
                        None if cmd.starts_with("renamer:") => {
                            self.ejecutar_de_renamer(&cmd, backend, buzon)
                        }
                        None => self.no_implementado(&cmd),
                    };
                    let mut envios = vec![cierre];
                    envios.append(&mut resto);
                    return (ack, envios);
                }
            }
            "ArrowDown" | "down" => p.down(),
            "ArrowUp" | "up" => p.up(),
            "PageDown" | "pgdn" => p.page_down(alto),
            "PageUp" | "pgup" => p.page_up(alto),
            "Backspace" | "backspace" => p.backspace(),
            otra => {
                // Una tecla de TEXTO es un punto de código, no una unidad
                // UTF-16 ni un nombre de tecla: `ArrowLeft` no se teclea.
                let mut chars = otra.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if !k.ctrl && !k.alt && !k.meta => p.push_char(c),
                    _ => return (self.aplicada(), Vec::new()),
                }
            }
        }
        let cambio = ViewChange::Palette {
            palette: self.vista_paleta(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Un comando del catálogo que este host no ejecuta, dicho con la misma
    /// frase que el TUI.
    pub(super) fn no_implementado(
        &mut self,
        cmd: &str,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let frase =
            norte_frontend::keymap::unavailable_message_in(cmd, Availability::NotHere, self.lang);
        self.status.message = Some(clamp_display(frase));
        let cambio = ViewChange::Status(self.status.clone());
        (
            ActionAck::Unavailable {
                reason_key: "cmd-not-here".to_owned(),
            },
            vec![self.parche(vec![cambio])],
        )
    }
}
