//! El menú de la ventana.
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
    /// Cierra el menú desplegado, apuntando por dónde iba.
    ///
    /// UNA puerta: el menú se cierra desde cuatro sitios —la tecla, `Escape`,
    /// elegir una entrada y pulsar fuera— y el que se olvidara de apuntar
    /// sería el que hace que la siguiente apertura empiece por el primero sin
    /// motivo aparente.
    pub(super) fn olvidar_menu(&mut self) {
        if let Some(m) = &self.menu {
            self.menu_ultimo = m.menu();
        }
        self.menu = None;
    }

    /// Despliega la barra de menús por donde iba, o la cierra si ya estaba.
    ///
    /// La misma tecla abre y cierra, como en el TUI: `alt+m` es «el menú», y
    /// pulsarla dos veces no puede dejar dos desplegables ni exigir `Esc`.
    pub(super) fn abrir_menu(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.menu.is_some() {
            self.olvidar_menu();
        } else {
            self.menu = Some(norte_frontend::menu::MenuState::reopen_at(self.menu_ultimo));
        }
        let cambio = ViewChange::Menu {
            menu: self.vista_menu(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Contesta con la pantalla ENTERA.
    ///
    /// Dos acciones lo hacen —un `Resync` y un cambio de tamaño— y las dos por
    /// el mismo motivo: lo que cambia no cabe en un parche, porque cambia
    /// todo. Una sola copia para que no diverjan.
    pub(super) fn responde_con_foto(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let snap = self.snapshot();
        (
            self.aplicada(),
            vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))],
        )
    }

    /// Un click en un título de la barra: despliega ese menú, o pliega el que
    /// hubiera si era el mismo.
    ///
    /// Un índice fuera de la barra se rechaza como obsoleto y no cierra nada:
    /// es una carrera con un catálogo anterior, no una orden.
    pub(super) fn desplegar_menu(
        &mut self,
        menu: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let i = menu as usize;
        if i >= norte_frontend::menu::MENUS.len() {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let mismo = self.menu.as_ref().is_some_and(|m| m.menu() == i);
        if mismo {
            self.olvidar_menu();
        } else {
            self.menu = Some(norte_frontend::menu::MenuState::reopen_at(i));
        }
        let cambio = ViewChange::Menu {
            menu: self.vista_menu(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// El ratón por encima de una entrada: mueve el cursor y nada más.
    pub(super) fn apuntar_en_menu(
        &mut self,
        row: u32,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(m) = self.menu.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        m.point_at(row as usize);
        let cambio = ViewChange::Menu {
            menu: self.vista_menu(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Un click sobre una entrada: la ejecuta.
    ///
    /// Se resuelve contra el menú que el HOST tiene abierto, no contra lo que
    /// diga el renderer: una fila que ya no existe —el menú cambió entre el
    /// pintado y el click— no ejecuta nada.
    pub(super) fn activar_del_menu(
        &mut self,
        row: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(m) = self.menu.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        m.point_at(row as usize);
        if m.item() != row as usize {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let elegido = m.selected();
        self.ejecutar_del_menu(elegido, backend, buzon)
    }

    /// Un click en la barra de paneles (#324): el botón `button` de la barra
    /// que este host mandó, por el MISMO despacho que su atajo. Dos caminos
    /// para abrir el mismo panel divergen en cuanto uno crece un detalle —
    /// la lección de ADR 0077 aplicada dentro de un solo frontend, igual que
    /// en la TUI.
    pub(super) fn pulsar_barra_de_paneles(
        &mut self,
        button: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let botones = self.botones_de_paneles();
        let Some(boton) = botones.get(button as usize) else {
            // La barra que el renderer pintó ya no es esta: un plugin
            // aportó un kind, o se retiró. Que pida foto.
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        let comando = boton.command.clone();
        match crate::commands::efecto_de(&comando, 1) {
            Some(efecto) => self.aplicar_efecto(efecto, backend, buzon),
            // Un kind aportado cuyo `layout.<kind>` no está en el catálogo:
            // el botón existe para ENSEÑAR el panel, y decir que no se puede
            // abrir desde aquí es mejor que un click mudo.
            None => self.no_implementado(&comando),
        }
    }

    /// Un click FUERA del desplegable lo cierra sin ejecutar nada.
    pub(super) fn cerrar_menu(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if self.menu.is_none() {
            return (self.aplicada(), Vec::new());
        }
        self.olvidar_menu();
        let cambio = ViewChange::Menu {
            menu: self.vista_menu(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }
}
