//! La línea de tiempo del journal (fase 7 del programa WOW): qué se ha
//! hecho en esta máquina, en orden, y hasta dónde se puede volver.
//!
//! El modelo es de los dos frontends. Lo que hay aquí es lo que no puede
//! decidirse dos veces sin que diverja:
//!
//! - **Qué es una FILA.** Un lote (`batch_id`) es una fila, no `n`: se
//!   deshace entero o no se toca, así que ofrecer un corte por la mitad de
//!   uno sería ofrecer algo que no existe.
//! - **Qué significa señalar una.** «Vuelve aquí» conserva la fila señalada
//!   entera, y deshace lo de después. De ahí que el corte sea el `seq` MÁS
//!   NUEVO del grupo y no el más viejo: con el más viejo, el propio lote
//!   señalado se deshacía a medias.
//! - **Cuántas entradas se va a llevar.** Se cuenta ANTES de preguntar,
//!   porque una confirmación que no dice cuánto no es una confirmación.
//!
//! Lo que NO hay aquí: colores, teclas y cómo se pinta un punto. Eso es de
//! cada frontend, y es lo único que de verdad cambia entre un terminal y una
//! ventana.

use norte_proto::methods::JournalRow;

/// La clase de actor del humano, tal y como la escribe el journal.
///
/// Es el valor de `actor_kind` que [`crate::timeline::Timeline`] compara
/// para saber qué filas puede deshacer, y vive aquí —y no como un literal
/// suelto en cada sitio— porque escribirlo mal no rompe nada visiblemente:
/// simplemente hace que el recuento diga cero para siempre.
pub const ACTOR_HUMANO: &str = "user";

/// Una fila de la línea de tiempo: una mutación, o un LOTE entero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRow {
    /// El `seq` más NUEVO del grupo. Es el corte que hay que mandar para
    /// conservar esta fila entera ([`Timeline::corte`]).
    pub seq: i64,
    /// Cuándo ocurrió lo más nuevo del grupo.
    pub ts_ms: i64,
    /// Quién: `"user"`, `"agent"`, `"plugin"`…
    pub actor_kind: String,
    /// Cuál, dentro de esa clase. `None` para el humano.
    pub actor_id: Option<String>,
    /// La operación. Para un lote, la de su entrada más nueva.
    pub op: String,
    /// Sobre qué. Ya pintable; lo enmascara quien construye la fila, que es
    /// quien sabe de dónde vienen esos bytes.
    pub path: String,
    /// El destino, si la operación tiene dos lados.
    pub path_to: Option<String>,
    /// Si TODAS las entradas del grupo declararon vuelta. Un lote con una
    /// irreversible dentro no es reversible: se deshace entero o nada.
    pub reversible: bool,
    /// Si el grupo YA está deshecho (su compensación sigue viva), o ES una
    /// compensación.
    ///
    /// Las dos cosas cuentan igual para lo único que importa aquí: el undo
    /// no las va a tocar. Una compensación se escribe con el actor del
    /// humano que ejecutó el undo y con una reversa de verdad, así que sin
    /// esto una línea de tiempo la contaría como deshacible — y después de
    /// deshacer cinco cosas prometería diez y haría cero.
    pub ya_desecho: bool,
    /// El texto de [`Self::path`] se pinta distinto de lo que dicen los
    /// bytes guardados: el servidor tuvo que enmascarar algo. Se marca en la
    /// fila, como en toda superficie de decisión.
    pub hostile: bool,
    /// Cuántas entradas del journal hay debajo de esta fila. `1` salvo en un
    /// lote.
    pub members: usize,
    /// El lote, si lo es. Sirve para pintarlo distinto: un grupo no se lee
    /// igual que una mutación suelta.
    pub batch_id: Option<i64>,
}

impl TimelineRow {
    /// Si esta fila la hizo el humano, o sea si `journal.undo_after` la va a
    /// mirar siquiera.
    #[must_use]
    pub fn es_del_humano(&self) -> bool {
        self.actor_kind == ACTOR_HUMANO
    }

    /// Si el undo de verdad va a intentar devolver esta fila.
    ///
    /// Son las TRES condiciones que aplica la consulta del core, no dos: del
    /// humano, con vuelta declarada, y ni deshecha ya ni siendo ella misma
    /// una compensación. Contar sólo las dos primeras es lo que hacía que la
    /// confirmación prometiera el doble de lo que iba a pasar.
    #[must_use]
    pub fn se_va_a_deshacer(&self) -> bool {
        self.es_del_humano() && self.reversible && !self.ya_desecho
    }
}

/// Lo que se va a llevar un corte, contado ANTES de preguntar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Corte {
    /// Entradas del HUMANO, reversibles, posteriores al corte: las que el
    /// undo va a intentar devolver.
    pub a_deshacer: usize,
    /// Entradas del humano posteriores al corte que el undo NO va a tocar
    /// porque no tienen vuelta, porque ya están deshechas, o porque son
    /// ellas mismas la compensación de otra.
    ///
    /// Se cuentan aparte y no se suman a [`Self::a_deshacer`] por la razón
    /// de siempre: un número que las mezclara prometería algo que no va a
    /// pasar, y esta cifra es la que se enseña justo antes de preguntar.
    pub irreversibles: usize,
    /// Entradas posteriores al corte que NO son del humano. El undo no las
    /// toca — son de un agente o de un plugin, y se deshacen por su propia
    /// vía—, y por eso se cuentan aparte en vez de sumarse a las otras: un
    /// número que mezclara las tres prometería algo que no va a pasar.
    pub ajenas: usize,
}

impl Corte {
    /// Si un corte aquí no va a hacer nada.
    #[must_use]
    pub fn no_hace_nada(&self) -> bool {
        self.a_deshacer == 0
    }
}

/// La línea de tiempo cargada: las filas que se han traído, en orden de la
/// más nueva a la más vieja, y por dónde anda el cursor.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    rows: Vec<TimelineRow>,
    cursor: usize,
    next_before_seq: Option<i64>,
    cargada: bool,
}

impl Timeline {
    /// Una línea de tiempo con la primera página ya dentro.
    #[must_use]
    pub fn new(rows: &[JournalRow], next_before_seq: Option<i64>) -> Self {
        Self {
            rows: agrupar(rows),
            cursor: 0,
            next_before_seq,
            cargada: true,
        }
    }

    /// Si alguien ha llegado a preguntarle al journal.
    ///
    /// `false` en una recién nacida —la que hereda una disposición guardada,
    /// antes de que el bucle la llene—, y sirve para no decir «todavía no se
    /// ha hecho nada» sobre un journal que no se ha mirado. En una pantalla
    /// de historial, esa frase es la peor equivocación posible.
    #[must_use]
    pub fn cargada(&self) -> bool {
        self.cargada
    }

    /// Añade una página MÁS VIEJA al final.
    ///
    /// Se agrupa la página entera junto con la última fila que ya había, por
    /// si un lote quedó partido entre dos páginas: el journal pagina por
    /// entradas y no sabe de lotes, así que el corte puede caer dentro de
    /// uno. Sin esto, la mitad de un lote se pintaría como un grupo propio y
    /// ofrecería un corte por su medio, que es exactamente lo que no existe.
    pub fn extend(&mut self, rows: &[JournalRow], next_before_seq: Option<i64>) {
        let nuevas = agrupar(rows);
        if let (Some(ultima), Some(primera)) = (self.rows.last(), nuevas.first())
            && ultima.batch_id.is_some()
            && ultima.batch_id == primera.batch_id
        {
            let cola = self.rows.pop().unwrap_or_else(|| unreachable!());
            let mut it = nuevas.into_iter();
            let primera = it.next().unwrap_or_else(|| unreachable!());
            self.rows.push(fundir(cola, &primera));
            self.rows.extend(it);
        } else {
            self.rows.extend(nuevas);
        }
        self.next_before_seq = next_before_seq;
        self.cargada = true;
    }

    /// Las filas, de la más nueva a la más vieja.
    #[must_use]
    pub fn rows(&self) -> &[TimelineRow] {
        &self.rows
    }

    /// Dónde está el cursor.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Mueve el cursor a `i`, acotado.
    pub fn set_cursor(&mut self, i: usize) {
        self.cursor = i.min(self.rows.len().saturating_sub(1));
    }

    /// Sube una fila (hacia lo más nuevo).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja una fila (hacia lo más viejo).
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// La fila bajo el cursor.
    #[must_use]
    pub fn selected(&self) -> Option<&TimelineRow> {
        self.rows.get(self.cursor)
    }

    /// Qué `seq` hay que mandar a `journal.undo_after` para volver al estado
    /// de la fila bajo el cursor, conservándola.
    #[must_use]
    pub fn corte(&self) -> Option<i64> {
        self.selected().map(|r| r.seq)
    }

    /// Qué se va a llevar ese corte, contando las filas MÁS NUEVAS que la
    /// señalada.
    ///
    /// Se cuenta sobre lo cargado, y eso basta SOLO si el undo no pasa de lo
    /// cargado: lo que se hizo después de pintar la lista no está aquí. Por
    /// eso quien pide el undo manda también [`Self::techo`] (`upto_seq`,
    /// 0.80.0), y el core no deshace nada más nuevo. Lo de más abajo del
    /// cursor, que puede no estar cargado, un corte aquí no lo toca.
    #[must_use]
    pub fn resumen(&self) -> Corte {
        let mut c = Corte::default();
        for fila in self.rows.iter().take(self.cursor) {
            if !fila.es_del_humano() {
                c.ajenas += fila.members;
            } else if fila.se_va_a_deshacer() {
                c.a_deshacer += fila.members;
            } else {
                c.irreversibles += fila.members;
            }
        }
        c
    }

    /// El TECHO de un undo desde esta lista: el `seq` más nuevo que se ha
    /// cargado, y por tanto lo más nuevo que [`Self::resumen`] ha podido
    /// contar. Va como `upto_seq` en `journal.undo_after`: sin él, lo que se
    /// hizo después de pintar la lista entraría en el undo sin haberse
    /// contado.
    #[must_use]
    pub fn techo(&self) -> Option<i64> {
        self.rows.first().map(|r| r.seq)
    }

    /// El cursor para pedir la página siguiente (más vieja), o `None` si ya
    /// no queda nada por detrás.
    #[must_use]
    pub fn next_before_seq(&self) -> Option<i64> {
        self.next_before_seq
    }

    /// Si no hay ninguna fila.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Cuántas filas hay.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }
}

/// Junta las entradas del mismo lote en una fila.
///
/// Las entradas llegan de la más nueva a la más vieja, y las de un lote
/// SUELEN ser contiguas en `seq` — pero no tiene por qué: dos tareas de lote
/// concurrentes intercalan sus entradas, y eso lo contempla `alloc_batch` en
/// el core. Por eso se busca el grupo en TODO lo que ya se lleva agrupado y
/// no sólo en la fila anterior: un lote partido en dos filas ofrecería dos
/// cortes por dentro de una unidad que se deshace entera.
///
/// Que eso no sea además PELIGROSO lo garantiza el core, no esto: la
/// consulta de `undo_after` deja fuera el lote completo cuando el corte cae
/// dentro de él. Esta agrupación es lo que hace que la pantalla no ofrezca
/// un corte que el core va a ignorar.
fn agrupar(rows: &[JournalRow]) -> Vec<TimelineRow> {
    let mut out: Vec<TimelineRow> = Vec::with_capacity(rows.len());
    for r in rows {
        let ya = r
            .batch_id
            .and_then(|b| out.iter_mut().find(|u| u.batch_id == Some(b)));
        if let Some(u) = ya {
            u.members += 1;
            // Un lote es reversible sólo si lo son TODAS sus entradas: se
            // deshace entero o no se toca. Y basta con que una de ellas esté
            // ya deshecha para que el undo no lo vaya a tocar.
            u.reversible = u.reversible && r.reversible;
            u.ya_desecho = u.ya_desecho || r.undone || r.undoes_seq.is_some();
            continue;
        }
        out.push(TimelineRow {
            seq: r.seq,
            ts_ms: r.ts_ms,
            actor_kind: r.actor_kind.clone(),
            actor_id: r.actor_id.clone(),
            op: r.op.clone(),
            path: r.path.clone(),
            path_to: r.path_to.clone(),
            reversible: r.reversible,
            // Una compensación es una mutación que ocurrió y se enseña, pero
            // el undo no la vuelve a deshacer: cuenta como ya desecha.
            ya_desecho: r.undone || r.undoes_seq.is_some(),
            hostile: r.hostile,
            members: 1,
            batch_id: r.batch_id,
        });
    }
    out
}

/// Funde dos trozos del MISMO lote partido entre dos páginas. El `seq` y la
/// operación son los del trozo más NUEVO, que es el que manda para el corte.
fn fundir(nuevo: TimelineRow, viejo: &TimelineRow) -> TimelineRow {
    TimelineRow {
        members: nuevo.members + viejo.members,
        reversible: nuevo.reversible && viejo.reversible,
        ya_desecho: nuevo.ya_desecho || viejo.ya_desecho,
        hostile: nuevo.hostile || viejo.hostile,
        ..nuevo
    }
}

#[cfg(test)]
mod tests {
    use super::{Timeline, agrupar};
    use norte_proto::methods::JournalRow;

    fn fila(seq: i64, actor: &str, reversible: bool, batch: Option<i64>) -> JournalRow {
        JournalRow {
            undoes_seq: None,
            undone: false,
            hostile: false,
            seq,
            ts_ms: 1_000 + seq,
            actor_kind: actor.to_owned(),
            actor_id: None,
            op: "copied".to_owned(),
            path: format!("file:///a/{seq}"),
            path_to: None,
            reversible,
            batch_id: batch,
        }
    }

    /// Un lote es UNA fila: se deshace entero o no se toca, así que una
    /// lista que lo partiera ofrecería un corte que no existe.
    #[test]
    fn un_lote_es_una_fila() {
        let filas = [
            fila(9, "user", true, None),
            fila(8, "user", true, Some(3)),
            fila(7, "user", true, Some(3)),
            fila(6, "user", true, Some(3)),
            fila(5, "user", true, None),
        ];
        let t = Timeline::new(&filas, None);
        assert_eq!(t.len(), 3, "suelta, lote, suelta");
        assert_eq!(t.rows()[1].members, 3);
        assert_eq!(t.rows()[1].seq, 8, "el más nuevo del lote manda");
    }

    /// Un lote con una entrada irreversible dentro NO es reversible: se
    /// deshace entero o nada, así que prometer vuelta sería prometer media.
    #[test]
    fn un_lote_con_una_irreversible_no_es_reversible() {
        let filas = [
            fila(3, "user", true, Some(1)),
            fila(2, "user", false, Some(1)),
        ];
        let t = Timeline::new(&filas, None);
        assert_eq!(t.len(), 1);
        assert!(!t.rows()[0].reversible);
    }

    /// El corte conserva la fila señalada ENTERA: es el `seq` más nuevo del
    /// grupo. Con el más viejo, señalar un lote lo deshacía a medias.
    #[test]
    fn el_corte_conserva_el_lote_senalado_entero() {
        let filas = [
            fila(9, "user", true, None),
            fila(8, "user", true, Some(3)),
            fila(7, "user", true, Some(3)),
        ];
        let mut t = Timeline::new(&filas, None);
        t.down();
        assert_eq!(t.corte(), Some(8), "el más nuevo del lote señalado");
    }

    /// El recuento previo separa lo que se va a deshacer de lo que se va a
    /// SALTAR y de lo que no es del humano. Un número que los mezclara
    /// prometería algo que el undo no va a hacer.
    #[test]
    fn el_recuento_separa_lo_que_de_verdad_se_deshace() {
        let filas = [
            fila(10, "user", true, None),
            fila(9, "agent", true, None),
            fila(8, "user", false, None),
            fila(7, "user", true, Some(2)),
            fila(6, "user", true, Some(2)),
            fila(5, "user", true, None),
        ];
        let mut t = Timeline::new(&filas, None);
        // Cursor en la última (la más vieja): todo lo de arriba entra.
        t.set_cursor(99);
        let c = t.resumen();
        assert_eq!(c.a_deshacer, 3, "la suelta de arriba y las dos del lote");
        assert_eq!(c.irreversibles, 1);
        assert_eq!(c.ajenas, 1, "la del agente no la toca este undo");
    }

    /// **Una compensación no se cuenta como deshacible, ni una entrada ya
    /// deshecha.**
    ///
    /// Es el fallo que encontró la revisión de protocolo: una compensación
    /// se escribe con el actor del HUMANO que ejecutó el undo y con una
    /// reversa de verdad, así que mirando sólo `actor_kind` y `reversible`
    /// pasa por deshacible. Deshaces cinco cosas, recargas, señalas el mismo
    /// corte: el diálogo prometía diez y el undo hacía cero.
    #[test]
    fn ni_una_compensacion_ni_lo_ya_deshecho_cuentan() {
        let mut deshecha = fila(4, "user", true, None);
        deshecha.undone = true;
        let mut compensacion = fila(5, "user", true, None);
        compensacion.undoes_seq = Some(4);

        let filas = [compensacion, deshecha, fila(3, "user", true, None)];
        let mut t = Timeline::new(&filas, None);
        t.set_cursor(99);
        let c = t.resumen();

        assert_eq!(c.a_deshacer, 0, "las dos de arriba ya están resueltas");
        assert_eq!(c.irreversibles, 2, "y se cuentan como «no se van a tocar»");
        assert!(c.no_hace_nada());
    }

    /// Un lote se agrupa aunque sus entradas NO sean contiguas: dos tareas
    /// de lote concurrentes intercalan sus `seq`, y una lista que lo partiera
    /// ofrecería dos cortes por dentro de una unidad que se deshace entera.
    #[test]
    fn un_lote_con_seqs_intercalados_sigue_siendo_una_fila() {
        let filas = [
            fila(9, "user", true, Some(1)),
            fila(8, "user", true, Some(2)),
            fila(7, "user", true, Some(1)),
        ];
        let t = Timeline::new(&filas, None);
        assert_eq!(t.len(), 2, "dos lotes, no tres filas");
        assert_eq!(t.rows()[0].members, 2, "el lote 1 junta 9 y 7");
    }

    /// Con el cursor en la fila más nueva no hay nada por encima, así que el
    /// corte no hace nada — y la pantalla puede decirlo antes de preguntar.
    #[test]
    fn un_corte_en_lo_mas_nuevo_no_hace_nada() {
        let filas = [fila(2, "user", true, None), fila(1, "user", true, None)];
        let t = Timeline::new(&filas, None);
        assert!(t.resumen().no_hace_nada());
    }

    /// Un lote partido entre dos páginas se vuelve a juntar: el journal
    /// pagina por entradas y no sabe de lotes, así que el corte de página
    /// puede caer dentro de uno.
    #[test]
    fn un_lote_partido_entre_paginas_se_junta() {
        let primera = [fila(9, "user", true, None), fila(8, "user", true, Some(3))];
        let segunda = [fila(7, "user", true, Some(3)), fila(6, "user", true, None)];
        let mut t = Timeline::new(&primera, Some(8));
        t.extend(&segunda, None);

        assert_eq!(t.len(), 3, "suelta, lote entero, suelta");
        assert_eq!(t.rows()[1].members, 2);
        assert_eq!(t.rows()[1].seq, 8, "sigue mandando el más nuevo");
    }

    /// Y dos lotes DISTINTOS pegados no se funden por estar al lado.
    #[test]
    fn dos_lotes_distintos_no_se_funden() {
        let filas = [
            fila(4, "user", true, Some(2)),
            fila(3, "user", true, Some(1)),
        ];
        assert_eq!(agrupar(&filas).len(), 2);
    }
}
