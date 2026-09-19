//! «Ir a cualquier sitio» en la ventana (#357, fase 6 del programa WOW).
//!
//! El modelo —secciones, orden, filtrado, cursor—, cómo se construye cada
//! clase de fila y qué significa confirmarla son de `norte_frontend::goto`,
//! los mismos que usa la TUI. Aquí vive lo que sólo esta ventana sabe: de qué
//! listas suyas salen las filas, cómo le llegan las conexiones (las da el
//! daemon) y cómo se pregunta al índice sin congelar la pantalla.
//!
//! Parte de `controller`: son métodos de `Estado`. El único escritor sigue
//! siendo el actor.

#[allow(clippy::wildcard_imports)]
use super::*;

use norte_frontend::goto::{
    Accion, FixedSource, Goto, GotoLine, GotoRow, GotoSource, MINIMO_PARA_EL_INDICE, RutaSource,
    SECCION_COMANDOS, SECCION_CONEXIONES, SECCION_FAVORITOS, SECCION_HISTORIA, SECCION_INDICE,
    SECCION_POPULARES, TOPE_DEL_INDICE, TRAIDAS_POR_LISTA,
};

impl Estado {
    /// Abre «ir a».
    ///
    /// Las filas se toman como una FOTO al abrir, igual que la paleta: una
    /// lista que cambia bajo el cursor mientras se lee es cómo un Enter acaba
    /// en otro sitio. Las dos excepciones llegan tarde y por el buzón: las
    /// CONEXIONES (las sabe el daemon, no este proceso) y lo que encuentre el
    /// índice (una pregunta por consulta).
    pub(super) fn abrir_ir_a(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let fuentes = self.fuentes_de_ir_a();
        self.ir_a = Some(Goto::new(fuentes));
        self.gen_ir_a += 1;
        let apertura = self.gen_ir_a;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.connections()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::ConexionesDeIrA(
                    apertura, res,
                ))))
                .await;
        });
        let cambio = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Las fuentes SÍNCRONAS: lo que esta ventana ya tiene en memoria. Las
    /// conexiones empiezan vacías y se rellenan cuando el daemon contesta.
    fn fuentes_de_ir_a(&self) -> Vec<Box<dyn GotoSource + Send>> {
        let hueco = self.hueco();
        let enc = hueco.pane.name_encoding();
        let actual = hueco.pane.dir().clone();
        let mut out: Vec<Box<dyn GotoSource + Send>> = Vec::new();
        out.push(Box::new(RutaSource::new(norte_i18n::t_in(
            self.lang,
            "goto-path-desc",
        ))));
        // La historia es la ÚNICA sección que lleva la reinterpretación del
        // panel, porque es la única cuyas rutas son de ese panel.
        let historia: Vec<GotoRow> =
            norte_frontend::history::history_rows(&hueco.historial, &actual, "", enc)
                .into_iter()
                .filter(|r| r.mark != norte_frontend::history::HistoryMark::Current)
                .take(TRAIDAS_POR_LISTA)
                .map(|r| norte_frontend::goto::fila_ruta(SECCION_HISTORIA.id, None, &r.path, enc))
                .collect();
        out.push(Box::new(FixedSource::new(SECCION_HISTORIA, historia)));
        let populares: Vec<GotoRow> =
            norte_frontend::history::popular_rows(&self.popular, &actual, "")
                .into_iter()
                .take(TRAIDAS_POR_LISTA)
                .map(|r| norte_frontend::goto::fila_ruta(SECCION_POPULARES.id, None, &r.path, None))
                .collect();
        out.push(Box::new(FixedSource::new(SECCION_POPULARES, populares)));
        // Un favorito cuyo destino no parsea NO se ofrece: la lista de sitios
        // ya lo enseña con su error.
        let favoritos: Vec<GotoRow> = self
            .config
            .common
            .hotlist
            .iter()
            .filter_map(|h| {
                h.target.as_ref().ok().map(|p| {
                    norte_frontend::goto::fila_ruta(SECCION_FAVORITOS.id, Some(&h.name), p, None)
                })
            })
            .collect();
        out.push(Box::new(FixedSource::new(SECCION_FAVORITOS, favoritos)));
        out.push(Box::new(FixedSource::new(SECCION_CONEXIONES, Vec::new())));
        // Los comandos, los MISMOS que la paleta de esta ventana ofrece: la
        // paleta ya resuelve cuáles implementa este host y con qué efectos.
        let comandos = norte_frontend::goto::filas_de_comandos(self.filas_de_paleta());
        out.push(Box::new(
            FixedSource::new(SECCION_COMANDOS, comandos).solo_con_consulta(),
        ));
        out
    }

    /// Llegaron las conexiones: se ponen en su sección, SIN mover el cursor
    /// (lo garantiza `reemplazar_seccion`). Un fallo deja la sección vacía y
    /// no se anuncia: es una sección menos, no una pantalla que no abre.
    pub(super) fn conexiones_de_ir_a(
        &mut self,
        apertura: u64,
        res: Result<Vec<norte_proto::methods::ConnectionEntry>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if apertura != self.gen_ir_a {
            return None;
        }
        let goto = self.ir_a.as_mut()?;
        let filas: Vec<GotoRow> = res
            .ok()?
            .iter()
            .take(crate::bridge::MAX_ROWS_PER_BATCH)
            .map(|c| norte_frontend::goto::fila_conexion(&c.name, &c.url))
            .collect();
        goto.reemplazar_seccion(SECCION_CONEXIONES, filas, false);
        let cambio = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// Pregunta al índice por lo que hay escrito, si vale la pena.
    ///
    /// Relanzar ABORTA la pregunta anterior. Por debajo de
    /// [`MINIMO_PARA_EL_INDICE`] no se pregunta y se VACÍA la sección: dejar
    /// ahí lo que contestó a una consulta más larga es enseñar una respuesta
    /// a una pregunta que ya no se hizo.
    fn pedir_al_indice_de_ir_a(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if let Some(vieja) = self.ir_a_indice.take() {
            vieja.abort();
        }
        let Some(goto) = self.ir_a.as_mut() else {
            return;
        };
        let consulta = goto.query().to_owned();
        if consulta.chars().count() < MINIMO_PARA_EL_INDICE {
            goto.reemplazar_seccion(SECCION_INDICE, Vec::new(), true);
            return;
        }
        let apertura = self.gen_ir_a;
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        self.ir_a_indice = Some(tokio::spawn(async move {
            let res = match tokio::time::timeout(
                PLAZO_PLUGINS,
                backend.semantic_search(consulta.clone(), TOPE_DEL_INDICE),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::IndiceDeIrA(
                    apertura, consulta, res,
                ))))
                .await;
        }));
    }

    /// Mete la respuesta del índice en su sección.
    ///
    /// No abre nada ni escribe en la barra (un índice apagado contesta
    /// `Unsupported`, y eso es lo normal de quien no lo tiene); no se fía del
    /// tamaño de la respuesta (el MISMO `validate_semantic_hits` que la
    /// búsqueda semántica); y no toca nada si la pantalla ya se cerró o lo
    /// escrito cambió mientras el índice pensaba.
    pub(super) fn indice_de_ir_a(
        &mut self,
        apertura: u64,
        consulta: &str,
        res: Result<Vec<norte_proto::methods::SemanticHit>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if apertura != self.gen_ir_a {
            return None;
        }
        let goto = self.ir_a.as_mut()?;
        if goto.query() != consulta {
            return None;
        }
        let hits = norte_frontend::validate_semantic_hits(res.ok()?)?;
        goto.reemplazar_seccion(
            SECCION_INDICE,
            norte_frontend::goto::filas_del_indice(&hits),
            true,
        );
        let cambio = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        Some(self.parche(vec![cambio]))
    }

    /// Cierra «ir a» y abandona la pregunta al índice: lo que venga detrás
    /// manda, y una respuesta tardía ya no tiene dónde caer.
    fn cerrar_ir_a(&mut self) {
        self.ir_a = None;
        self.gen_ir_a += 1;
        if let Some(vieja) = self.ir_a_indice.take() {
            vieja.abort();
        }
    }

    /// Las teclas mientras «ir a» está abierto.
    ///
    /// FIJAS, como las de la paleta y por lo mismo: no hay verbos `dialog.*`
    /// para teclear un carácter o correr la selección. `Escape` cierra,
    /// `Enter` va, las flechas mueven y lo demás teclea.
    pub(super) fn tecla_en_ir_a(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(g) = self.ir_a.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match k.key.as_str() {
            "Escape" | "esc" => self.cerrar_ir_a(),
            "Enter" | "enter" => {
                let clave = g.selected().map(|r| r.key.clone());
                return self.confirmar_ir_a(clave, backend, buzon);
            }
            "ArrowDown" | "down" => g.down(),
            "ArrowUp" | "up" => g.up(),
            "Backspace" | "backspace" => {
                g.backspace();
                self.pedir_al_indice_de_ir_a(backend, buzon);
            }
            otra => {
                // Una tecla de TEXTO es un punto de código, no una unidad
                // UTF-16 ni un nombre de tecla: `ArrowLeft` no se teclea.
                let mut chars = otra.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if !k.ctrl && !k.alt && !k.meta => {
                        g.push_char(c);
                        self.pedir_al_indice_de_ir_a(backend, buzon);
                    }
                    _ => return (self.aplicada(), Vec::new()),
                }
            }
        }
        let cambio = ViewChange::Goto {
            goto: self.vista_ir_a(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Confirma la fila elegida: cierra la pantalla ANTES de actuar —el cierre
    /// en su propio parche, como la paleta, para que lo que abra el efecto no
    /// quede debajo— y hace lo que decide `norte_frontend::goto::accion`.
    fn confirmar_ir_a(
        &mut self,
        clave: Option<String>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.cerrar_ir_a();
        let cierre = self.parche(vec![ViewChange::Goto { goto: None }]);
        let Some(clave) = clave else {
            return (self.aplicada(), vec![cierre]);
        };
        let (ack, mut resto) = match norte_frontend::goto::accion(&clave) {
            Accion::Ir(dir) => (
                self.aplicada(),
                self.navegar(&dir, Trail::Record, backend, buzon),
            ),
            // Por el MISMO camino que una tecla: «ir a» es otra puerta al
            // catálogo, no un segundo despachador.
            Accion::Comando(cmd) => match efecto_de(&cmd, 1) {
                Some(efecto) => self.aplicar_efecto(efecto, backend, buzon),
                None => self.no_implementado(&cmd),
            },
            Accion::Nada(motivo) => (self.aplicada(), self.decir(motivo)),
        };
        let mut envios = vec![cierre];
        envios.append(&mut resto);
        (ack, envios)
    }

    /// La proyección de «ir a».
    pub(super) fn vista_ir_a(&self) -> Option<crate::dto::GotoView> {
        let g = self.ir_a.as_ref()?;
        let filas = g.rows();
        let lines: Vec<crate::dto::GotoLineView> = g
            .lines()
            .iter()
            .filter_map(|l| match l {
                GotoLine::Header(s) => Some(crate::dto::GotoLineView::Header {
                    title: norte_i18n::t_in(self.lang, s.title_key),
                }),
                GotoLine::Row(i) => filas.get(*i).map(|r| crate::dto::GotoLineView::Row {
                    text: clamp_display(r.text.clone()),
                    desc: clamp_display(r.desc.clone()),
                    hostile: r.hostile,
                }),
            })
            .collect();
        Some(crate::dto::GotoView {
            query: clamp_display(g.query().to_owned()),
            cursor: (!lines.is_empty() && !g.is_empty()).then_some(g.cursor() as u64),
            lines,
            empty: norte_i18n::t_in(self.lang, "goto-empty"),
        })
    }
}
