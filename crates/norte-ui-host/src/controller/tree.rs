//! El árbol de ramas del panel lateral.
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
    /// Tope de ramas hijas de UNA rama.
    ///
    /// Un directorio con cien mil subdirectorios no se pinta: se recorta, y lo
    /// que se ve es «hasta aquí». Sin tope, una sola rama abierta convierte
    /// cada foto del host en un mensaje de megabytes.
    pub(super) const MAX_RAMAS: usize = 2000;

    /// El hueco del árbol, si la disposición coloca uno.
    pub(super) fn hueco_de_ramas(&self) -> Option<SlotId> {
        self.arbol
            .slot_ids()
            .into_iter()
            .find(|s| kind_de(&self.arbol, *s).is_some_and(|k| k.as_str() == "tree"))
    }

    /// Ancla el árbol donde esté MIRANDO el listado enfocado.
    pub(super) fn sembrar_ramas(&mut self) {
        let raiz = self.hueco().pane.dir().clone();
        self.ramas
            .get_or_insert_with(norte_frontend::tree::Tree::default)
            .anchor(raiz);
        self.gen_ramas += 1;
    }

    /// El árbol sigue al listado ACTIVO: revela su directorio y pide lo que
    /// falte para pintarlo.
    ///
    /// Se llama desde el embudo por el que pasa TODO listado que aterriza
    /// ([`Estado::aterrizar_listado`]) y desde el cambio de foco, que son los
    /// dos momentos en que «dónde está mirando el panel» cambia. Ponerlo en
    /// cada gesto que provoca un `cd` —el ratón, la paleta, el menú, el
    /// rastro, el propio árbol— sería la lista que un día se queda corta.
    ///
    /// Solo el ACTIVO. Un listado del otro lado que termina de cargar no es
    /// dónde está trabajando el lector, y mover el árbol por él lo dejaría
    /// apuntando a un panel que nadie está mirando.
    ///
    /// Y revela, no re-ancla ([`norte_frontend::tree::Tree::follow`]): lo que
    /// el lector abrió a mano sigue abierto.
    pub(super) fn seguir_ramas(
        &mut self,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if self.hueco_de_ramas().is_none() || slot != self.activo() {
            return;
        }
        let Some(dir) = self.huecos.get(&slot).map(|h| h.pane.dir().clone()) else {
            return;
        };
        self.ramas
            .get_or_insert_with(norte_frontend::tree::Tree::default)
            .follow(&dir);
        // Las filas se han movido —hay ancestros desplegados que antes no
        // estaban—, así que todo índice pintado hasta ahora nombra otra rama.
        self.gen_ramas += 1;
        self.pedir_ramas(backend, buzon);
    }

    /// Pide la siguiente rama que haga falta, y UNA por vuelta.
    ///
    /// Perezoso por la misma razón que el listado local no trae tamaños: un
    /// árbol que se leyera entero al abrirse tardaría minutos en un `$HOME`
    /// grande y horas contra un remoto. Una rama por vuelta acota además lo
    /// que un directorio enorme o un servidor lento pueden trabar: la
    /// siguiente pide la siguiente.
    pub(super) fn pedir_ramas(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if self.hueco_de_ramas().is_none() {
            return;
        }
        let Some(dir) = self
            .ramas
            .as_ref()
            .and_then(norte_frontend::tree::Tree::wants)
        else {
            return;
        };
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        tokio::spawn(async move {
            // Sin atributos: el árbol enseña nombres de directorio y nada más,
            // y pedir tamaños o permisos por rama sería pagarlos por cada
            // carpeta que alguien despliega.
            let hijos = match backend.list(dir.clone(), Vec::new()).await {
                Ok((stream, _)) => Self::ramas_del_listado(stream).await,
                // Una rama que no se deja leer se marca como leída y VACÍA:
                // sin esto se volvería a pedir en cada vuelta, que es un bucle
                // de peticiones contra un directorio prohibido.
                Err(_) => Vec::new(),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::RamasDeArbol(dir, hijos))))
                .await;
        });
    }

    /// Los subdirectorios de un listado, en el orden del panel de al lado.
    pub(super) async fn ramas_del_listado(mut stream: norte_client::EntryStream) -> Vec<VPath> {
        use futures::StreamExt as _;
        let mut entradas = Vec::new();
        while entradas.len() < Self::MAX_RAMAS {
            match stream.next().await {
                Some(Ok(e)) => {
                    if e.kind == norte_proto::EntryKind::Dir {
                        entradas.push(e);
                    }
                }
                // Un error a mitad de rama deja lo que se leyó: media rama
                // enseña menos de lo que hay, pero no enseña nada FALSO, y la
                // alternativa es tirar el trabajo de un directorio enorme por
                // su última entrada.
                Some(Err(_)) | None => break,
            }
        }
        // El MISMO comparador que el listado de al lado: dos columnas que
        // enseñan lo mismo en distinto orden se leen como si dijeran cosas
        // distintas.
        norte_frontend::sort_entries(&mut entradas);
        entradas.into_iter().map(|e| e.path).collect()
    }

    /// Llegaron los hijos de una rama.
    ///
    /// Y se pide la siguiente aquí mismo: es lo que encadena el recorrido
    /// perezoso sin un reloj de por medio.
    pub(super) fn aplicar_ramas(
        &mut self,
        dir: VPath,
        hijos: Vec<VPath>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        // Solo si el árbol EXISTE en esta disposición: una respuesta rezagada
        // de un panel ya cerrado no resucita su estado ni manda una foto.
        self.hueco_de_ramas()?;
        let arbol = self.ramas.as_mut()?;
        arbol.insert_children(dir, hijos);
        // Las filas nuevas se insertan EN MEDIO: todo índice pintado hasta
        // ahora nombra otra rama.
        self.gen_ramas += 1;
        self.pedir_ramas(backend, buzon);
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// El árbol, proyectado.
    ///
    /// Sin estado se proyecta VACÍO en vez de no proyectarse: un hueco que la
    /// disposición coloca y el host no pinta desaparecería de la pantalla, y
    /// preservar lo que hay es la regla de la sesión (ADR 0059).
    pub(super) fn arbol_de_ramas(&self, id: u32) -> crate::dto::TreeSlotView {
        let vacio = norte_frontend::tree::Tree::default();
        let arbol = self.ramas.as_ref().unwrap_or(&vacio);
        let filas = arbol.rows();
        let raiz = arbol.root().cloned();
        let rows = filas
            .iter()
            .map(|r| {
                // La raíz lleva su ruta entera: «`/`» a secas, o el nombre de
                // la última carpeta, no dicen desde dónde cuelga esto.
                let (pintable, hostil) = if raiz.as_ref() == Some(&r.path) {
                    norte_frontend::path_display(&r.path)
                } else {
                    // Sin nombre solo la raíz de un provider, y esa ya se fue
                    // por la otra rama: aun así se pinta la ruta entera en vez
                    // de quedarse en blanco.
                    r.path.file_name().map_or_else(
                        || norte_frontend::path_display(&r.path),
                        |n| norte_frontend::display_name(n.as_bytes()),
                    )
                };
                crate::dto::TreeRowView {
                    label: clamp_display(pintable),
                    hostile: hostil,
                    depth: u32::try_from(r.depth).unwrap_or(u32::MAX),
                    expanded: r.expanded,
                    children: r.children,
                }
            })
            .collect();
        crate::dto::TreeSlotView {
            slot_id: id,
            rows,
            cursor: arbol.cursor() as u64,
            generation: self.gen_ramas,
        }
    }

    /// Un click en una rama: la elige, y según el gesto navega o la pliega.
    pub(super) fn tocar_rama(
        &mut self,
        row: u32,
        generation: u64,
        navegar: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if generation != self.gen_ramas {
            // Lo pulsado y lo que hay ahora no son el mismo árbol: los hijos
            // de una rama aterrizan EN MEDIO. Rechazar es lo único correcto —
            // seguir habría navegado a otra carpeta.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let Some(arbol) = self.ramas.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        if row as usize >= arbol.rows().len() {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        arbol.set_cursor(row as usize);
        if !navegar {
            arbol.toggle();
            self.gen_ramas += 1;
            self.pedir_ramas(backend, buzon);
            let snap = self.snapshot();
            return (
                self.aplicada(),
                vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
            );
        }
        // Desplegar Y navegar: quien pulsa sobre una rama quiere ver qué hay
        // dentro, y verlo en el listado es la respuesta completa.
        arbol.expand();
        let Some(destino) = arbol.selected() else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        self.gen_ramas += 1;
        self.pedir_ramas(backend, buzon);
        // Al listado ENFOCADO, por el mismo camino que cualquier otra
        // navegación: es lo que hace que tener el árbol abierto no cambie a
        // dónde van las operaciones.
        (
            self.aplicada(),
            self.navegar(&destino, Trail::Record, backend, buzon),
        )
    }
}
