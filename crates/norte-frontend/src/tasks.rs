//! Lo que un tablero de tasks pinta de un progreso, compartido.
//!
//! Aquí y no en un frontend por la misma razón que los avisos persistentes:
//! es aritmética de presentación sobre datos del wire, y dos copias acaban
//! divergiendo. La divergencia ya ocurrió una vez: el TUI caía a las
//! ENTRADAS cuando no había bytes totales y la ventana gráfica no, así que un
//! borrado —que no cuenta bytes— pintaba una barra clavada en cero.

/// El porcentaje de una task: por bytes si se conocen, si no por entradas.
///
/// `None` = no se sabe todavía (el walk no ha terminado y no hay totales).
/// Es `None` y no un `0` fingido a propósito: «va por el 0 %» y «no se sabe
/// cuánto queda» son dos cosas distintas, y el wire ya las distingue —los
/// totales son `Option` justo por eso—.
#[must_use]
pub fn progress_pct(p: &norte_proto::TaskProgress) -> Option<u8> {
    let de = |hecho: u64, total: u64| -> Option<u8> {
        if total == 0 {
            return None;
        }
        u8::try_from((hecho.min(total).saturating_mul(100)) / total).ok()
    };
    match (p.bytes_total, p.entries_total) {
        (Some(total), _) if total > 0 => de(p.bytes_done, total),
        (_, Some(total)) if total > 0 => de(p.entries_done, total),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progreso(bytes: Option<u64>, entradas: Option<u64>) -> norte_proto::TaskProgress {
        norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(1),
            kind: norte_proto::TaskKind::Delete,
            state: norte_proto::TaskState::Running,
            bytes_done: 5,
            bytes_total: bytes,
            entries_done: 1,
            entries_total: entradas,
            current: None,
        }
    }

    /// Con bytes, manda el byte.
    #[test]
    fn con_bytes_totales_manda_el_byte() {
        assert_eq!(progress_pct(&progreso(Some(10), Some(4))), Some(50));
    }

    /// Sin bytes totales, cuentan las entradas: un borrado no pesa bytes, y
    /// sin esta caída pintaba una barra parada en cero de principio a fin.
    #[test]
    fn sin_bytes_cuentan_las_entradas() {
        assert_eq!(progress_pct(&progreso(None, Some(4))), Some(25));
    }

    /// Sin ninguno de los dos, no se sabe, y eso NO es cero.
    #[test]
    fn sin_totales_no_se_sabe() {
        assert_eq!(progress_pct(&progreso(None, None)), None);
        assert_eq!(progress_pct(&progreso(Some(0), None)), None);
    }

    /// Un `done` mayor que su total no pasa del 100 %.
    #[test]
    fn no_se_pasa_del_cien() {
        let mut p = progreso(Some(2), None);
        p.bytes_done = 9;
        assert_eq!(progress_pct(&p), Some(100));
    }
}
