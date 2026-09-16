//! El panel que pinta un PLUGIN, en la ventana (fase 3).
//!
//! El mismo molde que el preview acoplado, a propósito: un estado por hueco,
//! una petición viva con su testigo, y una respuesta que llega con otro
//! testigo se tira. Lo que cambia es qué se pide —un marco, no un fichero— y
//! que aquí hay algo que CONSERVAR entre repintados: el estado opaco del
//! guest, que es lo único que sobrevive (el permiso de leer se acuña por
//! llamada, en el core).
//!
//! Lo que el guest describe se convierte con
//! [`norte_frontend::frame::StyledFrame::de_wire`], que es la misma puerta que
//! usa el terminal: enmascara el texto de cada tramo y estrecha su rol. Una
//! conversión escrita aquí a mano reabriría el agujero que esa función cerró.

#[allow(clippy::wildcard_imports)]
use super::*;

/// Lo que hace distinto un repintado de otro.
///
/// Lleva el KIND y no solo la geometría: un `SlotId` se reutiliza —los presets
/// traen ids pequeños y fijos—, así que sin él el panel de otro plugin
/// heredaba el marco y el estado del primero.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Firma {
    /// Qué panel es: `plugin:<id>:<kind>`.
    kind: String,
    /// El directorio que mira.
    dir: VPath,
    /// Ancho útil, sin el marco.
    cols: u32,
    /// Alto útil, sin el marco.
    rows: u32,
    /// El nombre bajo el cursor del listado al que sigue.
    cursor: Option<String>,
}

/// Lo que un hueco de panel tiene AHORA y lo que está pidiendo.
#[derive(Default)]
pub(super) struct EstadoPanel {
    /// De QUÉ panel es lo que hay guardado aquí.
    ///
    /// Un `SlotId` se reutiliza —`poner_arbol` cambia el árbol entero y
    /// conserva los ids, y los presets y las plantillas traen ids pequeños y
    /// fijos—, así que el hueco 3 puede pasar de un plugin a otro al cambiar
    /// de disposición o al restaurar la sesión. Sin esto, lo de A seguía aquí
    /// para B: su marco —con sus zonas pulsables— pintado bajo el título de B
    /// hasta que llegara el primero suyo, y su ESTADO OPACO entregado a B en
    /// la primera petición. El blob no lo mira nadie en esta casa, pero es de
    /// A, y el consentimiento del lector fue plugin a plugin.
    kind: Option<String>,
    /// El último marco que llegó. Se conserva mientras se pide el siguiente:
    /// un plugin lento deja la foto de antes, no un hueco que parpadea.
    frame: Option<norte_frontend::frame::StyledFrame>,
    /// El estado opaco del guest, tal cual. Este proceso no lo mira.
    state: Option<Vec<u8>>,
    /// La firma de lo que se está enseñando, o de lo que se intentó y volvió
    /// sin marco. Las dos cosas en un campo porque las dos responden a la
    /// misma pregunta: ¿hace falta pedir esto? Sin anotar el intento vacío, un
    /// panel cuyo plugin ya no está se repide en cada mensaje del actor.
    firma: Option<Firma>,
    /// La petición en vuelo, con su testigo.
    en_vuelo: Option<(RequestToken, Firma)>,
}

impl Estado {
    /// Los huecos de panel de plugin COLOCADOS, con su kind y su tamaño.
    ///
    /// Del reparto y no del árbol, como el preview: un hueco detrás de una
    /// pestaña existe, pero no se está viendo, y lo que no se ve no pide.
    fn huecos_de_panel(&self) -> Vec<(u32, String, u16, u16)> {
        self.reparto
            .placements
            .iter()
            .filter_map(|(slot, r)| {
                let kind = kind_de(&self.arbol, *slot)?;
                if !kind.as_str().starts_with("plugin:") {
                    return None;
                }
                // Y que el kind esté DECLARADO por un plugin consentido. El
                // prefijo lo escribe quien edite una disposición, y sin esta
                // puerta un `plugin:loquesea:loquesea` en un fichero bastaba
                // para que el host le pasara al resolutor el directorio que el
                // lector está mirando.
                self.kinds.get(&kind)?;
                let SlotId(id) = *slot;
                Some((id, kind.as_str().to_owned(), r.width, r.height))
            })
            .collect()
    }

    /// Qué debería estar enseñando el panel del hueco `slot`.
    ///
    /// El vínculo se resuelve con el motor compartido, igual que el preview y
    /// la hoja: un hueco seguido que muere degrada al rol `active`.
    fn firma_de_panel(&self, slot: SlotId, kind: &str, ancho: u16, alto: u16) -> Firma {
        let mut diags = Vec::new();
        let seguido =
            norte_frontend::layout::resolve_follow(&self.arbol, slot, &self.roles, &mut diags)
                .or_else(|| self.roles.get(norte_frontend::layout::RoleId::Active));
        let hueco = seguido
            .and_then(|SlotId(s)| self.huecos.get(&s))
            .or_else(|| self.huecos.get(&self.activo()));
        Firma {
            kind: kind.to_owned(),
            dir: hueco.map_or_else(|| self.hueco().pane.dir().clone(), |h| h.pane.dir().clone()),
            // Sin el marco: el guest describe lo de DENTRO.
            cols: u32::from(ancho.saturating_sub(2)),
            rows: u32::from(alto.saturating_sub(2)),
            // `cursor_entry` y no `selected`, por lo mismo que el preview: el
            // panel habla de lo que hay BAJO el cursor. Y por `display_name`,
            // que es lo que esta casa pintaría.
            cursor: hueco.and_then(|h| h.pane.cursor_entry()).and_then(|e| {
                e.path
                    .file_name()
                    .map(|n| norte_frontend::display_name(n.as_bytes()).0)
            }),
        }
    }

    /// Pide el marco de cada panel de plugin colocado que lo necesite.
    ///
    /// Se llama después de cada mensaje del actor, como el preview: el cursor
    /// lo mueve cualquier cosa, y el guest recibe la fila que hay bajo él.
    /// Una petición viva por hueco; mientras hay una, no se empieza otra —
    /// soltar la respuesta no cancela el trabajo, que ya está instanciando
    /// wasm.
    pub(super) fn sondear_paneles(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // El caso NORMAL —ni un panel de plugin colocado y nada guardado— sale
        // sin tocar el árbol: esto corre después de cada mensaje del actor, y
        // recorrer los huecos para descubrir que no hay ninguno se paga en
        // cada tecla de cada sesión que no usa plugins.
        let huecos = self.huecos_de_panel();
        if huecos.is_empty() && self.paneles.is_empty() {
            return Vec::new();
        }
        // Un hueco que ya no existe no guarda nada. Importa más que en el
        // preview: lo que guarda un panel es el estado OPACO de su guest, y un
        // `SlotId` reutilizado se lo entregaría al plugin siguiente.
        let vivos: Vec<u32> = self
            .arbol
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| id)
            .collect();
        self.paneles.retain(|id, _| vivos.contains(id));

        for (id, kind, ancho, alto) in huecos {
            let firma = self.firma_de_panel(SlotId(id), &kind, ancho, alto);
            let est = self.paneles.entry(id).or_default();
            // El hueco cambió de panel: lo que había era de OTRO plugin y no
            // se hereda —ni el marco, ni el estado opaco—.
            if est.kind.as_deref() != Some(kind.as_str()) {
                *est = EstadoPanel {
                    kind: Some(kind.clone()),
                    ..EstadoPanel::default()
                };
            }
            if est.firma.as_ref() == Some(&firma) || est.en_vuelo.is_some() {
                continue;
            }
            // El kind se parte ANTES de marcar nada en vuelo. Al revés, un
            // kind con el prefijo y sin la segunda mitad —`plugin:git`, que
            // un fichero de disposición puede nombrar— dejaba el hueco con
            // una petición en vuelo que no existía: como una viva impide
            // empezar otra, ese panel no volvía a pedir nunca.
            let Some((plugin_id, panel_kind)) = partes(&kind) else {
                continue;
            };
            self.token += 1;
            let token = RequestToken(self.token);
            let state = est.state.clone();
            self.paneles.entry(id).or_default().en_vuelo = Some((token, firma.clone()));
            let params = norte_proto::methods::PluginPanelRenderParams {
                plugin_id: plugin_id.to_owned(),
                kind: panel_kind.to_owned(),
                dir: firma.dir.clone(),
                cols: firma.cols,
                rows: firma.rows,
                lang: norte_frontend::frame::lang_code().to_owned(),
                cursor_name: firma.cursor.clone(),
                state,
                // Un repintado por cambio de contexto es el evento NEUTRO. Que
                // el guest reciba el clic y el comando es lo que falta.
                event: norte_proto::methods::PanelEvent::Refresh,
            };
            let backend = Arc::clone(backend);
            let buzon = buzon.clone();
            tokio::spawn(async move {
                let res =
                    match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_panel_render(params))
                        .await
                    {
                        Ok(r) => r,
                        Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
                    };
                let _ = buzon
                    .send(Mensaje::PanelContenido(Box::new((id, token, res))))
                    .await;
            });
        }
        Vec::new()
    }

    /// El marco de un panel aterriza: se enseña si el testigo es el de la
    /// última petición de ESE hueco, y se tira si no.
    pub(super) fn aterrizar_panel(
        &mut self,
        slot: u32,
        token: RequestToken,
        res: Result<Option<norte_proto::methods::PanelFrame>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let est = self.paneles.get_mut(&slot)?;
        // El testigo se compara por REFERENCIA y la firma se MUEVE: clonarla
        // era clonar una ruta y dos cadenas en cada aterrizaje, y la vieja no
        // hace falta para nada más.
        if est.en_vuelo.as_ref().map(|(t, _)| *t) != Some(token) {
            return None;
        }
        let (_, firma) = est.en_vuelo.take()?;
        // El intento queda anotado pase lo que pase: sin esto, un panel sin
        // plugin que lo pinte se repide tras cada mensaje del actor.
        est.firma = Some(firma.clone());
        let Ok(Some(marco)) = res else {
            return None;
        };
        // Y que lo firme quien se pidió: el marco dice de qué plugin es.
        if partes(&firma.kind).map(|(id, _)| id) != Some(marco.plugin_id.as_str()) {
            return None;
        }
        // Un marco IDÉNTICO no mueve la pantalla, y una foto entera por cada
        // movimiento del cursor sí pesa: un panel que describe el directorio
        // —no la fila— devuelve lo mismo una y otra vez.
        let nuevo = norte_frontend::frame::StyledFrame::de_wire(&marco);
        let cambia = est.frame.as_ref() != Some(&nuevo);
        est.frame = Some(nuevo);
        est.state = marco.state;
        if !cambia {
            return None;
        }
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// El panel de un hueco, en la forma del puente.
    ///
    /// Sin marco todavía —la primera petición en vuelo, o el plugin falló— el
    /// hueco viaja con sus líneas vacías: el renderer pinta el borde y el
    /// título, que es lo que dice que el panel está y de quién es.
    pub(super) fn vista_de_panel(&self, id: u32) -> crate::dto::PanelSlotView {
        // El kind sale del ÁRBOL —un fichero de disposición, `--layout` o la
        // sesión—, no del registro: `validate` comprueba la forma y conserva
        // los kinds que este host no conoce (ADR 0059), así que nadie le ha
        // exigido un alfabeto. Es texto que puede traer controles y acaba en
        // el DOM y en un `aria-label`, igual que el nombre de un kind
        // desconocido, y se trata igual.
        let kind = kind_de(&self.arbol, SlotId(id)).map_or_else(String::new, |k| {
            partes(k.as_str()).map_or_else(String::new, |(_, panel)| {
                clamp_display(norte_frontend::display_name(panel.as_bytes()).0)
            })
        });
        let est = self.paneles.get(&id);
        let lines = est
            .and_then(|e| e.frame.as_ref())
            .map(|f| {
                f.lines
                    .iter()
                    .map(|linea| linea.iter().map(super::views::span_view).collect())
                    .collect()
            })
            .unwrap_or_default();
        let hits = est
            .and_then(|e| e.frame.as_ref())
            .map(|f| {
                f.hits
                    .iter()
                    .map(|h| crate::dto::HitView {
                        row: h.row,
                        col: h.col,
                        width: h.width,
                    })
                    .collect()
            })
            .unwrap_or_default();
        crate::dto::PanelSlotView {
            slot_id: id,
            title: kind,
            lines,
            hits,
        }
    }

    /// Se pulsó una celda de un panel de plugin: se resuelve QUÉ zona era y se
    /// ejecuta su comando.
    ///
    /// El comando no viaja por el cable: lo tiene el marco, que está aquí. Y
    /// se filtra con [`norte_frontend::frame::zona_puede`], la misma lista que
    /// aplica el terminal — el plugin elige la etiqueta Y el comando, y nada
    /// los ata, así que sin filtro una zona que pone «Actualizar» podría
    /// nombrar algo que copia ficheros. El consentimiento fue para pintar.
    ///
    /// Lo que pasa el filtro va por el MISMO camino que el menú y la barra de
    /// paneles (`efecto_de` + `aplicar_efecto`): una segunda puerta al
    /// catálogo sería un segundo despachador.
    pub(super) fn clic_en_panel(
        &mut self,
        slot: u32,
        row: u16,
        col: u16,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Un hueco ESCONDIDO —detrás de una pestaña— conserva su marco, así
        // que sus zonas seguirían resolviéndose aunque nadie las vea. El
        // renderer no pinta lo escondido, luego un clic ahí no viene de una
        // persona: la respuesta es la misma que da el preview, «pide foto».
        // En el terminal esto no hace falta porque la cuenta parte del
        // rectángulo pintado; aquí la celda llega por el cable.
        if self.oculto(slot) {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let comando = self
            .paneles
            .get(&slot)
            .and_then(|e| e.frame.as_ref())
            .and_then(|f| f.hit_at(row, col))
            .map(|h| h.command.clone())
            .filter(|c| norte_frontend::frame::zona_puede(c));
        let Some(comando) = comando else {
            // Una celda sin zona, o una zona que nombra algo fuera de su
            // alcance: no pasa nada, y no es un error del lector.
            return (self.aplicada(), Vec::new());
        };
        match crate::commands::efecto_de(&comando, 1) {
            Some(efecto) => self.aplicar_efecto(efecto, backend, buzon),
            None => self.no_implementado(&comando),
        }
    }
}

/// `plugin:<id>:<kind>` partido en las dos mitades que necesita la RPC.
///
/// El separador es el PRIMER `:` tras el prefijo, y es inequívoco porque el
/// alfabeto que valida `KindRegistry::insert_panels` no deja pasar dos puntos
/// ni en el id ni en el kind.
fn partes(kind: &str) -> Option<(&str, &str)> {
    kind.strip_prefix("plugin:")?.split_once(':')
}
