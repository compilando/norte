//! Comparar y sincronizar vistos desde `App`: resolver QUÉ se compara y qué
//! se sincroniza, cerrar los dos paneles, y la sonda de tamaño con la que se
//! hidrata la fila seleccionada del panel de diferencias (#157).

use super::{App, CompareView};
use norte_frontend::sync::SyncRoots;
use norte_i18n::t;
use norte_proto::{EntryKind, VPath};

impl App {
    /// `Shift+F2`: resuelve QUÉ comparar y lo deja pendiente para el run loop.
    ///
    /// El izquierdo es el pane con FOCO (spec: «el panel que lanzó la
    /// comparación es el izquierdo»), el derecho el otro — no `panes[0]` y
    /// `panes[1]`, porque el lector que pulsa la tecla desde el pane derecho
    /// espera que su directorio sea el suyo.
    ///
    /// Dos negativas se dan AQUÍ, sin ir y volver al daemon:
    ///
    /// * **Los dos panes en el mismo sitio.** El daemon responde `-32602` a
    ///   eso (C6) y tiene razón, pero la frase que el lector necesita no
    ///   depende de una vuelta por la red.
    /// * **Un pane virtual.** Una lista de hits no es un directorio, así que
    ///   no hay raíz que mandar — la misma negativa que ya dan mirror y pull.
    pub fn request_compare(&mut self) {
        // El visor sustituye a los panes en la pantalla y `ui::draw` le da
        // precedencia sobre este panel, así que abrirlo por detrás dejaría
        // los píxeles diciendo una cosa y el teclado yendo a otra — el bug
        // exacto contra el que está escrito el rustdoc de `modal_wins`.
        if self.viewer.is_some() {
            return;
        }
        if self.panes[0].virtual_search || self.panes[1].virtual_search {
            self.message = Some(t("msg-pane-not-a-location"));
            return;
        }
        let left = self.focused().dir().clone();
        let right = self.panes[self.focus() ^ 1].dir().clone();
        if left == right {
            self.message = Some(t("compare-same-path"));
            return;
        }
        self.pending_compare = Some(norte_proto::methods::FsCompareParams {
            left,
            right,
            criteria: norte_proto::methods::CompareCriteria::default(),
            max_depth: None,
            // Vive con el modelo (#158), no aquí: la GUI pide la MISMA
            // comparación, y dos copias que se separaran darían veredictos
            // distintos para los mismos dos directorios.
            mtime_tolerance_ms: norte_frontend::compare::MTIME_TOLERANCE_MS,
            // Sin toggle en la UI, y a propósito: `Backend::compare` responde
            // `Unsupported` a `true` antes de que exista Task alguna, porque
            // el engine acepta el campo y lo ignora. Ofrecer la casilla sería
            // ofrecer una promesa que nadie cumple.
            follow_symlinks: false,
            // Tampoco hay toggle: el pane de diferencias enseña un huérfano
            // como UNA fila, y descenderlo es lo que un plan de
            // sincronización pide por su cuenta (spec 2).
            descend_orphans: None,
        });
    }

    /// Las dos raíces de una sincronización, en el orden `(origen, destino)`.
    ///
    /// Con el panel de diferencias abierto las decide su lado ACTIVO, que es
    /// lo que `Tab` cambia: nada se infiere del foco ni del orden de los
    /// panes, porque el sentido de una sincronización es la mitad de lo que
    /// hay que aprobar. Sin panel abierto son el pane con foco y el otro, el
    /// mismo reparto que [`Self::request_compare`].
    ///
    /// La decisión ENTERA es [`norte_frontend::sync::sync_roots`] (#161), no
    /// una copia local de sus dos brazos: la GUI necesita exactamente la misma
    /// —incluido el brazo del lado activo, que es el que llega con su panel—
    /// y aquí solo se le da lo que esta TUI sabe.
    #[must_use]
    fn sync_roots(&self) -> SyncRoots {
        let other = &self.panes[self.focus() ^ 1];
        norte_frontend::sync::sync_roots(
            self.sync_source_view(),
            &norte_frontend::sync::Panes {
                focused_root: self.focused().dir(),
                focused_encoding: self.focused().name_encoding(),
                other_root: other.dir(),
                other_encoding: other.name_encoding(),
            },
        )
    }

    /// El panel de diferencias del que sale la selección, si lo hay.
    fn sync_source_view(&self) -> Option<&CompareView> {
        self.compare.as_ref()
    }

    /// `sync.plan`: resuelve QUÉ sincronizar y lo deja pendiente para el run
    /// loop, o dice por qué no.
    ///
    /// Devuelve los params que dejó pendientes, para que un test lea la
    /// decisión sin run loop.
    ///
    /// Las negativas que se dan AQUÍ, sin ir y volver al daemon:
    ///
    /// * **Sin journal.** Lo dice [`norte_core::backend::Backend::is_journalled`],
    ///   que es `false` en embebido: desde #167 ese engine sí lleva el journal
    ///   del directorio de estado, pero no instala spool, y sin spool
    ///   `sync.plan` se niega en cerrado (regla dura 4). Planificar contra él
    ///   sería enseñar un plan que nadie puede aprobar. La misma verdad que
    ///   [`norte_frontend::availability::Facts::journalled`] ya atenúa en la
    ///   hoja de referencia; esto es lo que pasa si el lector llega igual.
    /// * **Un pane virtual**, y **las dos raíces en el mismo sitio**: idénticas
    ///   a las de comparar, por las mismas razones.
    /// * **Más marcas que [`SYNC_MAX_INCLUDE`]**. `Backend::sync_plan` lo
    ///   rechaza con `InvalidPath`, que no dice cuántas sobran.
    ///
    /// [`SYNC_MAX_INCLUDE`]: norte_proto::methods::SYNC_MAX_INCLUDE
    pub fn request_sync(
        &mut self,
        mode: norte_proto::methods::SyncMode,
    ) -> Option<&norte_proto::methods::SyncPlanParams> {
        if self.viewer.is_some() {
            return None;
        }
        if !self.backend_journalled {
            self.message = Some(t("msg-sync-needs-daemon"));
            return None;
        }
        if self.compare.is_none() && (self.panes[0].virtual_search || self.panes[1].virtual_search)
        {
            self.message = Some(t("msg-pane-not-a-location"));
            return None;
        }
        let SyncRoots {
            source,
            dest,
            source_encoding,
            dest_encoding,
        } = self.sync_roots();
        if source == dest {
            self.message = Some(t("compare-same-path"));
            return None;
        }
        let include = match self.sync_include(&source, &dest) {
            Ok(include) => include,
            Err(e) => {
                self.message = Some(norte_frontend::sync::include_error_message(
                    &e,
                    norte_i18n::active(),
                ));
                return None;
            }
        };
        self.pending_sync = Some(norte_proto::methods::SyncPlanParams {
            source,
            dest,
            mode,
            // Los criterios son los de comparar y el default del wire ya los
            // trae. `follow_symlinks` y `descend_orphans` se quedan en su
            // default a propósito: `Backend::sync_plan` responde `Unsupported`
            // a los dos, porque el segundo no es del llamante —lo fija el
            // planificador al lado del origen— y el primero no lo cumple nadie.
            compare: norte_proto::methods::SyncCompareOptions::default(),
            on_unknown: norte_proto::methods::OnUnknown::default(),
            include,
        });
        // La reinterpretación del ORIGEN se congela aquí, con las raíces, y
        // viaja al panel: un lector que había pulsado `Alt+E` para leer un
        // share CP1251 no puede recuperar `????.txt` al sincronizarlo (#57,
        // el mismo fallo que el panel de diferencias arregló en su review).
        self.pending_sync_encoding = (source_encoding, dest_encoding);
        self.pending_sync.as_ref()
    }

    /// La lista `include` que sale de las marcas del panel de diferencias, o
    /// el motivo por el que no hay una.
    ///
    /// QUÉ cuenta como negativa —y contra qué raíz se mide cada marca— lo
    /// decide [`norte_frontend::sync::include_from_rows`], que vive junto a
    /// `anchor_of` porque contesta la misma pregunta del otro lado del viaje.
    ///
    /// # Errors
    /// Lo que devuelva aquella; [`Self::request_sync`] lo traduce a una frase.
    fn sync_include(
        &self,
        source: &VPath,
        dest: &VPath,
    ) -> Result<Option<Vec<norte_proto::methods::RelPath>>, norte_frontend::sync::IncludeError>
    {
        let marked = self
            .compare
            .as_ref()
            .map(|v| v.pane.marked_rows())
            .unwrap_or_default();
        norte_frontend::sync::include_from_rows(source, dest, &marked)
    }

    /// Cierra el panel de sincronización. La cancelación de la Task es del run
    /// loop (es suya); esto solo suelta el estado de presentación.
    pub fn close_sync(&mut self) {
        self.sync = None;
        self.pending_sync_apply = None;
    }

    /// El pane al que pertenece el lado ACTIVO del panel de diferencias.
    ///
    /// `None` con el panel cerrado. Es lo que hace que el `Enter` de una fila
    /// lleve al lector a donde esa fila vive DE VERDAD sin costarle el otro
    /// directorio.
    #[must_use]
    pub fn compare_active_pane(&self) -> Option<usize> {
        let view = self.compare.as_ref()?;
        Some(match view.pane.active_side() {
            norte_proto::methods::Side::Right => view.left_pane ^ 1,
            _ => view.left_pane,
        })
    }

    /// Cierra el panel de diferencias. La cancelación de la Task es del run
    /// loop (es suya); esto solo suelta el estado de presentación.
    pub fn close_compare(&mut self) {
        self.compare = None;
    }

    /// Paths de la fila SELECCIONADA del panel de diferencias que valen la
    /// pena sondear con un `stat` (#157): un lado con entrada, de tipo
    /// `File` (los directorios y los enlaces no tienen un tamaño que un
    /// `stat` corriente resuelva — mismo criterio que
    /// [`Self::focused_needs_stat`]), sin `size` ya, y que
    /// [`Self::compare_size_probed`] no haya pedido todavía.
    ///
    /// `None` cuando no hay panel abierto o su fila seleccionada no tiene
    /// nada que hidratar — que es el caso normal en cuanto la sonda ya
    /// contestó, así que el run loop no vuelve a pedir lo mismo cada frame.
    #[must_use]
    pub fn compare_size_probe_targets(&self) -> Vec<VPath> {
        let Some(view) = &self.compare else {
            return Vec::new();
        };
        let Some(row) = view.pane.selected_row() else {
            return Vec::new();
        };
        [row.left.as_ref(), row.right.as_ref()]
            .into_iter()
            .flatten()
            .filter(|e| {
                e.kind == EntryKind::File
                    && e.size.is_none()
                    && !self.compare_size_probed.contains(&e.path)
            })
            .map(|e| e.path.clone())
            .collect()
    }

    /// Mete el resultado de la sonda #157 en la caché de presentación
    /// ([`Self::compare_size_hints`]) y lo marca sondeado
    /// ([`Self::compare_size_probed`]) pase lo que pase — un `stat` que
    /// falló tampoco se reintenta hasta la próxima comparación, mismo
    /// criterio que el pane normal con `last_probed`.
    pub fn hydrate_compare_size(&mut self, generation: u64, path: VPath, size: Option<u64>) {
        // #198: de OTRA comparación. Ni el tamaño ni la marca de sondeado —
        // marcarlo dejaría a la comparación viva sin pedirlo nunca, que es la
        // mitad silenciosa del mismo fallo.
        if generation != self.compare_generation {
            return;
        }
        self.compare_size_probed.insert(path.clone());
        if let Some(size) = size {
            self.compare_size_hints.insert(path, size);
        }
    }

    /// La comparación que empieza. Vacía la caché de tamaños y su dedup, y
    /// AVANZA la generación: lo uno sin lo otro es el fallo de #198.
    pub fn begin_compare_generation(&mut self) {
        self.compare_size_hints.clear();
        self.compare_size_probed.clear();
        self.compare_generation = self.compare_generation.wrapping_add(1);
    }

    /// La comparación a la que pertenecen las tablas de tamaños ahora mismo.
    /// El run loop la guarda al lanzar la sonda y la devuelve al hidratar.
    #[must_use]
    pub fn compare_generation(&self) -> u64 {
        self.compare_generation
    }
}
