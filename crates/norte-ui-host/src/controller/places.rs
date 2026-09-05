//! La barra de sitios: unidades y favoritos.
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
    /// El foco está en la barra lateral de sitios.
    pub(super) fn sitios_tienen_el_foco(&self) -> bool {
        self.sitios.is_some()
            && self
                .roles
                .get(RoleId::Active)
                .is_some_and(|s| self.hueco_de_sitios() == Some(s))
    }

    /// El movimiento y la activación, con el foco en la barra lateral.
    ///
    /// El vocabulario es el del LISTADO porque es el único mapa que esta
    /// ventana tiene —no hay pantalla `dialog` aquí—, y cada comando
    /// significa en la barra lo que significa en su superficie: bajar baja
    /// por ella, entrar va al sitio, y la tecla de marcar PLIEGA, porque una
    /// barra lateral no tiene nada que marcar y sí dos secciones que abrir y
    /// cerrar.
    ///
    /// La activación necesita el backend, así que se devuelve `None` para
    /// que la trate `aplicar_efecto` por su camino normal; aquí solo se
    /// mueve el cursor.
    pub(super) fn efecto_en_sitios(
        &mut self,
        efecto: Efecto,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        // Lo SUYO, antes de contar filas: por lo mismo que en el panel de
        // procesos, un panel vacío que contesta «aplicado» a todo se traga la
        // tecla con la que se sale de él.
        if !matches!(
            efecto,
            Efecto::Cursor(_) | Efecto::Pagina(_) | Efecto::Extremo { .. }
        ) {
            return None;
        }
        let estado = self.sitios.as_mut()?;
        let filas = estado.rows().len();
        if filas == 0 {
            return Some((self.aplicada(), Vec::new()));
        }
        let total = i64::try_from(filas).unwrap_or(i64::MAX);
        let actual = i64::try_from(estado.cursor().min(filas - 1)).unwrap_or(0);
        let destino = match efecto {
            Efecto::Cursor(n) => actual.saturating_add(n.clamp(-total, total)),
            Efecto::Pagina(n) => {
                actual.saturating_add(n.clamp(-total, total).saturating_mul(total))
            }
            Efecto::Extremo { al_final: false } => 0,
            Efecto::Extremo { al_final: true } => total - 1,
            // Todo lo demás sigue su camino. Entrar y plegar, en concreto,
            // necesitan el backend —una navegación, o volver a pedir los
            // volúmenes—, así que los atiende quien sí lo tiene.
            _ => return None,
        };
        estado.set_cursor(usize::try_from(destino.max(0)).unwrap_or(0).min(filas - 1));
        let snap = self.snapshot();
        Some((
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        ))
    }

    /// Alimenta la barra lateral con los favoritos de la configuración.
    ///
    /// De la config con la que ARRANCÓ la ventana, que es la que está usando.
    /// Un favorito cuya ruta no parsea se conserva con su clave de error: la
    /// hotlist es data del usuario, no configuración estructural, y uno que
    /// desaparece en silencio es un fallo que nadie puede ver.
    pub(super) fn sembrar_sitios(&mut self) {
        let items: Vec<(String, Result<VPath, String>)> = self
            .config
            .common
            .hotlist
            .iter()
            .map(|h| (h.name.clone(), h.target.clone()))
            .collect();
        let estado = self
            .sitios
            .get_or_insert_with(norte_frontend::places::PlacesState::new);
        estado.set_favorites(&items);
        self.gen_sitios += 1;
    }

    /// Pide los volúmenes para la barra lateral.
    ///
    /// Lo llaman el arranque y desplegar la sección de unidades. Y nadie más:
    /// una barra lateral con reloj rompería la regla de suspensión del ADR
    /// 0058 desde el primer frame, y `host.volumes` no es gratis — monta y
    /// consulta espacio en cada filesystem.
    pub(super) fn pedir_sitios(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if self.hueco_de_sitios().is_none() {
            return;
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            let res = match tokio::time::timeout(PLAZO_PLUGINS, backend.volumes()).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::SitiosVolumenes(res))))
                .await;
        });
    }

    /// Los volúmenes llegaron a la barra lateral.
    ///
    /// Un fallo NO vacía lo que hubiera: lo que se veía sigue siendo lo
    /// último que el host dijo.
    pub(super) fn aplicar_sitios(
        &mut self,
        res: Result<Vec<norte_proto::methods::Volume>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let Ok(vols) = res else {
            return None;
        };
        // Solo si la barra EXISTE en esta disposición. `get_or_insert_with`
        // creaba un estado —sin favoritos, porque `sembrar_sitios` no corre—
        // para una respuesta rezagada de una disposición que ya no tiene
        // hueco `places`, y luego mandaba una foto entera para nada.
        // `pedir_sitios` ya se guarda igual.
        self.hueco_de_sitios()?;
        self.sitios
            .get_or_insert_with(norte_frontend::places::PlacesState::new)
            .set_drives(&vols);
        // Las unidades se insertan ANTES que los favoritos: todo indice
        // pintado hasta ahora nombra otra fila.
        self.gen_sitios += 1;
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// Un click en una fila de la barra lateral: la elige Y la activa.
    pub(super) fn activar_sitio(
        &mut self,
        row: u32,
        generation: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if generation != self.gen_sitios {
            // Lo pulsado y lo que hay ahora no son la misma lista: los
            // volúmenes aterrizan EN MEDIO. Rechazar es lo único correcto —
            // `set_cursor` recorta al último, así que seguir habría navegado
            // al último sitio de la barra.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let Some(estado) = self.sitios.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if row as usize >= estado.rows().len() {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        estado.set_cursor(row as usize);
        self.activar_sitio_del_cursor(backend, buzon)
    }

    /// Activa la fila del cursor de la barra lateral: navega a ella, o pliega
    /// su sección si es una cabecera.
    ///
    /// El `cd` va al LISTADO enfocado por el mismo camino que cualquier otro:
    /// es lo que hace que tener la barra abierta no cambie a dónde van las
    /// operaciones.
    pub(super) fn activar_sitio_del_cursor(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(estado) = self.sitios.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if let Some(destino) = estado.activate().cloned() {
            return (
                self.aplicada(),
                self.navegar(&destino, Trail::Record, backend, buzon),
            );
        }
        // Una cabecera: se pliega. Y desplegar las unidades ES el momento de
        // volver a pedirlas — un disco montado o desmontado desde que se
        // abrió la ventana se ve aquí, sin un reloj de por medio.
        estado.toggle_fold();
        self.gen_sitios += 1;
        let desplegadas = estado
            .rows()
            .iter()
            .any(|r| matches!(r, norte_frontend::places::PlaceRow::Drive { .. }));
        if desplegadas {
            self.pedir_sitios(backend, buzon);
        }
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// La barra lateral de sitios, proyectada.
    ///
    /// Si todavía no hay estado —la disposición la coloca pero nadie la ha
    /// alimentado— se proyecta VACÍA con sus dos cabeceras, que es lo que
    /// hace el modelo compartido: la lista no da un brinco cuando lleguen los
    /// volúmenes.
    pub(super) fn barra_de_sitios(&self, id: u32) -> crate::dto::PlacesSlotView {
        use norte_frontend::places::{PlaceRow, PlacesState};
        let generation = self.gen_sitios;

        let vacia = PlacesState::new();
        let estado = self.sitios.as_ref().unwrap_or(&vacia);
        let rows = estado
            .rows()
            .iter()
            .map(|r| match r {
                PlaceRow::Header { section, folded } => crate::dto::PlaceRowView::Header {
                    label: clamp_display(norte_i18n::t_in(self.lang, section.label_key())),
                    folded: *folded,
                },
                PlaceRow::Drive {
                    label,
                    mount,
                    free,
                    total,
                    read_only,
                } => {
                    // La etiqueta son BYTES y el punto de montaje un `VPath`:
                    // los dos por la puerta compartida, nunca por
                    // `to_string_lossy`.
                    let (pintable, hostil) = if label.is_empty() {
                        norte_frontend::display::path_display(mount)
                    } else {
                        norte_frontend::display_name(label)
                    };
                    crate::dto::PlaceRowView::Drive {
                        label: clamp_display(pintable),
                        hostile: hostil,
                        detail: clamp_display(self.espacio_de(*free, *total, *read_only)),
                    }
                }
                PlaceRow::Favorite { name, target } => {
                    let (destino, hostil) = match target {
                        Ok(v) => norte_frontend::display::path_display(v),
                        Err(_) => (String::new(), false),
                    };
                    let (nombre, nombre_hostil) = norte_frontend::display_name(name.as_bytes());
                    crate::dto::PlaceRowView::Favorite {
                        // El nombre lo escribe el usuario, pero puede venir
                        // de la capa de PROYECTO: se enmascara igual.
                        name: clamp_display(nombre),
                        target: clamp_display(destino),
                        // El nombre O el destino. La bandera documentaba el
                        // destino y el nombre se enmascaraba tirando la suya,
                        // así que un favorito llamado con un override bidi
                        // llegaba sin marca ninguna.
                        hostile: hostil || nombre_hostil,
                        broken: target.as_ref().err().map_or_else(String::new, |clave| {
                            clamp_display(norte_i18n::t_in(self.lang, clave))
                        }),
                    }
                }
            })
            .collect();
        crate::dto::PlacesSlotView {
            slot_id: id,
            rows,
            cursor: estado.cursor() as u64,
            generation,
        }
    }

    /// El espacio de un volumen, dicho.
    ///
    /// Un tamaño que el sistema no contestó se DICE: un `0` se lee como
    /// «lleno», que es lo contrario de «no lo sé».
    pub(super) fn espacio_de(
        &self,
        free: Option<u64>,
        total: Option<u64>,
        read_only: bool,
    ) -> String {
        let mut trozos = Vec::new();
        match (free, total) {
            (Some(f), Some(t)) => trozos.push(norte_i18n::ta_in(
                self.lang,
                "picker-volume-space",
                &[
                    ("free", &norte_frontend::human_bytes_short(f)),
                    ("total", &norte_frontend::human_bytes_short(t)),
                ],
            )),
            _ => trozos.push(norte_i18n::t_in(self.lang, "volumes-size-unknown")),
        }
        if read_only {
            trozos.push(norte_i18n::t_in(self.lang, "picker-volume-read-only"));
        }
        trozos.join(" · ")
    }

    /// El hueco que ocupa la barra lateral, si la disposición coloca una.
    pub(super) fn hueco_de_sitios(&self) -> Option<SlotId> {
        self.reparto
            .placements
            .iter()
            .map(|(s, _)| *s)
            .find(|s| kind_de(&self.arbol, *s).is_some_and(|k| k.as_str() == "places"))
    }

    /// La hoja de atributos de un hueco `metadata`.
    ///
    /// Lo que enseña sale del panel al que este hueco SIGUE, resuelto con el
    /// motor compartido: un hueco que sigue a un rol que se ha quedado sin
    /// panel degrada al activo en vez de mirar al vacío en silencio.
    ///
    /// No pide nada: la `Entry` ya la trajo el listado.
    pub(super) fn hoja_de_atributos(&self, slot: SlotId) -> crate::dto::MetadataSlotView {
        let SlotId(id) = slot;
        let mut diags = Vec::new();
        let seguido =
            norte_frontend::layout::resolve_follow(&self.arbol, slot, &self.roles, &mut diags)
                .or_else(|| self.roles.get(norte_frontend::layout::RoleId::Active));
        // Con el FOCO en la propia hoja el rol activo es ella, y seguirse a
        // sí misma es seguir a nadie: entonces manda el listado activo, que
        // siempre existe (mismo arreglo que el visor acoplado, #291).
        let pane = seguido
            .and_then(|SlotId(s)| self.huecos.get(&s))
            .or_else(|| self.huecos.get(&self.activo()))
            .map(|h| &h.pane);
        // `cursor_entry` y no `selected`: la hoja DESCRIBE lo que hay bajo el
        // cursor, y sobre la fila `..` —donde el cursor nace— «lo señalado»
        // es `None` a propósito. Preguntando por el operando, el panel salía
        // vacío en cada arranque y después de cada `cd`.
        // La bandera sale del MISMO índice que la entrada (`cursor_entry` /
        // `cursor_is_parent_row`): preguntando por `cursor()` a mano, un
        // filtro de quick search —que elige por su cuenta y no mueve el
        // cursor real— dejaba la hoja describiendo `..` mientras el listado
        // resaltaba otra fila.
        let entrada = pane.and_then(|p| p.cursor_entry());
        let fila_de_subir = pane.is_some_and(PaneState::cursor_is_parent_row);
        let Some(e) = entrada else {
            return crate::dto::MetadataSlotView {
                slot_id: id,
                fields: Vec::new(),
                note: clamp_display(norte_i18n::t_in(self.lang, "metadata-empty")),
            };
        };
        // Qué filas van dentro lo decide el crate COMPARTIDO, no este host:
        // la misma hoja la pinta el TUI, y cuando cada uno tenía su copia ya
        // divergieron (el TUI no marcaba un valor de atributo hostil).
        // Aquí solo se acota lo que cruza el puente.
        let catalogo = self.catalogos.get(e.path.scheme());
        let fields = norte_frontend::metadata::sheet(e, fila_de_subir, catalogo, self.lang)
            .into_iter()
            .map(|f| crate::dto::MetadataFieldView {
                label: clamp_display(f.label),
                value: clamp_display(f.value),
                hostile: f.hostile,
            })
            .collect();
        crate::dto::MetadataSlotView {
            slot_id: id,
            fields,
            note: String::new(),
        }
    }
}
