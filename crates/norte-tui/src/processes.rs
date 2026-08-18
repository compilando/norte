//! El panel de procesos (fase A): el cursor, y nada más.
//!
//! Las filas son las del `TaskBoard` que ya pinta la franja: este panel no
//! guarda una segunda copia, porque dos listas de tareas se separan y la que
//! se ve deja de ser la que se cancela.
//!
//! **No hay pausa.** El protocolo tiene `task.cancel` y no tiene otra cosa, y
//! un control que no hace lo que dice es peor que un control que falta.

/// El kind que ocupa un hueco de procesos.
pub const KIND: &str = "processes";

/// El estado propio del panel: dónde está el cursor.
#[derive(Debug, Default)]
pub struct Processes {
    cursor: usize,
}

impl Processes {
    /// Dónde está el cursor, acotado a `filas`.
    ///
    /// Se acota al LEER y no al mover: las filas aparecen y desaparecen solas
    /// —una tarea termina y se barre—, así que un cursor guardado siempre
    /// puede haberse quedado fuera.
    #[must_use]
    pub fn cursor(&self, filas: usize) -> usize {
        self.cursor.min(filas.saturating_sub(1))
    }

    /// Sube.
    pub const fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja, sin pasarse de la última fila.
    pub fn down(&mut self, filas: usize) {
        self.cursor = (self.cursor + 1).min(filas.saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_cursor_no_se_sale_por_abajo() {
        let mut p = Processes::default();
        p.down(2);
        p.down(2);
        p.down(2);
        assert_eq!(p.cursor(2), 1);
    }

    #[test]
    fn el_cursor_no_se_sale_por_arriba() {
        let mut p = Processes::default();
        p.up();
        assert_eq!(p.cursor(3), 0);
    }

    /// Una tarea termina y su fila se va: el cursor guardado apuntaba a una
    /// fila que ya no está, y lo que se lee es la última que sí está.
    #[test]
    fn un_cursor_de_una_fila_que_ya_no_esta_se_acota_al_leer() {
        let mut p = Processes::default();
        p.down(5);
        p.down(5);
        assert_eq!(p.cursor(5), 2);
        assert_eq!(p.cursor(1), 0);
    }

    /// Sin filas no hay cursor que valga, y el `saturating_sub` es lo que
    /// evita el pánico de índice en un panel abierto con el sistema en reposo.
    #[test]
    fn sin_filas_el_cursor_es_cero() {
        let p = Processes::default();
        assert_eq!(p.cursor(0), 0);
    }
}
