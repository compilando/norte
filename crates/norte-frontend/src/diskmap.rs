//! El estado del mapa de disco (fase 4), compartido por las dos superficies.
//!
//! Aquí no se pinta nada: el reparto en rectángulos es [`crate::treemap`], y
//! quién lo dibuja es cada frontend. Esto es lo que los DOS necesitan saber —
//! qué directorio se está enseñando, qué se ha medido, cuál es el hijo elegido
//! y si la medida sigue en marcha— y vive junto por lo de siempre: una
//! decisión escrita dos veces diverge en silencio (ADR 0077).
//!
//! # El hijo elegido se recuerda por NOMBRE
//! Un mapa se vuelve a medir: al refrescar, al volver de un cambio externo, al
//! entrar y salir. Si el elegido fuera un índice, una medida que ya no trae al
//! hijo de arriba movería la selección a otro fichero sin que nadie tocara una
//! tecla — y en un mapa la tecla siguiente ENTRA en lo elegido. El nombre lo
//! identifica; la posición solo lo encuentra.

use norte_proto::methods::{DirUsageChild, FsDirUsageReportResult};
use norte_proto::{Segment, TaskId, VPath};

/// En qué punto está la medida de este mapa.
///
/// Cuatro estados y no un `Option`, porque «no se ha pedido», «se está
/// midiendo» y «se pidió y falló» se enseñan distinto: un panel que
/// colapsara los tres diría «vacío» sobre un directorio que nadie ha mirado y
/// sobre uno al que se denegó el permiso.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Estado {
    /// Nadie ha pedido nada todavía.
    #[default]
    Quieto,
    /// Hay una medida en marcha; se puede cancelar.
    Midiendo(TaskId),
    /// Terminó y lo medido está en el informe.
    Hecho,
    /// Falló, y esto es el motivo ya traducido para enseñarlo.
    Fallo(String),
}

/// Lo que el mapa de disco de un hueco sabe ahora mismo.
#[derive(Debug, Default)]
pub struct DiskMap {
    /// Qué directorio se está describiendo. `None` = ninguno todavía.
    dir: Option<VPath>,
    /// Lo medido. Vacío mientras no aterrice nada.
    informe: FsDirUsageReportResult,
    /// El nombre del hijo elegido, si hay alguno.
    elegido: Option<Segment>,
    /// En qué punto está la medida.
    estado: Estado,
}

impl DiskMap {
    /// Un mapa vacío.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// El directorio que se está describiendo.
    #[must_use]
    pub fn dir(&self) -> Option<&VPath> {
        self.dir.as_ref()
    }

    /// Lo medido hasta ahora.
    #[must_use]
    pub fn informe(&self) -> &FsDirUsageReportResult {
        &self.informe
    }

    /// En qué punto está la medida.
    #[must_use]
    pub fn estado(&self) -> &Estado {
        &self.estado
    }

    /// Apunta a otro directorio: se olvida lo medido y la elección.
    ///
    /// Lo medido es DE un directorio, así que conservarlo al cambiar pintaría
    /// el mapa del anterior bajo el título del nuevo — durante el rato que
    /// tarde la medida, que es justo el rato en que alguien lo mira.
    pub fn apuntar(&mut self, dir: VPath) {
        self.dir = Some(dir);
        self.informe = FsDirUsageReportResult::default();
        self.elegido = None;
        self.estado = Estado::Quieto;
    }

    /// Dice que hay una medida en marcha.
    pub fn midiendo(&mut self, task: TaskId) {
        self.estado = Estado::Midiendo(task);
    }

    /// La task de la medida en marcha, si la hay (para cancelarla).
    #[must_use]
    pub fn task(&self) -> Option<TaskId> {
        match self.estado {
            Estado::Midiendo(id) => Some(id),
            _ => None,
        }
    }

    /// Dice por qué no se pudo medir.
    pub fn fallo(&mut self, motivo: String) {
        self.estado = Estado::Fallo(motivo);
    }

    /// Aterriza un informe —parcial o definitivo— sobre este mapa.
    ///
    /// **La elección se conserva por nombre**, y se suelta solo si ese hijo ya
    /// no está. Un informe parcial llega varias veces mientras la medida corre,
    /// y con la elección atada a la posición el cursor iría saltando de
    /// fichero en fichero según fueran llegando los hijos.
    ///
    /// `listo` distingue el último informe de los de en medio: es lo que
    /// decide si esto se puede guardar en la caché.
    pub fn aterrizar(&mut self, informe: FsDirUsageReportResult, listo: bool) {
        let sigue = self
            .elegido
            .as_ref()
            .is_some_and(|n| informe.children.iter().any(|c| c.name == *n));
        if !sigue {
            self.elegido = None;
        }
        self.informe = informe;
        if listo {
            self.estado = Estado::Hecho;
        }
    }

    /// El hijo elegido, si lo hay y sigue estando.
    #[must_use]
    pub fn elegido(&self) -> Option<&DirUsageChild> {
        let n = self.elegido.as_ref()?;
        self.informe.children.iter().find(|c| c.name == *n)
    }

    /// Mueve la elección `delta` posiciones sobre los hijos NOMBRADOS.
    ///
    /// Sin nada elegido, el primer movimiento elige el primero —que es el
    /// mayor, porque el informe llega ordenado por listado pero el mapa se
    /// recorre como se pinta— en vez de no hacer nada: una tecla que no hace
    /// nada la primera vez parece rota.
    ///
    /// Se ACOTA en los extremos y no da la vuelta: en una lista de rectángulos
    /// el borde es una posición legítima donde quedarse, y envolver haría que
    /// bajar desde el último saltara al otro lado de la pantalla.
    pub fn mover(&mut self, delta: isize) {
        if self.informe.children.is_empty() {
            self.elegido = None;
            return;
        }
        let actual = self
            .elegido
            .as_ref()
            .and_then(|n| self.informe.children.iter().position(|c| c.name == *n));
        let nuevo = match actual {
            None => 0,
            Some(i) => {
                let max = self.informe.children.len().saturating_sub(1);
                let cand = isize::try_from(i).unwrap_or(0).saturating_add(delta);
                usize::try_from(cand).unwrap_or(0).min(max)
            }
        };
        self.elegido = self.informe.children.get(nuevo).map(|c| c.name.clone());
    }

    /// Elige un hijo por su nombre —lo que hace un clic en su rectángulo—, y
    /// dice si existía.
    pub fn elegir(&mut self, name: &Segment) -> bool {
        let existe = self.informe.children.iter().any(|c| c.name == *name);
        if existe {
            self.elegido = Some(name.clone());
        }
        existe
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::EntryKind;

    fn seg(s: &str) -> Segment {
        Segment::new(s.as_bytes().to_vec()).expect("segmento")
    }

    fn hijo(name: &str, bytes: u64) -> DirUsageChild {
        DirUsageChild {
            name: seg(name),
            kind: EntryKind::Dir,
            bytes,
            entries: 1,
            partial: false,
        }
    }

    fn informe(nombres: &[&str]) -> FsDirUsageReportResult {
        FsDirUsageReportResult {
            children: nombres.iter().map(|n| hijo(n, 10)).collect(),
            listed: true,
            ..FsDirUsageReportResult::default()
        }
    }

    /// Lo elegido se recuerda por NOMBRE: un informe que ya no trae al vecino
    /// de arriba no mueve la selección a otro fichero.
    ///
    /// Es la diferencia que importa, porque la tecla siguiente ENTRA en lo
    /// elegido: con un índice, medir otra vez podía dejar el cursor sobre un
    /// directorio distinto del que el lector estaba mirando.
    #[test]
    fn lo_elegido_se_recuerda_por_nombre_y_no_por_posicion() {
        let mut m = DiskMap::new();
        m.aterrizar(informe(&["a", "b", "c"]), true);
        assert!(m.elegir(&seg("c")));
        // Vuelve a medir y ya no está `a`: `c` sigue siendo lo elegido aunque
        // ahora esté una posición más arriba.
        m.aterrizar(informe(&["b", "c"]), true);
        assert_eq!(m.elegido().map(|c| c.name.clone()), Some(seg("c")));
    }

    /// Si el elegido desaparece, se suelta: enseñar como elegido algo que ya no
    /// está es prometer una tecla que no puede funcionar.
    #[test]
    fn si_lo_elegido_desaparece_se_suelta() {
        let mut m = DiskMap::new();
        m.aterrizar(informe(&["a", "b"]), true);
        assert!(m.elegir(&seg("a")));
        m.aterrizar(informe(&["b"]), true);
        assert!(m.elegido().is_none());
    }

    /// El primer movimiento elige: una tecla que no hace nada la primera vez
    /// parece rota.
    #[test]
    fn el_primer_movimiento_elige_el_primero() {
        let mut m = DiskMap::new();
        m.aterrizar(informe(&["a", "b"]), true);
        m.mover(1);
        assert_eq!(m.elegido().map(|c| c.name.clone()), Some(seg("a")));
    }

    /// Se ACOTA en los extremos, no da la vuelta.
    #[test]
    fn moverse_se_acota_en_los_bordes() {
        let mut m = DiskMap::new();
        m.aterrizar(informe(&["a", "b", "c"]), true);
        m.elegir(&seg("c"));
        m.mover(1);
        assert_eq!(
            m.elegido().map(|c| c.name.clone()),
            Some(seg("c")),
            "abajo del todo se queda abajo"
        );
        m.elegir(&seg("a"));
        m.mover(-1);
        assert_eq!(m.elegido().map(|c| c.name.clone()), Some(seg("a")));
    }

    /// Apuntar a otro directorio olvida lo medido: el mapa del anterior bajo el
    /// título del nuevo es la respuesta equivocada durante justo el rato en que
    /// alguien lo está mirando.
    #[test]
    fn apuntar_a_otro_directorio_olvida_lo_medido() {
        let mut m = DiskMap::new();
        m.aterrizar(informe(&["a"]), true);
        m.elegir(&seg("a"));
        m.apuntar(VPath::parse("mem:///otro").expect("wire"));
        assert!(m.informe().children.is_empty());
        assert!(m.elegido().is_none());
        assert_eq!(m.estado(), &Estado::Quieto);
    }

    /// Un mapa a medias no se declara hecho: `aterrizar(_, false)` deja el
    /// estado donde estaba para que el panel siga diciendo que mide.
    #[test]
    fn un_informe_parcial_no_declara_la_medida_terminada() {
        let mut m = DiskMap::new();
        m.midiendo(TaskId::new(7));
        m.aterrizar(informe(&["a"]), false);
        assert_eq!(m.estado(), &Estado::Midiendo(TaskId::new(7)));
        assert_eq!(m.task(), Some(TaskId::new(7)));
        m.aterrizar(informe(&["a", "b"]), true);
        assert_eq!(m.estado(), &Estado::Hecho);
        assert!(m.task().is_none(), "terminada ya no hay nada que cancelar");
    }
}
