//! Apagar el host.
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
    /// Apaga: vuelca la sesión si esta ventana es su dueña, y DICE si algo
    /// quedó sin escribir.
    ///
    /// Volcar aquí y no solo en un tick es lo que hace que cerrar justo
    /// después de navegar guarde el directorio nuevo y no el anterior. Un
    /// conflicto en el último momento no se reintenta a lo loco: se informa,
    /// que es lo único honesto cuando ya no hay pantalla que corregir.
    pub(super) async fn apagar(&mut self, backend: &dyn HostBackend) -> ShutdownReport {
        // Una task viva al cerrar es trabajo sin terminar, lo diga la sesión
        // o no: cerrar a mitad de una copia y reportar «todo bien» es
        // exactamente lo que este informe existe para no hacer.
        let hay_tasks = self.tasks.values().any(|t| {
            matches!(
                t.vista.state,
                crate::dto::TaskStateView::Queued | crate::dto::TaskStateView::Running
            )
        });
        if !self.sesion.owner || self.sesion.futuro {
            // Una ventana suelta no escribe, y una sesión del futuro no se
            // machaca.
            return ShutdownReport {
                incomplete: hay_tasks,
            };
        }
        let ahora = u64::try_from(ahora_ms()).unwrap_or(0);
        let mut body = self.capturar_sesion();
        // Con una escritura del tic EN VUELO no se manda otra encima: iría
        // con la misma revisión y una de las dos conflictaría seguro. Si lo
        // que se estaba escribiendo es lo que hay ahora, no queda nada por
        // escribir; si no, lo último no llegó, y se dice.
        if let Some(en_vuelo) = &self.sesion.en_vuelo {
            return ShutdownReport {
                incomplete: hay_tasks || **en_vuelo != body,
            };
        }
        let vivos: Vec<SlotId> = self.huecos.keys().map(|id| SlotId(*id)).collect();
        if self
            .sesion
            .policy
            .prepare(&mut body, &vivos, ahora)
            .is_none()
        {
            // Nada cambió desde lo último que se mandó.
            return ShutdownReport {
                incomplete: hay_tasks,
            };
        }
        let Ok(json) = serde_json::to_value(&body) else {
            return ShutdownReport { incomplete: true };
        };
        let puesta = backend
            .session_put(
                norte_frontend::session::SCHEMA_VERSION,
                self.sesion.revision,
                json,
            )
            .await;
        // Que el cuerpo se PASE de tamaño no es lo mismo que un conflicto, y
        // tratarlo igual era perder la pantalla entera del lector sin decir
        // nada (#316): el core rehúsa el `put` COMPLETO y deja almacenado lo
        // que hubiera, o sea dónde estaba el lector hace días. La TUI ya
        // degradaba; esta ventana no, que es la divergencia silenciosa del ADR
        // 0077.
        //
        // Se tira lo mismo que tira la TUI, porque la decisión es compartida
        // (`SessionBody::degrade_for_size`), y se reintenta UNA vez: si ni sin
        // historial cabe, no hay nada más que degradar que no sea dónde está
        // el lector, y eso es lo que había que salvar.
        // El `degrade_for_size` va en el CUERPO del `if` y no en un guard de
        // `match`: muta `body`, y un guard con efecto es una trampa para el
        // siguiente que lo toque.
        let mut puesta = puesta;
        if matches!(puesta, Err(Error::LimitExceeded { .. })) && body.degrade_for_size() {
            puesta = match serde_json::to_value(&body) {
                Ok(json) => {
                    backend
                        .session_put(
                            norte_frontend::session::SCHEMA_VERSION,
                            self.sesion.revision,
                            json,
                        )
                        .await
                }
                // Lo mismo que doce líneas más arriba: un cuerpo que no
                // serializa es «no se escribió», no un pánico del core.
                Err(_) => return ShutdownReport { incomplete: true },
            };
        }
        // Un conflicto es otra ventana que escribió en medio: lo suyo se
        // queda, y se dice que lo nuestro no llegó. Pisarlo sería perder la
        // sesión de alguien.
        let Ok(rev) = puesta else {
            self.sesion.policy.resend();
            return ShutdownReport { incomplete: true };
        };
        self.sesion.revision = rev;
        self.sesion.policy.sent(std::sync::Arc::new(body));
        ShutdownReport {
            incomplete: hay_tasks,
        }
    }
}
