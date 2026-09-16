//! Qué fila del panel de procesos está elegida, y nada más.
//!
//! Las filas son las del tablero de tasks que ya pinta la franja: el panel no
//! guarda una segunda copia, porque dos listas de tareas se separan y la que
//! se ve deja de ser la que se cancela.
//!
//! **No hay pausa.** El protocolo tiene `task.cancel` y no tiene otra cosa, y
//! un control que no hace lo que dice es peor que un control que falta.
//!
//! Vive aquí y no en un frontend porque lo que hay dentro no es de pintar: es
//! la respuesta a «¿qué tarea se pararía?», y esa pregunta tiene que tener una
//! sola respuesta en el terminal y en la ventana (ADR 0077). Escrita dos veces
//! ya empezaba a separarse.
//!
//! # Una posición no nombra una tarea
//!
//! El tablero se mueve solo: una task terminada se barre a los diez segundos,
//! y una ajena que no cabe se desaloja por delante. Guardar «la fila 1» y
//! acotarla al leer evita el pánico de índice, pero no evita lo peor: si la
//! que se va estaba ENCIMA, la fila 1 pasa a nombrar otra tarea sin que el
//! lector toque nada, y la tecla de cancelar para una copia que nadie eligió.
//!
//! Por eso lo que se guarda es la IDENTIDAD de la task elegida, y la posición
//! solo es el respaldo para cuando esa identidad ya no está. Es la misma
//! distinción que el listado hace entre una `RowKey` y un índice.

/// El porcentaje que le toca a una FILA del listado, si alguna tarea está
/// trabajando sobre ella (spec 2026-09-15, fase 2).
///
/// Lo que se pasa son los operandos de las tareas vivas con su porcentaje —
/// el tablero lo guarda cada frontend en un tipo suyo, y esta función no
/// necesita conocerlo—. La coincidencia es por ruta EXACTA, byte a byte: una
/// copia que trabaja dentro de un directorio no pinta el directorio a medias,
/// porque «la mitad de esta carpeta» no es lo que el número dice.
///
/// Con dos tareas sobre la misma fila manda la MENOS avanzada: lo que falta
/// para que esa fila esté tranquila es lo que falte a la más atrasada.
///
/// ```
/// use norte_frontend::processes::progress_for;
/// use norte_proto::VPath;
/// let vp = |s: &str| VPath::parse(s).unwrap();
/// let tareas = [(vp("mem:///a"), Some(30_u8)), (vp("mem:///a"), Some(70)), (vp("mem:///b"), None)];
/// let iter = || tareas.iter().map(|(p, pct)| (p, *pct));
/// assert_eq!(progress_for(iter(), &vp("mem:///a")), Some(30), "manda la más atrasada");
/// assert_eq!(progress_for(iter(), &vp("mem:///b")), None, "sin porcentaje, nada que pintar");
/// assert_eq!(progress_for(iter(), &vp("mem:///c")), None);
/// ```
#[must_use]
pub fn progress_for<'a>(
    tareas: impl IntoIterator<Item = (&'a norte_proto::VPath, Option<u8>)>,
    fila: &norte_proto::VPath,
) -> Option<u8> {
    tareas
        .into_iter()
        .filter(|(ruta, _)| *ruta == fila)
        .filter_map(|(_, pct)| pct)
        .min()
}

/// Qué tarea tiene el cursor del panel de procesos.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Processes {
    /// Dónde estaba, para cuando la elegida ya no esté.
    cursor: usize,
    /// Cuál está elegida. `None` antes de mover nada.
    anclada: Option<u64>,
}

impl Processes {
    /// La fila elegida sobre `ids`, o `None` si el tablero está vacío.
    ///
    /// `ids` son las tasks PINTADAS, en el orden en que se pintan. Manda la
    /// identidad: mientras la elegida siga en el tablero, la fila es la suya
    /// esté donde esté. Cuando ya no está, se cae a la última posición
    /// conocida, acotada — que es lo que hace que barrer la última deje la
    /// selección en la que ahora es la última, y no arriba del todo.
    ///
    /// ```
    /// use norte_frontend::processes::Processes;
    /// let mut p = Processes::default();
    /// p.mover(1, &[10, 11, 12]);
    /// assert_eq!(p.fila(&[10, 11, 12]), Some(1));
    /// // Caduca la 10, que estaba ENCIMA: la elegida sigue siendo la 11.
    /// assert_eq!(p.fila(&[11, 12]), Some(0));
    /// // Y si se va la elegida, manda la posición acotada.
    /// assert_eq!(p.fila(&[12]), Some(0));
    /// assert_eq!(p.fila(&[]), None);
    /// ```
    #[must_use]
    pub fn fila(&self, ids: &[u64]) -> Option<usize> {
        if ids.is_empty() {
            return None;
        }
        if let Some(anclada) = self.anclada
            && let Some(i) = ids.iter().position(|id| *id == anclada)
        {
            return Some(i);
        }
        Some(self.cursor.min(ids.len() - 1))
    }

    /// Lo mismo, pero `0` con el tablero vacío, para indexar sin una rama.
    ///
    /// La diferencia con [`Self::fila`] no es cosmética, y por eso son dos: lo
    /// que cruza el puente es la opcional, porque un índice sin fila detrás
    /// pinta un resalte sobre la nada.
    ///
    /// ```
    /// use norte_frontend::processes::Processes;
    /// let p = Processes::default();
    /// assert_eq!(p.fila_o_cero(&[]), 0);
    /// assert_eq!(p.fila(&[]), None);
    /// ```
    #[must_use]
    pub fn fila_o_cero(&self, ids: &[u64]) -> usize {
        self.fila(ids).unwrap_or(0)
    }

    /// Sube una fila.
    ///
    /// ```
    /// use norte_frontend::processes::Processes;
    /// let mut p = Processes::default();
    /// p.mover(2, &[10, 11, 12]);
    /// p.up(&[10, 11, 12]);
    /// assert_eq!(p.fila(&[10, 11, 12]), Some(1));
    /// ```
    pub fn up(&mut self, ids: &[u64]) {
        self.mover(-1, ids);
    }

    /// Baja una fila, sin pasarse de la última.
    ///
    /// ```
    /// use norte_frontend::processes::Processes;
    /// let mut p = Processes::default();
    /// p.down(&[10, 11]);
    /// p.down(&[10, 11]);
    /// p.down(&[10, 11]);
    /// assert_eq!(p.fila(&[10, 11]), Some(1), "no se sale por abajo");
    /// ```
    pub fn down(&mut self, ids: &[u64]) {
        self.mover(1, ids);
    }

    /// Mueve `delta` filas de una vez, sin salirse por ninguna punta.
    ///
    /// Con el tablero VACÍO no toca nada: el panel se abre con el sistema en
    /// reposo y las teclas siguen llegando, y borrar ahí lo recordado haría
    /// que abrir el panel un momento sin tareas perdiera la elección.
    ///
    /// ```
    /// use norte_frontend::processes::Processes;
    /// let mut p = Processes::default();
    /// p.mover(10, &[10, 11, 12, 13]);
    /// assert_eq!(p.fila(&[10, 11, 12, 13]), Some(3));
    /// p.mover(3, &[]);
    /// assert_eq!(p.fila(&[10, 11, 12, 13]), Some(3), "sin filas no se olvida");
    /// p.mover(-10, &[10, 11, 12, 13]);
    /// assert_eq!(p.fila(&[10, 11, 12, 13]), Some(0));
    /// ```
    pub fn mover(&mut self, delta: i64, ids: &[u64]) {
        let Some(actual) = self.fila(ids) else {
            return;
        };
        let actual = i64::try_from(actual).unwrap_or(i64::MAX);
        let ultimo = i64::try_from(ids.len() - 1).unwrap_or(i64::MAX);
        let destino = actual.saturating_add(delta).clamp(0, ultimo);
        self.cursor = usize::try_from(destino).unwrap_or(0);
        self.anclada = ids.get(self.cursor).copied();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El caso que la posición sola no sabe contestar: la que se va estaba
    /// ENCIMA de la elegida. Con un índice acotado, la fila 1 pasaba a nombrar
    /// otra tarea sin que nadie tocara nada, y cancelar paraba una copia que
    /// el lector no había elegido.
    #[test]
    fn caducar_una_fila_de_arriba_no_cambia_la_tarea_elegida() {
        let mut p = Processes::default();
        p.mover(1, &[10, 11, 12, 13]);
        assert_eq!(p.fila(&[10, 11, 12, 13]), Some(1));
        assert_eq!(
            p.fila(&[11, 12, 13]),
            Some(0),
            "la 11 sigue siendo la 11, ahora en otra fila"
        );
        assert_eq!(
            p.fila(&[12, 13]),
            Some(1),
            "sin la 11 manda la posición recordada, que era la 1"
        );
    }

    #[test]
    fn el_cursor_no_se_sale_por_abajo() {
        let mut p = Processes::default();
        for _ in 0..3 {
            p.down(&[10, 11]);
        }
        assert_eq!(p.fila(&[10, 11]), Some(1));
    }

    #[test]
    fn el_cursor_no_se_sale_por_arriba() {
        let mut p = Processes::default();
        p.up(&[10, 11, 12]);
        assert_eq!(p.fila(&[10, 11, 12]), Some(0));
    }

    /// Sin filas no hay fila que valga, y esto es lo que evita el pánico de
    /// índice en un panel abierto con el sistema en reposo.
    #[test]
    fn sin_filas_no_hay_fila() {
        let p = Processes::default();
        assert_eq!(p.fila(&[]), None);
        assert_eq!(p.fila_o_cero(&[]), 0);
    }

    /// La posición se RECUERDA cuando la elegida ya no está: si el tablero
    /// encoge y vuelve a crecer, la fila vuelve a donde estaba en vez de
    /// haberse quedado pegada arriba.
    #[test]
    fn encogerse_no_borra_donde_estaba_la_fila() {
        let mut p = Processes::default();
        let todas = [10, 11, 12, 13, 14, 15];
        p.mover(4, &todas);
        assert_eq!(p.fila(&todas), Some(4));
        assert_eq!(p.fila(&[10, 11]), Some(1), "acotado mientras hay dos filas");
        assert_eq!(p.fila(&todas), Some(4), "y vuelve cuando vuelve a caber");
    }

    /// Subir después de que el tablero encoja mueve DE VERDAD.
    ///
    /// Restando sobre el número guardado —que es lo que las dos superficies
    /// hacían— la tecla era muda: con el cursor en la 8 y tres filas, subir lo
    /// dejaba en 7 y se seguía leyendo la 2.
    #[test]
    fn subir_con_el_tablero_encogido_mueve_una_fila_de_las_que_se_ven() {
        let mut p = Processes::default();
        let todas: Vec<u64> = (0..10).collect();
        p.mover(8, &todas);
        // Se van las de arriba Y la elegida: manda la posición, acotada.
        let quedan = [100, 101, 102];
        assert_eq!(p.fila(&quedan), Some(2));
        p.up(&quedan);
        assert_eq!(p.fila(&quedan), Some(1), "sube desde ahí, no desde el 8");
    }

    /// Mover con el tablero vacío no es un caso raro, y no puede perder la
    /// elección: el panel sigue abierto y las teclas siguen llegando.
    #[test]
    fn mover_sin_filas_no_olvida_lo_elegido() {
        let mut p = Processes::default();
        p.mover(2, &[10, 11, 12]);
        p.mover(3, &[]);
        p.up(&[]);
        assert_eq!(p.fila(&[10, 11, 12]), Some(2));
    }
}
