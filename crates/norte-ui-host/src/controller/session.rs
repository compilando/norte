//! Leer y volcar la sesión.
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
    /// Lee la sesión y la aplica, si se puede.
    ///
    /// Tres cosas se deciden aquí, y las tres son de ADR 0059:
    ///
    /// - **Quién escribe.** Una ventana SUELTA no escribe. La sesión es un
    ///   documento con un solo escritor, y dos ventanas guardando la suya
    ///   encima de la otra es exactamente lo que produce una pantalla que
    ///   nadie pidió.
    /// - **Qué se aplica.** Solo lo que este host entiende. Un hueco de un
    ///   kind desconocido NO se toca, ni siquiera para borrarlo.
    /// - **Qué NO se sobrescribe.** Si lo guardado es de un esquema más
    ///   nuevo, se arranca de la configuración y se deja quieto: arrancar sin
    ///   sesión es recuperable; machacar la de una versión futura no.
    pub(super) async fn leer_sesion(&mut self, backend: &dyn HostBackend) {
        let Ok((sesion, owner)) = backend.session_get().await else {
            // Sin sesión legible se arranca igual: es memoria de dónde
            // estabas, no un requisito para existir.
            return;
        };
        self.sesion.revision = sesion.revision;
        self.sesion.owner = owner;
        if sesion.version > norte_frontend::session::SCHEMA_VERSION {
            self.sesion.futuro = true;
            return;
        }
        if sesion.version == 0 {
            // Nadie la ha escrito todavía.
            return;
        }
        let Ok(body) = serde_json::from_value::<norte_frontend::session::SessionBody>(sesion.body)
        else {
            return;
        };
        self.aplicar_sesion(&body);
        self.sesion.conocidos = body.slots.keys().copied().collect();
        self.sesion.leida = body;
    }

    /// Los huecos que `[profile.start]` siembra, ya filtrados a los que ESTA
    /// pantalla tiene.
    ///
    /// Quién gana lo decide [`norte_frontend::config::profile_start_seeds`],
    /// que es de los dos frontends: la sesión manda, y el perfil solo dice
    /// dónde abre un hueco del que la sesión no sabe nada. Un id que el perfil
    /// nombre y esta disposición no coloque no tiene dónde abrir, así que se
    /// cae aquí.
    ///
    /// Apunta lo sembrado. Sin esa cuenta, un lector sin sesión guardada
    /// —instalación nueva, o un `session_get` que ni se pudo leer— volvía al
    /// directorio de arranque del perfil cada vez que entraba y salía de él:
    /// para él la sesión no sabe nunca nada, así que el veto de arriba no veta.
    pub(super) fn siembra_de_perfil(&mut self) -> Vec<(u32, VPath)> {
        let siembra: Vec<(u32, VPath)> = norte_frontend::config::profile_start_seeds(
            &self.config.common.profile_start,
            &self.sesion.conocidos,
            &self.sesion.sembrados,
        )
        .into_iter()
        .filter(|(id, _)| self.huecos.contains_key(id))
        .collect();
        for (id, _) in &siembra {
            self.sesion.sembrados.insert(*id);
        }
        // Un id que el perfil nombra y esta disposición no coloca no tiene
        // dónde abrir. Se DICE, como en el terminal: callarlo es la misma
        // clase de silencio que la clave entera tenía antes de la ADR 0098.
        let colocados: std::collections::BTreeSet<u32> = self.huecos.keys().copied().collect();
        let huerfanos = norte_frontend::config::profile_start_huerfanos(
            &self.config.common.profile_start,
            &colocados,
        );
        if !huerfanos.is_empty() {
            let ids: Vec<String> = huerfanos.iter().map(u32::to_string).collect();
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-profile-start-orphans",
                &[
                    ("n", &huerfanos.len().to_string()),
                    ("ids", &ids.join(", ")),
                ],
            )));
        }
        siembra
    }

    /// Devuelve el panel ACTIVO al directorio que se escribió al arrancar.
    ///
    /// Va DESPUÉS de aplicar la sesión, y ése es todo el arreglo: la sesión
    /// escribe el sitio de todos los huecos, así que un argumento de la línea
    /// de órdenes solo puede ganar volviendo a ponerlo encima. Un directorio
    /// que alguien acaba de teclear es más específico que dónde cerró ayer —
    /// la misma regla que hace que `--layout` gane a `[ui] layout`.
    ///
    /// Solo el activo: el otro panel se queda donde la sesión lo dejó. Y solo
    /// el SITIO — el orden y los ocultos son preferencias, y no se tocan.
    pub(super) fn fijar_dir_pedido(&mut self) {
        let Some(dir) = self.dir_pedido.take() else {
            return;
        };
        let activo = self.activo();
        if let Some(hueco) = self.huecos.get_mut(&activo) {
            hueco.pane.begin_loading(dir);
        }
    }

    /// Coloca cada hueco donde la sesión dice que estaba.
    pub(super) fn aplicar_sesion(&mut self, body: &norte_frontend::session::SessionBody) {
        for (id, hueco) in &mut self.huecos {
            let Some(estado) = body.slots.get(id) else {
                continue;
            };
            hueco.pane.begin_loading(estado.path.clone());
            // El orden y los ocultos se ESCRIBÍAN en la sesión y no los leía
            // nadie: la ventana se acordaba de dónde estabas y olvidaba cómo
            // lo estabas mirando, así que ordenar por tamaño o apartar los
            // dotfiles duraba hasta cerrar.
            hueco.pane.set_sort(estado.sort);
            hueco.pane.set_show_hidden(estado.show_hidden);
            hueco
                .historial
                .seed(estado.back.clone(), estado.forward.clone());
        }
    }

    /// La pantalla de AHORA como cuerpo de sesión.
    ///
    /// Las MARCAS no entran: son una selección de trabajo, no un sitio donde
    /// estabas, y restaurarlas haría que una ventana nueva abriese con media
    /// docena de ficheros elegidos que nadie eligió.
    pub(super) fn capturar_sesion(&self, ahora: u64) -> norte_frontend::session::SessionBody {
        // Se parte de lo LEÍDO y se pisa solo lo propio: los huecos de otro
        // frontend y las disposiciones guardadas siguen ahí.
        //
        // Y `layouts` NO se toca. Hasta esta fase `self.arbol` era constante,
        // así que escribirlo era escribir lo que se había leído; ahora cambia
        // con `layout.pick` y con cada `Ctrl+→`, y el TUI adopta
        // `layouts["default"]` al arrancar. Curiosear un minuto en el selector
        // le cambiaba el arranque al TUI, que es lo que el rustdoc del campo
        // prohíbe por su nombre (ADR 0058 D5) y lo que
        // `aplicar_disposicion_elegida` promete no hacer: «se aplica para ESTA
        // ventana». Era verdad para la configuración y falso para la sesión,
        // que es la que lee el otro frontend.
        let mut body = self.sesion.leida.clone();
        for (id, hueco) in &self.huecos {
            body.slots.insert(
                *id,
                norte_frontend::session::SlotState {
                    path: hueco.pane.dir().clone(),
                    cursor: hueco.pane.cursor() as u64,
                    back: hueco.historial.trail().to_vec(),
                    forward: hueco.historial.forward_trail().to_vec(),
                    sort: hueco.pane.sort(),
                    columns: Vec::new(),
                    show_hidden: hueco.pane.show_hidden(),
                    // El sello de edad, con el reloj de quien escribe. Un
                    // cero sellaba los huecos VIVOS con la época: para la
                    // barrida propia era inocuo —`0.saturating_sub(x)` nunca
                    // pasa de `MAX_AGE_MS`— pero el siguiente escritor con
                    // reloj de verdad los veía con treinta días y se los
                    // llevaba en su primer volcado.
                    touched_ms: ahora,
                },
            );
        }
        body
    }
}
