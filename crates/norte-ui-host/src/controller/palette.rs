//! La paleta de comandos y las filas que ponen las extensiones.
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
    /// Abre la paleta de comandos.
    ///
    /// Las filas se construyen AQUÍ, al abrir, y se congelan: es lo que el
    /// modelo compartido espera (pliega el haystack de cada fila una vez, no
    /// por tecla).
    pub(super) fn abrir_paleta(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.paleta = Some(norte_frontend::palette_state::Palette::new(
            self.filas_de_paleta(),
        ));
        self.pedir_filas_de_plugin(backend, buzon);
        let cambio = ViewChange::Palette {
            palette: self.vista_paleta(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Pide el catálogo para las filas de PLUGIN de la paleta.
    ///
    /// No se espera: la paleta se pinta ya con los comandos propios y las de
    /// plugin se unen cuando el daemon conteste. Congelar la ventana hasta
    /// entonces sería pagar el viaje aunque no haya ninguna extensión.
    pub(super) fn pedir_filas_de_plugin(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // En SOLO LECTURA no se piden: lo que hace un comando de plugin lo
        // decide el plugin, y esta ventana no lo va a lanzar. Es la misma
        // regla que `filas_de_paleta` ya aplica a los comandos propios —
        // ofrecer lo que se va a rehusar es prometer algo que no se hará.
        if self.efectos == crate::commands::Efectos::SoloLectura {
            return;
        }
        self.gen_paleta += 1;
        let apertura = self.gen_paleta;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_list()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::PluginsDePaleta(
                    apertura, res,
                ))))
                .await;
        });
    }

    /// Las filas de plugin llegaron: se UNEN a la paleta abierta.
    ///
    /// Conservando lo tecleado (`extend_rows`): reconstruirla perdería la
    /// query, y perder lo que alguien acaba de escribir por unas filas que
    /// llegan tarde es peor que no tenerlas.
    ///
    /// Un fallo NO tumba la paleta ni se anuncia: los comandos propios
    /// siguen ahí, que es el mismo criterio que el TUI («un daemon caído
    /// degrada la paleta, no la tumba»).
    pub(super) fn aplicar_filas_de_plugin(
        &mut self,
        apertura: u64,
        res: Result<norte_proto::methods::PluginListResult, Error>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if apertura != self.gen_paleta {
            return Vec::new();
        }
        let Ok(lista) = res else {
            return Vec::new();
        };
        let Some(p) = self.paleta.as_mut() else {
            return Vec::new();
        };
        // El mismo filtro y el mismo tope que el GESTOR aplica al catálogo:
        // un daemon hostil puede anunciar los plugins que quiera, y por aquí
        // cada uno además aporta una fila por comando. Sin el `is_valid_
        // plugin_id`, un id con `:` dentro rompe la clave que `plugin_rows`
        // compone y `parse_plugin_key` deshace, que es justo el contrato que
        // las dos comparten.
        let catalogo: Vec<_> = lista
            .plugins
            .iter()
            .filter(|p| norte_proto::methods::is_valid_plugin_id(&p.id))
            .take(crate::extensions::MAX_EXTENSIONES)
            .cloned()
            .collect();
        // El modelo COMPARTIDO decide qué se ofrece: solo aprobadas y
        // encendidas —la misma puerta que `plugin.run_command` exige por su
        // cuenta—, en orden de manifiesto, y con el prefijo que impide que
        // un comando de tercero se disfrace de uno propio.
        let mut filas = norte_frontend::palette::plugin_rows_in(&catalogo, self.lang);
        // Y un tope de FILAS: el manifiesto no acota cuántos comandos declara
        // un plugin, así que uno aprobado con doscientos mil convertía cada
        // `ctrl+p` en un mensaje de cientos de megas.
        filas.truncate(crate::bridge::MAX_ROWS_PER_BATCH);
        if filas.is_empty() {
            return Vec::new();
        }
        p.extend_rows(filas);
        self.rotulos_plugin = catalogo
            .iter()
            .map(|p| {
                let comandos = p
                    .commands
                    .iter()
                    .map(|c| (c.id.clone(), crate::extensions::texto_de_tercero(&c.title)))
                    .collect();
                (
                    p.id.clone(),
                    (crate::extensions::texto_de_tercero(&p.name), comandos),
                )
            })
            .collect();
        let cambio = ViewChange::Palette {
            palette: self.vista_paleta(),
        };
        vec![self.parche(vec![cambio])]
    }

    /// Ejecuta un comando de extensión y enseña su salida.
    ///
    /// La autorización es del SERVIDOR: `plugin.run_command` resuelve el
    /// comando contra el catálogo y exige aprobada + encendida por su
    /// cuenta. Lo que la fila comprueba de este lado es coherencia con lo
    /// que el lector está mirando, jamás el permiso.
    pub(super) fn ejecutar_de_plugin(
        &mut self,
        clave: &str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some((id, comando)) = norte_frontend::palette::parse_plugin_key(clave) else {
            return self.no_implementado(clave);
        };
        if self.efectos == crate::commands::Efectos::SoloLectura {
            // Lo que hace un comando de plugin lo decide el plugin: puede
            // escribir. Una ventana sin efectos no lo lanza.
            return Self::no_muta();
        }
        self.gen_salida += 1;
        let apertura = self.gen_salida;
        // Los dos rótulos se resuelven AHORA, con el catálogo que la paleta
        // usó: la respuesta puede tardar, y buscarlos al volver es buscarlos
        // en un catálogo que ya no es el mismo.
        let (rotulo, titulo) = self.rotulos_de_comando(id, comando);
        let backend2 = Arc::clone(backend);
        let buzon2 = buzon.clone();
        let (id2, comando2) = (id.to_owned(), comando.to_owned());
        let id3 = id2.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(
                PLAZO_COMANDO,
                backend2.plugin_run_command(id2, comando2, String::new()),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon2
                .send(Mensaje::Fondo(Box::new(Fondo::SalidaDeComando(
                    apertura,
                    Box::new(SalidaPedida {
                        id: id3,
                        plugin: rotulo,
                        comando: titulo,
                        res,
                    }),
                ))))
                .await;
        });
        (self.aplicada(), self.decir("host-plugin-running"))
    }

    /// Cómo se llaman, para el panel de salida: el nombre de la extensión y
    /// el título del comando, ya enmascarados. Si el gestor no está abierto
    /// se cae al id, que es lo único que este proceso asigna.
    pub(super) fn rotulos_de_comando(
        &self,
        id: &str,
        comando: &str,
    ) -> (crate::extensions::Texto, crate::extensions::Texto) {
        let del_catalogo = self.rotulos_plugin.get(id);
        let nombre = del_catalogo
            .map(|(n, _)| n.clone())
            .or_else(|| Some(self.extensiones.as_ref()?.concesion(id)?.nombre))
            // Sin rótulo conocido se cae al id —que el core SÍ valida— pero
            // por la misma puerta que todo lo demás: quien lo manda es el
            // daemon y no este proceso.
            .unwrap_or_else(|| crate::extensions::texto_de_tercero(id));
        let titulo = del_catalogo
            .and_then(|(_, cs)| cs.get(comando).cloned())
            .or_else(|| {
                self.extensiones
                    .as_ref()?
                    .comandos_de_id(id)
                    .iter()
                    .find(|c| c.id == comando)
                    .map(|c| (c.title.clone(), c.hostile))
            })
            // El id de un COMANDO no se pinta: el manifiesto no le valida
            // charset. Sin título conocido, la línea se queda sin él.
            .unwrap_or_default();
        (nombre, titulo)
    }
}
