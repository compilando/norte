//! El quick search de un pane: filtrar o saltar mientras se teclea.
//!
//! Aparte porque tiene su propio ciclo de vida —se abre, come teclas, se
//! confirma o se cancela— y porque un listado nuevo lo cierra: mezclado con
//! el resto del pane, cada método de listado tenía que acordarse de él.

use super::{Mode, PaneState, QuickSearch, VPath};

impl PaneState {
    /// Arranca el quick search en `mode` sobre las entries actuales, plegando
    /// con la reinterpretación de nombres vigente (#98/F1).
    pub fn quick_start(&mut self, mode: Mode) {
        self.quick = Some(QuickSearch::new(mode, &self.entries, self.name_encoding));
    }

    /// En [`Mode::Jump`] el cursor REAL sigue a la selección del quick search
    /// (el listado no cambia; saltar ES mover el cursor). En Filter, no-op.
    pub(super) fn quick_sync_jump(&mut self) {
        if let Some(q) = &self.quick
            && q.mode() == Mode::Jump
            && let Some(i) = q.selected_entry_index()
        {
            self.cursor = i;
        }
    }

    /// Un carácter tecleado con el quick search activo.
    pub fn quick_char(&mut self, c: char) {
        if let Some(q) = &mut self.quick {
            q.push_char(c);
            self.quick_sync_jump();
        }
    }

    /// Backspace con el quick search activo.
    pub fn quick_backspace(&mut self) {
        if let Some(q) = &mut self.quick {
            q.backspace();
            self.quick_sync_jump();
        }
    }

    /// Selección del quick search una posición abajo.
    pub fn quick_down(&mut self) {
        if let Some(q) = &mut self.quick {
            q.down();
            self.quick_sync_jump();
        }
    }

    /// Selección del quick search una posición arriba.
    pub fn quick_up(&mut self) {
        if let Some(q) = &mut self.quick {
            q.up();
            self.quick_sync_jump();
        }
    }

    /// Cierra el quick search fijando el cursor REAL a la selección (Enter: la
    /// op siguiente parte de ahí). Devuelve `true` si el cursor apunta a una
    /// entrada que el usuario VEÍA: en Filter sin matches devuelve `false` (la
    /// lista pintada estaba vacía); en Jump devuelve `true` si hay entradas (el
    /// listado se pinta entero, el cursor real es visible por definición).
    pub fn quick_confirm(&mut self) -> bool {
        let Some(q) = self.quick.take() else {
            return false;
        };
        if let Some(i) = q.selected_entry_index() {
            self.cursor = i;
            return true;
        }
        q.mode() == Mode::Jump && !self.entries.is_empty()
    }

    /// Cierra el quick search SIN tocar el cursor real: en Filter el listado
    /// completo vuelve con el cursor donde estaba; en Jump el cursor se queda
    /// donde saltó.
    pub fn quick_cancel(&mut self) {
        self.quick = None;
    }

    /// Índices REALES visibles bajo el filtro; `None` = sin filtro (quick
    /// inactivo, o modo Jump: el listado se pinta entero).
    #[must_use]
    pub fn quick_visible(&self) -> Option<&[usize]> {
        self.quick
            .as_ref()
            .filter(|q| q.mode() == Mode::Filter)
            .map(QuickSearch::visible)
    }

    /// Siguiente match con wrap (Tab en modo [`Mode::Jump`]): mueve la selección
    /// del quick al match siguiente y, en Jump, arrastra el cursor real. No-op
    /// sin quick search. (#82)
    pub fn quick_next(&mut self) {
        if let Some(q) = &mut self.quick {
            q.next_match();
            self.quick_sync_jump();
        }
    }

    /// El quick search vivo (para que el render pinte la query y su contador);
    /// `None` = navegación normal. Solo lectura. (#82)
    #[must_use]
    pub fn quick(&self) -> Option<&QuickSearch> {
        self.quick.as_ref()
    }

    /// El path de la entrada seleccionada DENTRO del quick search, capturado
    /// ANTES de mutar/re-ordenar `entries` (contrato de [`QuickSearch::refresh`]:
    /// los índices previos al sort no identifican nada). (#82)
    pub(super) fn quick_selected_path(&self) -> Option<VPath> {
        let i = self.quick.as_ref()?.selected_entry_index()?;
        Some(self.entries.get(i)?.path.clone())
    }

    /// RE-APLICA el quick vivo sobre las entradas ACTUALES (sin cambiarlas ni
    /// mover el cursor real): cierra un fill cuyo cierre podría re-ordenar. (#82)
    pub fn refresh_quick(&mut self) {
        let quick_prev = self.quick_selected_path();
        if let Some(q) = &mut self.quick {
            q.refresh(&self.entries, quick_prev.as_ref());
        }
    }
}
