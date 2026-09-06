//! Navegar: entrar, subir y andar el rastro.
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
    /// Las tres acciones que CAMBIAN de directorio.
    ///
    /// Aparte de las de arriba porque son las únicas que dejan trabajo en
    /// vuelo: las demás terminan dentro de esta función.
    pub(super) fn navegacion(
        &mut self,
        accion: &UiAction,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match accion {
            UiAction::Activate {
                slot_id,
                key,
                generation,
            } => {
                let (slot_id, key, generation) = (*slot_id, *key, *generation);
                let Some(i) = self.fila_de(slot_id, key, generation) else {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                let Some(entrada) = self.hueco().pane.entries().get(i) else {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                };
                // Qué se puede navegar lo dice el crate COMPARTIDO: un
                // directorio, un enlace y un CONTENEDOR, que se abre por
                // dentro. Aquí se miraba `kind != Dir`, así que un `.zip` y un
                // symlink se entregaban al escritorio mientras el terminal
                // entraba en ellos — con un comentario, tres líneas más
                // abajo, afirmando que la decisión era la misma.
                let navegable = norte_frontend::nav::enter_target(entrada);
                if navegable.is_none() {
                    // Un FICHERO se abre, que es lo que hace un gestor
                    // ortodoxo: con el programa que el escritorio le asocie si
                    // está en este disco, y con el visor INTERNO si no —a
                    // `xdg-open` no se le puede dar un `sftp://`, y ahí el
                    // visor es lo único que se puede hacer—. La misma
                    // decisión que toma el TUI, y ahora de verdad: sale de la
                    // misma función (ADR 0077).
                    return if norte_frontend::shell::is_local(&entrada.path) {
                        self.abrir_externo()
                    } else {
                        self.pedir_visor(backend, buzon)
                    };
                }
                let destino = navegable.unwrap_or_else(|| entrada.path.clone());
                // Activar la fila `..` es SUBIR, y al subir el cursor
                // aterriza sobre el directorio del que se sale — lo mismo que
                // hace `UiAction::Parent` unas líneas más abajo. Sin esto, la
                // misma navegación dejaba el cursor en la primera fila según
                // se hubiera pedido con la fila o con la tecla, y subir y
                // bajar dejaba de ser reversible por una de las dos puertas.
                if self.hueco().pane.is_parent_row(i) {
                    let actual = self.hueco().pane.dir().clone();
                    self.hueco_mut().pane.set_pending_focus(actual);
                }
                (
                    self.aplicada(),
                    self.navegar(&destino, Trail::Record, backend, buzon),
                )
            }
            UiAction::Parent { slot_id } => {
                if *slot_id != self.activo() {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let actual = self.hueco().pane.dir().clone();
                let Some(padre) = actual.parent() else {
                    return (
                        ActionAck::Unavailable {
                            reason_key: "msg-nav-at-root".to_owned(),
                        },
                        Vec::new(),
                    );
                };
                // El cursor aterriza en el directorio del que se sale, no en
                // la primera fila: es lo que hace que subir y bajar sea
                // reversible. Lo resuelve `PaneState` al recibir el listado.
                self.hueco_mut().pane.set_pending_focus(actual);
                (
                    self.aplicada(),
                    self.navegar(&padre, Trail::Record, backend, buzon),
                )
            }
            UiAction::History { slot_id, back } => {
                let (slot_id, back) = (*slot_id, *back);
                if slot_id != self.activo() {
                    return (Self::obsoleta(StaleAction::Generation), Vec::new());
                }
                let actual = self.hueco().pane.dir().clone();
                let paso = if back {
                    TrailStep::Back
                } else {
                    TrailStep::Forward
                };
                let destino = if back {
                    self.hueco_mut().historial.step_back(actual)
                } else {
                    self.hueco_mut().historial.step_forward(actual)
                };
                let Some(destino) = destino else {
                    // Una tecla que se queda muda no se distingue de una
                    // rota: el rastro agotado lo DICE.
                    return (
                        ActionAck::Unavailable {
                            reason_key: paso.empty_message().to_owned(),
                        },
                        Vec::new(),
                    );
                };
                (
                    self.aplicada(),
                    self.navegar(&destino, Trail::Replay(paso), backend, buzon),
                )
            }
            _ => (Self::obsoleta(StaleAction::Generation), Vec::new()),
        }
    }

    /// Arranca una navegación: registra el paso en el rastro, marca el hueco
    /// como cargando y deja la petición EN VUELO con su testigo.
    pub(super) fn navegar(
        &mut self,
        destino: &VPath,
        trail: Trail,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.navegar_hueco(self.activo(), destino, trail, backend, buzon)
    }

    /// Lo mismo, sobre un hueco que NO tiene por qué ser el activo.
    ///
    /// Existe porque hay gestos que mueven OTRO panel: el espejo manda la
    /// ubicación del activo al destino, y un selector de volúmenes abierto
    /// para un lado de la pantalla monta ahí. Antes esto se hacía leyendo
    /// `activo()` tres veces por dentro, así que no había forma de decirlo.
    pub(super) fn navegar_hueco(
        &mut self,
        slot: u32,
        destino: &VPath,
        trail: Trail,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.token += 1;
        let token = RequestToken(self.token);
        let destino = destino.clone();
        let Some(hueco) = self.huecos.get_mut(&slot) else {
            return Vec::new();
        };
        let anterior = hueco.pane.dir().clone();
        // Un `Replay` es el rastro reproduciéndose: registrar ahí haría que
        // `back` se alimentara de sí mismo y el lector oscilara entre dos
        // directorios.
        if anterior != destino && trail == Trail::Record {
            hueco.historial.record(anterior);
        }
        // La memoria del cursor se toma con el dir que se ABANDONA todavía
        // puesto (contrato de `remember_cursor`).
        hueco.pane.remember_cursor();
        hueco.estado = SlotState::Loading;
        hueco.en_vuelo = Some(token);
        // El drenaje vive MÁS que la primera página: se marca aquí y solo lo
        // releva otra navegación del mismo hueco.
        hueco.drenando = Some(token);

        self.pedir_listado(slot, &destino, token, backend, buzon);

        let cambio = ViewChange::SlotState {
            slot_id: slot,
            state: SlotState::Loading,
        };
        vec![self.parche(vec![cambio])]
    }

    /// El índice de una fila, si la clave es de ESTA generación y existe.
    pub(super) fn fila_valida(&self, key: RowKey) -> Option<usize> {
        let i = usize::try_from(key.0).ok()?;
        (i < self.hueco().pane.entries().len()).then_some(i)
    }
}
