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

/// Peso de la muestra nueva en la media del ritmo.
///
/// Un tercio: con el ritmo instantáneo a secas el número baila en cada
/// snapshot (el wire coalesce a 30 Hz y un fichero pequeño entra entero entre
/// dos), y con una media larga el ritmo tarda en enterarse de que la red se
/// cayó. Un tercio se asienta en unas pocas muestras y sigue reaccionando.
const PESO: f64 = 1.0 / 3.0;

/// El ritmo de una task, estimado de sus snapshots.
///
/// **No viene del wire**: `TaskProgress` dice cuánto va hecho y no a qué
/// velocidad, así que el ritmo lo calcula quien mira, con el reloj del
/// pintado. Vive aquí porque las dos superficies lo enseñan y una media
/// distinta en cada una sería otra divergencia silenciosa (ADR 0077).
///
/// Es por TASK: quien lo guarda es el tablero, y una task que no publica dos
/// veces no tiene ritmo — `None`, que no es «cero».
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rate {
    ultimo: Option<(u64, i64)>,
    bps: Option<f64>,
}

impl Rate {
    /// Anota un snapshot y devuelve el ritmo vigente, en bytes por segundo.
    ///
    /// Descarta lo que no puede medir: dos snapshots con el mismo reloj (o uno
    /// que va hacia atrás, que en un reloj inyectado es un test o un ajuste de
    /// hora) y un contador que RETROCEDE — una task reanudada empieza a contar
    /// de nuevo, y arrastrar el ritmo viejo mentiría sobre la red de ahora.
    ///
    /// ```
    /// use norte_frontend::tasks::Rate;
    /// # use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
    /// # fn p(bytes: u64) -> TaskProgress {
    /// #     TaskProgress { task_id: TaskId::new(1), kind: TaskKind::Copy,
    /// #         state: TaskState::Running, bytes_done: bytes, bytes_total: Some(1000),
    /// #         entries_done: 0, entries_total: None, current: None,
    /// #         unreadable: None, unvisited: None }
    /// # }
    /// let mut r = Rate::default();
    /// assert_eq!(r.observe(&p(0), 0), None, "con una sola foto no hay ritmo");
    /// assert_eq!(r.observe(&p(100), 1000), Some(100.0), "100 B en un segundo");
    /// ```
    pub fn observe(&mut self, p: &norte_proto::TaskProgress, now_ms: i64) -> Option<f64> {
        let hechos = p.bytes_done;
        // La primera foto solo deja la base: sin dos no hay velocidad.
        let (antes, cuando) = self.ultimo.replace((hechos, now_ms))?;
        let dt = now_ms - cuando;
        if dt <= 0 || hechos < antes {
            // Sin tiempo que dividir, o un contador que se reinició: se toma
            // esta foto como la nueva base y se olvida lo estimado.
            self.bps = None;
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "bytes y milisegundos a f64 para una media: la pérdida es \
                      de dígitos que nadie pinta"
        )]
        let muestra = (hechos - antes) as f64 * 1000.0 / dt as f64;
        self.bps = Some(match self.bps {
            Some(previo) => previo.mul_add(1.0 - PESO, muestra * PESO),
            None => muestra,
        });
        self.bps
    }

    /// El ritmo vigente, en bytes por segundo. `None` = todavía no se sabe.
    #[must_use]
    pub fn bps(&self) -> Option<f64> {
        self.bps
    }

    /// Cuánto queda, en segundos, o `None` si no se puede decir.
    ///
    /// Hacen falta las dos cosas: un total (una copia sabe cuánto pesa; un
    /// borrado no) y un ritmo. Sin alguna, «queda un rato» es lo único cierto
    /// y se dice callando, no con un número inventado.
    ///
    /// ```
    /// use norte_frontend::tasks::Rate;
    /// # use norte_proto::{TaskId, TaskKind, TaskProgress, TaskState};
    /// # fn p(bytes: u64, total: Option<u64>) -> TaskProgress {
    /// #     TaskProgress { task_id: TaskId::new(1), kind: TaskKind::Copy,
    /// #         state: TaskState::Running, bytes_done: bytes, bytes_total: total,
    /// #         entries_done: 0, entries_total: None, current: None,
    /// #         unreadable: None, unvisited: None }
    /// # }
    /// let mut r = Rate::default();
    /// r.observe(&p(0, Some(1000)), 0);
    /// r.observe(&p(100, Some(1000)), 1000);
    /// assert_eq!(r.eta_secs(&p(100, Some(1000))), Some(9), "900 B a 100 B/s");
    /// assert_eq!(r.eta_secs(&p(100, None)), None, "sin total no hay cuenta atrás");
    /// ```
    #[must_use]
    pub fn eta_secs(&self, p: &norte_proto::TaskProgress) -> Option<u64> {
        let total = p.bytes_total?;
        let bps = self.bps?;
        if bps <= 0.0 || total <= p.bytes_done {
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "segundos que se pintan redondeados hacia arriba"
        )]
        let segundos = ((total - p.bytes_done) as f64 / bps).ceil() as u64;
        Some(segundos)
    }
}

/// El ritmo, escrito para una fila: `1.2 MiB/s`. Vacío si no se sabe.
///
/// ```
/// use norte_frontend::tasks::human_rate;
/// assert_eq!(human_rate(Some(1024.0)), "1.0 KiB/s");
/// assert_eq!(human_rate(None), "");
/// ```
#[must_use]
pub fn human_rate(bps: Option<f64>) -> String {
    let Some(bps) = bps else {
        return String::new();
    };
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "un ritmo negativo no existe y uno mayor que u64 tampoco"
    )]
    let bytes = bps.max(0.0) as u64;
    format!("{}/s", crate::human_bytes(bytes))
}

/// Lo que queda, escrito para una fila: `9s`, `1m 20s`, `2h 05m`. Vacío si no
/// se sabe.
///
/// Sin Fluent a propósito: son símbolos de unidad, como los de `human_bytes`,
/// y una cuenta atrás que cambia de idioma cada segundo no se lee mejor.
///
/// ```
/// use norte_frontend::tasks::human_eta;
/// assert_eq!(human_eta(Some(9)), "9s");
/// assert_eq!(human_eta(Some(80)), "1m 20s");
/// assert_eq!(human_eta(Some(7500)), "2h 05m");
/// assert_eq!(human_eta(None), "");
/// ```
#[must_use]
pub fn human_eta(secs: Option<u64>) -> String {
    let Some(s) = secs else {
        return String::new();
    };
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// `true` si esta clase de task es TRABAJO de tablero.
///
/// El panel de procesos es de lo que el lector puso a correr y puede parar:
/// copias, movimientos, borrados, empaquetados, una sincronización. Las
/// clases *observacionales* —buscar, comparar, medir un directorio, sumar,
/// indexar, planear una sincronización— no lo son: cada una tiene su propia
/// superficie (la lista de hallazgos, la ficha de sumas, el plan), y ahí es
/// donde se ven y se cancelan.
///
/// La distinción existe porque el panel puede ABRIRSE SOLO (ADR 0115), y
/// abrirlo por una búsqueda tapa media pantalla para decir lo que la lista de
/// hallazgos ya está diciendo. La TUI nunca metió esas clases en su tablero
/// —viven en `work.search`, `work.checksum`—, así que sin esta regla escrita
/// los dos frontends contaban cosas distintas: es la clase de divergencia que
/// el ADR 0077 pide matar en la REGLA, no en cada cableado.
///
/// ```
/// use norte_frontend::tasks::counts_as_work;
/// use norte_proto::TaskKind;
///
/// assert!(counts_as_work(TaskKind::Copy));
/// assert!(!counts_as_work(TaskKind::Search));
/// ```
#[must_use]
pub fn counts_as_work(kind: norte_proto::TaskKind) -> bool {
    !matches!(
        kind,
        norte_proto::TaskKind::Search
            | norte_proto::TaskKind::Compare
            | norte_proto::TaskKind::DirSize
            | norte_proto::TaskKind::Checksum
            | norte_proto::TaskKind::Index
            | norte_proto::TaskKind::Embed
            | norte_proto::TaskKind::SyncPlan
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cada clase del wire decide, A MANO, si es trabajo de tablero.
    ///
    /// El `match` de [`counts_as_work`] es por exclusión, así que una clase
    /// NUEVA cuenta como trabajo sin que nadie lo piense — que es el valor
    /// por defecto correcto (una clase nueva suele mutar, y un panel que no
    /// se abre nunca es una función que no existe), pero no puede ser una
    /// decisión silenciosa. Este `match` sin brazo comodín rompe la
    /// compilación del test en cuanto el enum crece, y obliga a elegir.
    #[test]
    fn cada_clase_decide_a_mano_si_es_trabajo() {
        use norte_proto::TaskKind as K;
        for kind in [
            K::Copy,
            K::Move,
            K::Delete,
            K::Undo,
            K::Search,
            K::Mkdir,
            K::Create,
            K::Index,
            K::Embed,
            K::RenameBatch,
            K::Compare,
            K::DirSize,
            K::Checksum,
            K::SetMode,
            K::Pack,
            K::TestArchive,
            K::Split,
            K::Combine,
            K::SyncPlan,
            K::Sync,
        ] {
            let esperado = match kind {
                // TRABAJO: mueve bytes o cambia el disco, y se para desde el
                // panel.
                K::Copy
                | K::Move
                | K::Delete
                | K::Undo
                | K::Mkdir
                | K::Create
                | K::RenameBatch
                | K::SetMode
                | K::Pack
                | K::TestArchive
                | K::Split
                | K::Combine
                | K::Sync => true,
                // OBSERVACIÓN: cada una tiene su propia superficie —la lista
                // de hallazgos, la ficha de sumas, el plan—, y taparla con el
                // panel sería decir dos veces lo mismo.
                K::Search
                | K::Compare
                | K::DirSize
                | K::Checksum
                | K::Index
                | K::Embed
                | K::SyncPlan => false,
                // Sin comodín A PROPÓSITO: ver el rustdoc de este test.
                otra => panic!(
                    "clase nueva en el wire ({otra:?}): decide aquí si abre \
                     el panel de procesos, y escríbelo en `counts_as_work`"
                ),
            };
            assert_eq!(
                counts_as_work(kind),
                esperado,
                "{kind:?} cambió de lado sin que nadie lo dijera"
            );
        }
    }

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
            unreadable: None,
            unvisited: None,
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
