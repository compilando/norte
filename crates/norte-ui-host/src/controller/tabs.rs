//! Pestañas y partición de huecos.
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
    /// Los tres efectos que tocan la DISPOSICIÓN, juntos.
    ///
    /// Agrupados aquí y no en `aplicar_efecto` porque ese método es un
    /// reparto y crece por familias: tres brazos que hacen lo mismo —cambiar
    /// la forma de la pantalla— son un brazo con tres casos.
    pub(super) fn efecto_de_disposicion(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::Tamano(delta) => self.redimensionar(delta, backend, buzon),
            Efecto::Igualar => self.igualar(backend, buzon),
            Efecto::Partir { vertical } => self.partir(vertical, backend, buzon),
            Efecto::CerrarHueco => self.cerrar_hueco(backend, buzon),
            Efecto::AlternarHueco { kind } => self.alternar_hueco(kind, backend, buzon),
            Efecto::PestanaNueva => self.pestana_nueva(backend, buzon),
            Efecto::CerrarPestana => self.cerrar_pestana(backend, buzon),
            Efecto::CiclarPestana { atras } => self.ciclar_pestana(atras, backend, buzon),
            Efecto::MoverPestana { derecha } => self.mover_pestana(derecha, backend, buzon),
            Efecto::IrAPestana { n } => self.ir_a_pestana(n, backend, buzon),
            _ => self.abrir_disposiciones(),
        }
    }

    /// Abre otra PESTAÑA junto al hueco enfocado.
    ///
    /// El listado nuevo arranca en el mismo directorio y se queda el foco,
    /// por lo mismo que al partir. `add_tab` envuelve el hueco en un grupo si
    /// todavía no lo estaba: no hay que decidirlo aquí.
    pub(super) fn pestana_nueva(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::KindId;
        let id = self.nuevo_slot();
        let nuevo = self.arbol.add_tab(
            SlotId(self.enfocado()),
            &Node::slot(SlotId(id), KindId::browser()),
        );
        self.aplicar_disposicion_con(nuevo, Some(SlotId(id)), backend, buzon)
    }

    /// Cierra la pestaña enfocada.
    ///
    /// Sin grupo no hace nada y lo DICE: cerrar el hueco entero es otro
    /// comando, y hacerlo aquí «porque no había pestañas» sería cerrar lo que
    /// nadie pidió cerrar.
    pub(super) fn cerrar_pestana(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(nuevo) = self.arbol.close_tab(SlotId(self.enfocado())) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
            );
        };
        self.aplicar_disposicion(nuevo, backend, buzon)
    }

    /// Pasa a la pestaña siguiente —o anterior—, CICLANDO.
    pub(super) fn ciclar_pestana(
        &mut self,
        atras: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let foco = SlotId(self.enfocado());
        let Some((tabs, activo)) = self.arbol.tabs_of(foco) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
            );
        };
        if tabs.is_empty() {
            return (self.aplicada(), Vec::new());
        }
        let n = isize::try_from(tabs.len()).unwrap_or(1);
        let i = isize::try_from(activo).unwrap_or(0);
        let delta = if atras { -1 } else { 1 };
        let destino = usize::try_from((i + delta).rem_euclid(n)).unwrap_or(0);
        self.activar_pestana(foco, destino, &tabs, backend, buzon)
    }

    /// Va a la pestaña `n` (base 1).
    pub(super) fn ir_a_pestana(
        &mut self,
        n: usize,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let foco = SlotId(self.enfocado());
        let Some((tabs, _)) = self.arbol.tabs_of(foco) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
            );
        };
        let i = n.saturating_sub(1);
        if i >= tabs.len() {
            // Pedir la séptima cuando hay tres no va a la última: no es lo
            // que se pidió, y adivinar aquí es cambiar de pestaña sola.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-such-tab".to_owned(),
                },
                Vec::new(),
            );
        }
        self.activar_pestana(foco, i, &tabs, backend, buzon)
    }

    /// Pone delante la pestaña `destino` del grupo de `foco`.
    ///
    /// Y le da el FOCO: la pestaña que está delante es con la que se trabaja,
    /// y dejarlo en la que se acaba de esconder deja las teclas apuntando a
    /// un listado que no se ve.
    pub(super) fn activar_pestana(
        &mut self,
        foco: SlotId,
        destino: usize,
        tabs: &[SlotId],
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let nuevo = self.arbol.set_active_for(foco, destino);
        let activo = tabs.get(destino).copied();
        self.aplicar_disposicion_con(nuevo, activo, backend, buzon)
    }

    /// Mueve la pestaña enfocada dentro de su grupo.
    ///
    /// NO da la vuelta: una pestaña que salta del final al principio por una
    /// pulsación de más es justo lo que nadie quería (la regla es del modelo
    /// compartido, y aquí solo se usa).
    pub(super) fn mover_pestana(
        &mut self,
        derecha: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let foco = SlotId(self.enfocado());
        if self.arbol.tabs_of(foco).is_none() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-tabs".to_owned(),
                },
                self.decir("host-no-tabs"),
            );
        }
        let nuevo = self.arbol.move_tab(foco, if derecha { 1 } else { -1 });
        self.aplicar_disposicion_con(nuevo, Some(foco), backend, buzon)
    }

    /// Un clic en una pestaña: la pone delante.
    ///
    /// El hueco viene del propio grupo, así que un clic contra un árbol que
    /// ya cambió no acierta por casualidad: si ese id ya no está en un grupo,
    /// se rehúsa.
    pub(super) fn elegir_pestana(
        &mut self,
        slot_id: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let quien = SlotId(slot_id);
        let Some((tabs, _)) = self.arbol.tabs_of(quien) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        let Some(i) = tabs.iter().position(|t| *t == quien) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        self.activar_pestana(quien, i, &tabs, backend, buzon)
    }

    /// El id de hueco más alto del árbol, más uno.
    ///
    /// Del ÁRBOL y no de `huecos`: los auxiliares —sitios, tablero, hoja de
    /// atributos— no están en ese mapa, y reusar el id de uno abierto sería
    /// meter dos cosas en el mismo hueco.
    pub(super) fn nuevo_slot(&self) -> u32 {
        self.arbol
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| id)
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    /// Parte el hueco enfocado y pone otro LISTADO al lado.
    ///
    /// El nuevo arranca en el directorio del que se parte, que es lo menos
    /// sorprendente: pedir sitio para trabajar no es irse a otra parte. Y el
    /// foco va al recién nacido, por lo mismo.
    pub(super) fn partir(
        &mut self,
        vertical: bool,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::{Dir, KindId};
        let dir = if vertical {
            Dir::Vertical
        } else {
            Dir::Horizontal
        };
        // Que quepan DOS, y con la misma cuenta que decide el colapso del
        // reparto: partir un hueco que ya no da para dos crea un panel que el
        // propio reparto esconde en el mismo frame —el `Split` se degrada a
        // pestañas— con el árbol guardándolo igualmente. La TUI se niega por
        // este mismo sitio (ADR 0077: una decisión duplicada entre frontends
        // diverge en silencio).
        let sitio = self
            .reparto
            .placements
            .iter()
            .find(|(s, _)| s.0 == self.enfocado())
            .is_none_or(|(_, re)| {
                norte_frontend::layout::has_room_to_split(*re, dir, &KindId::browser(), &self.kinds)
            });
        if !sitio {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-split-no-room".to_owned(),
                },
                self.decir("msg-layout-split-no-room"),
            );
        }
        let id = self.nuevo_slot();
        let nuevo = self.arbol.split_slot(
            SlotId(self.enfocado()),
            dir,
            &Node::slot(SlotId(id), KindId::browser()),
        );
        // El foco al recién nacido, y DENTRO de la misma aplicación: partir
        // es pedir sitio para trabajar en él. En dos pasos serían dos fotos,
        // y la primera enseñaría el foco donde ya no está.
        self.aplicar_disposicion_con(nuevo, Some(SlotId(id)), backend, buzon)
    }

    /// Cierra el hueco enfocado.
    ///
    /// Salvo si con eso la pantalla se queda sin LISTADO: una pantalla sin un
    /// listado usable no es una pantalla —es un cuelgue con bordes—, y esa es
    /// la misma regla que el reparto compartido ya aplica por su cuenta
    /// (#229). Aquí se dice, en vez de dejar una tecla que no hace nada.
    pub(super) fn cerrar_hueco(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(nuevo) = self.arbol.close_slot(SlotId(self.enfocado())) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-last-panel".to_owned(),
                },
                self.decir("msg-layout-last-panel"),
            );
        };
        let quedan = nuevo
            .slot_ids()
            .into_iter()
            .any(|s| es_listado(&nuevo, s, &self.kinds));
        if !quedan {
            return (
                ActionAck::Unavailable {
                    reason_key: "msg-layout-last-panel".to_owned(),
                },
                self.decir("msg-layout-last-panel"),
            );
        }
        self.aplicar_disposicion(nuevo, backend, buzon)
    }

    /// Abre —o cierra— el hueco auxiliar de este kind.
    ///
    /// Los tres que esta ventana sabe PINTAR. Uno que solo se pintaría en
    /// gris no se abre: `layout.preview` sigue sin construirse por eso, y lo
    /// dice el catálogo, no un hueco vacío.
    ///
    /// Los bordes y los tamaños son los MISMOS que el TUI usa, y no por
    /// simetría: son anchos medidos —dieciséis celdas es el mínimo del kind
    /// de sitios, ocho filas son las seis del tablero más el marco, treinta
    /// es la etiqueta más larga de la hoja con su valor al lado.
    pub(super) fn alternar_hueco(
        &mut self,
        kind: &'static str,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        use norte_frontend::layout::{Bindings, Edge, Follow, KindId, Size};
        let abierto = self
            .arbol
            .slot_ids()
            .into_iter()
            .find(|s| kind_de(&self.arbol, *s).is_some_and(|k| k.as_str() == kind));
        if let Some(id) = abierto {
            let Some(nuevo) = self.arbol.close_slot(id) else {
                return (self.aplicada(), Vec::new());
            };
            // Cerrar el registro BAJA lo que el proceso captura (#326): el
            // nivel del anillo se sube en caliente para poder enseñar más, y
            // solo sube. Sin esto, una sola pulsación de «traza» dejaba el
            // proceso guardando TRACE en memoria el resto de la sesión —
            // incluida la cota de `suppaftp`, que es lo único que impide que
            // ahí dentro aparezca una contraseña de FTP— con la interfaz
            // diciendo «info» y sin ningún panel donde verlo. Es lo que ya
            // hace la TUI al cerrar el suyo.
            if kind == super::logpanel::KIND {
                if let Some(anillo) = &self.log_ring {
                    anillo.set_level(self.log_panel.level());
                }
                // Y el sondeo se apaga: la época sube, así que el temporizador
                // en vuelo se deja morir sin rearmarse.
                self.log_epoca += 1;
            }
            return self.aplicar_disposicion(nuevo, backend, buzon);
        }
        let id = SlotId(self.nuevo_slot());
        let hoja = match kind {
            // La hoja de atributos SIGUE al rol activo: describe lo que el
            // cursor señala, y sin la atadura describiría el hueco donde
            // nació para siempre.
            "metadata" => Node::slot_bound(
                id,
                KindId::new(kind),
                Bindings {
                    follows: Some(Follow::Role(RoleId::Active)),
                },
            ),
            _ => Node::slot(id, KindId::new(kind)),
        };
        let (borde, tamano) = match kind {
            "places" => (Edge::Left, Size::Fixed(16)),
            "processes" => (Edge::Bottom, Size::Fixed(8)),
            // El registro abajo, y más alto que el tablero: sus líneas son
            // largas y ocho filas de las que dos son cromo no dejan leer una
            // traza. Es el mismo sitio que le da la TUI.
            "log" => (Edge::Bottom, Size::Fixed(12)),
            // El árbol a la izquierda y con el ancho de la barra de sitios: es
            // el mismo gesto —una columna de navegación al lado del listado— y
            // dos anchos distintos para lo mismo se notan.
            "tree" => (Edge::Left, Size::Fixed(24)),
            _ => (Edge::Right, Size::Fixed(30)),
        };
        let nuevo = self
            .arbol
            .dock(SlotId(self.enfocado()), borde, tamano, &hoja);
        let salida = self.aplicar_disposicion(nuevo, backend, buzon);
        if kind == "tree" {
            // Anclar al abrirlo, y solo entonces: el árbol se queda donde
            // está mientras el listado navega, que es lo que hace útil tenerlo
            // abierto. Re-anclarlo en cada navegación tiraría las ramas
            // abiertas cada vez que el lector entra en una carpeta.
            self.sembrar_ramas();
            self.pedir_ramas(backend, buzon);
        }
        if kind == super::logpanel::KIND {
            // Y el registro empieza a sondearse: el panel promete que SIGUE lo
            // que llega, y esta ventana solo repinta cuando alguien hace algo.
            // Sin esto, decía «pegado al final» sobre una lista congelada.
            self.log_epoca += 1;
            self.log_visto = self
                .log_ring
                .as_ref()
                .map_or(0, norte_config::logring::LogRing::pushed);
            self.sondear_registro(buzon);
        }
        salida
    }
}
