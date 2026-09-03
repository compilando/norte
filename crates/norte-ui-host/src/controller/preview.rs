//! El visor ACOPLADO (#291): qué debería estar enseñando cada hueco de
//! preview, y qué enseña.
//!
//! La misma decisión que `norte-tui/src/preview.rs`, con el mismo reparto de
//! responsabilidades (ADR 0077): un hueco de preview que el reparto no
//! colocó —cerrado, detrás de una pestaña, colapsado por falta de sitio— no
//! produce objetivo, así que no hay lectura que suspender; un directorio bajo
//! el cursor no se lee; y la respuesta viaja con su HUECO y su testigo, nunca
//! con una posición, para que una que llega tarde no aterrice en quien ocupe
//! ese sitio al llegar.
//!
//! Lo que cambia respecto a la TUI es el «cuándo»: allí se pregunta en cada
//! frame; aquí, después de cada mensaje del actor (`sondear_previews`), que
//! es lo más parecido a un frame que tiene un host que solo habla cuando
//! algo cambia.

// El mismo `impl Estado` partido en trozos, con los imports del padre: ver
// `viewer.rs`.
#[allow(clippy::wildcard_imports)]
use super::*;

/// El kind que ocupa un hueco de visor. El mismo que el visor a pantalla
/// completa: lo que cambia es el vínculo, no lo que hay dentro.
pub(super) const KIND: &str = "viewer";

/// Cuántas filas cruzan para un hueco de preview: el hueco las desplaza
/// solo, así que van enteras hasta el tope del puente.
const PREVIEW_MAX_ROWS: usize = crate::bridge::MAX_ROWS_PER_BATCH;

/// Lo que un hueco de preview tiene AHORA, y lo que está pidiendo.
#[derive(Default)]
pub(super) struct EstadoPreview {
    /// Qué ruta enseña (o intentó enseñar), si alguna.
    shown: Option<VPath>,
    /// El visor con lo leído.
    viewer: Option<norte_frontend::viewer::Viewer>,
    /// La clave Fluent que sustituye al fichero: un directorio, nada bajo
    /// el cursor, un error de lectura.
    note: Option<&'static str>,
    /// La lectura en vuelo, con su testigo: una respuesta con otro testigo
    /// es de un cursor que ya se movió.
    en_vuelo: Option<(RequestToken, VPath)>,
}

/// Qué debería estar enseñando un hueco de preview.
enum Quiere {
    /// Este fichero, que hay que leer.
    Fichero(VPath),
    /// Nada que leer, y esta clave dice por qué.
    Nota(&'static str),
}

impl Estado {
    /// Los huecos de preview COLOCADOS, con su ancho en celdas.
    ///
    /// Del reparto y no del árbol: un hueco detrás de una pestaña existe,
    /// pero no se está viendo, y lo que no se ve no lee.
    fn huecos_de_preview(&self) -> Vec<(u32, u16)> {
        self.reparto
            .placements
            .iter()
            .filter(|(slot, _)| kind_de(&self.arbol, *slot).is_some_and(|k| k.as_str() == KIND))
            .map(|(SlotId(id), r)| (*id, r.width))
            .collect()
    }

    /// Qué debería estar enseñando el hueco `slot`.
    ///
    /// El vínculo se resuelve con el motor compartido, como la hoja de
    /// atributos: un hueco seguido que muere degrada al rol `active`.
    fn quiere_preview(&self, slot: SlotId) -> Quiere {
        let mut diags = Vec::new();
        let seguido =
            norte_frontend::layout::resolve_follow(&self.arbol, slot, &self.roles, &mut diags)
                .or_else(|| self.roles.get(norte_frontend::layout::RoleId::Active));
        let entrada = seguido
            .and_then(|SlotId(s)| self.huecos.get(&s))
            .and_then(|h| h.pane.selected());
        let Some(e) = entrada else {
            return Quiere::Nota("preview-empty");
        };
        match e.kind {
            EntryKind::File => Quiere::Fichero(e.path.clone()),
            EntryKind::Dir => Quiere::Nota("preview-directory"),
            // Un enlace o algo que el provider no clasifica: no se lee a
            // ciegas, porque leer «lo que sea» es justo como un preview
            // automático se convierte en abrir un dispositivo de bloque.
            _ => Quiere::Nota("preview-not-a-file"),
        }
    }

    /// Pone cada hueco de preview colocado a enseñar lo que le toca: una
    /// nota, ya; un fichero, pidiéndolo si no es el que ya enseña ni el que
    /// ya está en vuelo. Devuelve una foto si alguna nota cambió.
    pub(super) fn sondear_previews(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // Un hueco que ya no existe no guarda nada: ni un visor de un fichero
        // que nadie ve, ni una respuesta en vuelo que aterrizaría en él.
        let vivos: Vec<u32> = self
            .arbol
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| id)
            .collect();
        self.previews.retain(|id, _| vivos.contains(id));

        let mut cambio = false;
        for (id, ancho) in self.huecos_de_preview() {
            match self.quiere_preview(SlotId(id)) {
                Quiere::Nota(clave) => {
                    let est = self.previews.entry(id).or_default();
                    if est.note != Some(clave) || est.viewer.is_some() {
                        *est = EstadoPreview {
                            note: Some(clave),
                            ..EstadoPreview::default()
                        };
                        cambio = true;
                    }
                }
                Quiere::Fichero(path) => {
                    let ya = self.previews.get(&id).is_some_and(|est| {
                        est.shown.as_ref() == Some(&path)
                            || est.en_vuelo.as_ref().is_some_and(|(_, p)| *p == path)
                    });
                    if ya {
                        continue;
                    }
                    self.token += 1;
                    let token = RequestToken(self.token);
                    self.previews.entry(id).or_default().en_vuelo = Some((token, path.clone()));
                    // El ancho del HUECO menos su marco, para el previewer
                    // (proto 0.66.0): una imagen encoge a lo que le digan.
                    let columnas = Some(u32::from(ancho.saturating_sub(2).max(1)));
                    let backend = Arc::clone(backend);
                    let buzon = buzon.clone();
                    tokio::spawn(async move {
                        let lectura = backend.read(
                            path.clone(),
                            Some(norte_proto::ByteRange {
                                offset: 0,
                                len: Some(VISOR_CAP + 1),
                            }),
                        );
                        let leido = match tokio::time::timeout(PLAZO_VISOR, lectura).await {
                            Ok(r) => r,
                            Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
                        };
                        // Un previewer que falla, que tarda o que no aplica
                        // NO es un error: se cae a la vista cruda.
                        let preview = match tokio::time::timeout(
                            PLAZO_PLUGINS,
                            backend.plugin_preview_styled(path.clone(), columnas),
                        )
                        .await
                        {
                            Ok(Ok(p)) => p,
                            _ => None,
                        };
                        let _ = buzon
                            .send(Mensaje::PreviewContenido(Box::new((
                                id,
                                (token, path, leido, preview),
                            ))))
                            .await;
                    });
                }
            }
        }
        if cambio {
            let snap = self.snapshot();
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))]
        } else {
            Vec::new()
        }
    }

    /// Lo leído para un hueco de preview aterriza: se enseña si el testigo
    /// es el de la última petición de ESE hueco, y se tira si no.
    pub(super) fn aterrizar_preview(
        &mut self,
        slot: u32,
        token: RequestToken,
        path: VPath,
        leido: Result<Vec<u8>, Error>,
        preview: Option<norte_proto::methods::PluginPreviewStyled>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let est = self.previews.get_mut(&slot)?;
        if est.en_vuelo.as_ref().map(|(t, _)| *t) != Some(token) {
            return None;
        }
        est.en_vuelo = None;
        if let Ok(mut bytes) = leido {
            let cap = usize::try_from(VISOR_CAP).unwrap_or(usize::MAX);
            let truncado = bytes.len() > cap;
            if truncado {
                bytes.truncate(cap);
            }
            est.viewer = Some(match preview {
                Some(p) => norte_frontend::viewer::Viewer::with_plugin_preview_styled(
                    path.clone(),
                    p.plugin_name,
                    &p.lines,
                    p.lossy,
                ),
                None => norte_frontend::viewer::Viewer::new(path.clone(), bytes, truncado),
            });
            est.note = None;
        } else {
            // No se pudo leer: se DICE, en el hueco, en vez de dejar el
            // fichero anterior puesto como si fuera este.
            est.viewer = None;
            est.note = Some("preview-unreadable");
        }
        est.shown = Some(path);
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// La proyección de un hueco de preview.
    pub(super) fn vista_de_preview(&self, slot: u32) -> crate::dto::PreviewSlotView {
        let est = self.previews.get(&slot);
        let viewer = est
            .and_then(|e| e.viewer.as_ref())
            .map(|v| self.vista_de_visor(v, PREVIEW_MAX_ROWS, false));
        let note = if viewer.is_some() {
            String::new()
        } else {
            let clave = est.and_then(|e| e.note).unwrap_or("preview-empty");
            clamp_display(norte_i18n::t_in(self.lang, clave))
        };
        crate::dto::PreviewSlotView {
            slot_id: slot,
            viewer,
            note,
        }
    }
}
