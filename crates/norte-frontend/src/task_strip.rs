//! La barra de progreso ligera del item `tasks` de la barra de estado
//! (ADR 0146): CUÁNDO se enseña y QUÉ dice, una vez para los dos frontends
//! (ADR 0077).
//!
//! El trabajo llega a ráfagas: se marca, se pulsa F5, y durante un rato hay
//! una o varias tareas. La barra sigue la ráfaga, no cada tarea:
//!
//! - no aparece hasta que la ráfaga lleva [`UMBRAL_MS`] en marcha: una copia
//!   de un fichero pequeño acaba antes, y pintar una barra 50 ms es un
//!   parpadeo, no una información;
//! - con varias tareas hay UNA barra, la del total;
//! - al acabar deja un «✓» [`HECHO_MS`] —también si la ráfaga fue demasiado
//!   corta para enseñar la barra: si no, una copia rápida no daría ninguna
//!   señal de haber ocurrido— o un «✗» [`FALLO_MS`] si algo falló;
//! - el panel de procesos que se abre solo espera [`PANEL_MS`]: lo que acaba
//!   antes lo cuenta esta barra, sin quitarle un tercio de pantalla al
//!   listado.
//!
//! Todo con el reloj que pasa quien pinta, así que se prueba sin dormir.

use std::collections::BTreeSet;

use norte_i18n::{Lang, t_in, ta_in};
use norte_proto::{TaskKind, TaskProgress, TaskState, VPath};

/// Lo que tarda en aparecer la barra desde que empieza una ráfaga.
pub const UMBRAL_MS: i64 = 400;
/// Lo que se queda el «✓» al acabar bien.
pub const HECHO_MS: i64 = 1_500;
/// Lo que se queda el «✗» al acabar con algún fallo: lo mismo que una fila
/// terminada en el tablero, que es donde se ve cuál y por qué.
pub const FALLO_MS: i64 = 10_000;
/// Lo que espera el panel de procesos automático antes de abrirse.
pub const PANEL_MS: i64 = 2_000;
/// Celdas de la barra en el terminal (la ventana la dibuja a su modo, con el
/// mismo ancho para que el reparto de la barra de estado sea el mismo).
pub const BAR_CELLS: usize = 10;

/// Una tarea del tablero, como la ve la barra.
#[derive(Debug, Clone, Copy)]
pub struct StripTask<'a> {
    /// El último progreso.
    pub progress: &'a TaskProgress,
    /// Sobre qué actúa (el operando pegajoso del tablero).
    pub operand: Option<&'a VPath>,
    /// El ritmo estimado, en bytes por segundo.
    pub bps: Option<f64>,
}

/// En qué momento de la ráfaga está la barra.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripPhase {
    /// Hay trabajo en marcha.
    Running,
    /// Todo el trabajo vivo está PAUSADO (ADR 0147): la barra se queda donde
    /// iba, con ⏸ y sin velocidad, que ahora no hay.
    Paused,
    /// Acabó todo bien.
    Done,
    /// Acabó, y algo falló o se canceló.
    Failed,
}

/// Lo que la barra dice ahora, ya redactado en lo que depende de los datos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripView {
    /// La fase.
    pub phase: StripPhase,
    /// Tareas en marcha (`Running`), hechas (`Done`) o fallidas (`Failed`).
    pub count: usize,
    /// El porcentaje del TOTAL de la ráfaga, si se sabe. `None` no es 0 %.
    pub percent: Option<u8>,
    /// La clase, si todas las de la ráfaga son la misma.
    pub kind: Option<TaskKind>,
    /// El nombre del operando, ya enmascarado, si la ráfaga es de UNA tarea.
    pub name: Option<String>,
    /// El ritmo sumado, ya escrito, o vacío.
    pub rate: String,
    /// Lo que queda, ya escrito, o vacío.
    pub eta: String,
}

/// Una ráfaga de trabajo en curso.
#[derive(Debug, Clone, Default)]
struct Rafaga {
    desde_ms: i64,
    /// Las tareas que han pertenecido a la ráfaga.
    ids: BTreeSet<u64>,
    /// Las que ya se contaron al terminar.
    contadas: BTreeSet<u64>,
    hechas: usize,
    fallidas: usize,
    /// La clase común, mientras lo sea.
    kind: Option<TaskKind>,
    /// Más de una clase distinta.
    mezcla: bool,
    /// El último nombre visto, para el «✓ copiado x» de una sola tarea.
    nombre: Option<String>,
}

/// El desenlace de la última ráfaga, mientras se enseña.
#[derive(Debug, Clone)]
struct Final {
    hasta_ms: i64,
    view: StripView,
}

/// La máquina de la barra. Una por frontend (por sesión de la ventana).
#[derive(Debug, Clone, Default)]
pub struct TaskStrip {
    rafaga: Option<Rafaga>,
    fin: Option<Final>,
    /// La vista de la ráfaga en curso, calculada en el último `update`.
    corriendo: Option<StripView>,
    /// Las tareas del tablero en el último `update`: una que aparece ya
    /// TERMINADA (empezó y acabó entre dos tics) también es de una ráfaga.
    vistas: BTreeSet<u64>,
}

fn en_marcha(p: &TaskProgress) -> bool {
    !p.state.is_terminal()
}

fn bien(p: &TaskProgress) -> bool {
    matches!(p.state, TaskState::Completed)
}

/// El nombre visible de un operando: su último segmento, enmascarado.
fn nombre(v: &VPath) -> Option<String> {
    v.file_name().map(|s| crate::display_name(s.as_bytes()).0)
}

impl TaskStrip {
    /// Anota el tablero de este instante. Solo mira el TRABAJO
    /// ([`crate::tasks::counts_as_work`]): una búsqueda tiene su lista.
    pub fn update<'a>(&mut self, now_ms: i64, tasks: impl IntoIterator<Item = StripTask<'a>>) {
        let tareas: Vec<StripTask<'a>> = tasks
            .into_iter()
            .filter(|t| crate::tasks::counts_as_work(t.progress.kind))
            .collect();
        let hay = tareas.iter().any(|t| en_marcha(t.progress));
        let nuevas: BTreeSet<u64> = tareas
            .iter()
            .map(|t| t.progress.task_id.get())
            .filter(|id| !self.vistas.contains(id))
            .collect();
        self.vistas = tareas.iter().map(|t| t.progress.task_id.get()).collect();
        if (hay || !nuevas.is_empty()) && self.rafaga.is_none() {
            // Trabajo nuevo: el desenlace anterior deja de ser noticia.
            self.fin = None;
            self.rafaga = Some(Rafaga {
                desde_ms: now_ms,
                ..Rafaga::default()
            });
        }
        let Some(r) = self.rafaga.as_mut() else {
            self.corriendo = None;
            return;
        };
        for t in &tareas {
            let id = t.progress.task_id.get();
            if (en_marcha(t.progress) || nuevas.contains(&id)) && r.ids.insert(id) {
                match r.kind {
                    None if !r.mezcla => r.kind = Some(t.progress.kind),
                    Some(k) if k != t.progress.kind => {
                        r.kind = None;
                        r.mezcla = true;
                    }
                    _ => {}
                }
            }
            if !r.ids.contains(&id) {
                continue;
            }
            if let Some(n) = t.operand.and_then(nombre) {
                r.nombre = Some(n);
            }
            if !en_marcha(t.progress) && r.contadas.insert(id) {
                if bien(t.progress) {
                    r.hechas += 1;
                } else {
                    r.fallidas += 1;
                }
            }
        }
        let de_la_rafaga: Vec<&StripTask<'a>> = tareas
            .iter()
            .filter(|t| r.ids.contains(&t.progress.task_id.get()))
            .collect();
        if hay {
            self.corriendo = Some(vista_en_marcha(r, &de_la_rafaga));
            return;
        }
        // Se acabó la ráfaga: su desenlace. Sin ninguno contado —sus tareas
        // se fueron del tablero sin terminar, como las de un daemon que ya no
        // está— no hay nada que decir, y un «✓ 0 hechas» sería mentira.
        if r.hechas + r.fallidas == 0 {
            self.rafaga = None;
            self.corriendo = None;
            return;
        }
        let (phase, count, dura) = if r.fallidas > 0 {
            (StripPhase::Failed, r.fallidas, FALLO_MS)
        } else {
            (StripPhase::Done, r.hechas, HECHO_MS)
        };
        let view = StripView {
            phase,
            count,
            percent: None,
            kind: r.kind,
            name: if r.ids.len() == 1 {
                r.nombre.clone()
            } else {
                None
            },
            rate: String::new(),
            eta: String::new(),
        };
        self.fin = Some(Final {
            hasta_ms: now_ms.saturating_add(dura),
            view,
        });
        self.rafaga = None;
        self.corriendo = None;
    }

    /// Lo que la barra enseña en `now_ms`, o nada.
    #[must_use]
    pub fn view(&self, now_ms: i64) -> Option<StripView> {
        if let Some(r) = &self.rafaga {
            return (now_ms.saturating_sub(r.desde_ms) >= UMBRAL_MS)
                .then(|| self.corriendo.clone())
                .flatten();
        }
        self.fin
            .as_ref()
            .filter(|f| now_ms < f.hasta_ms)
            .map(|f| f.view.clone())
    }

    /// Si el panel de procesos AUTOMÁTICO debe estar abierto: hay una ráfaga
    /// que ya lleva [`PANEL_MS`].
    #[must_use]
    pub fn wants_panel(&self, now_ms: i64) -> bool {
        self.rafaga
            .as_ref()
            .is_some_and(|r| now_ms.saturating_sub(r.desde_ms) >= PANEL_MS)
    }

    /// El próximo instante en que la barra cambia SIN que llegue progreso:
    /// para quien no pinta a cada tic (la ventana programa un despertar).
    #[must_use]
    pub fn next_change_ms(&self, now_ms: i64) -> Option<i64> {
        if let Some(r) = &self.rafaga {
            return [r.desde_ms + UMBRAL_MS, r.desde_ms + PANEL_MS]
                .into_iter()
                .find(|&t| t > now_ms);
        }
        self.fin
            .as_ref()
            .map(|f| f.hasta_ms)
            .filter(|&t| t > now_ms)
    }
}

/// La vista de una ráfaga en marcha, sobre las tareas que siguen en el
/// tablero (terminadas incluidas: su total final cuenta en el porcentaje, o
/// este retrocedería cada vez que una acaba).
fn vista_en_marcha(rafaga: &Rafaga, tareas: &[&StripTask<'_>]) -> StripView {
    let vivas: Vec<&&StripTask<'_>> = tareas
        .iter()
        .filter(|tarea| en_marcha(tarea.progress))
        .collect();
    let suma = |campo: fn(&TaskProgress) -> Option<(u64, u64)>| -> Option<(u64, u64)> {
        tareas
            .iter()
            .try_fold((0u64, 0u64), |(hecho, total), tarea| {
                let (mas, de) = campo(tarea.progress)?;
                Some((hecho.saturating_add(mas), total.saturating_add(de)))
            })
    };
    let bytes = suma(|p| p.bytes_total.map(|tot| (p.bytes_done.min(tot), tot)));
    let entradas = suma(|p| p.entries_total.map(|tot| (p.entries_done.min(tot), tot)));
    let pct = |(hecho, total): (u64, u64)| -> Option<u8> {
        (total > 0).then(|| u8::try_from(hecho.saturating_mul(100) / total).unwrap_or(100))
    };
    let percent = bytes
        .filter(|&(_, t)| t > 0)
        .and_then(pct)
        .or_else(|| entradas.and_then(pct));
    let ritmos: Vec<f64> = vivas.iter().filter_map(|tarea| tarea.bps).collect();
    let bps = (!ritmos.is_empty()).then(|| ritmos.iter().sum::<f64>());
    let eta = match (bytes, bps) {
        (Some((hecho, total)), Some(ritmo)) if ritmo > 0.0 && total > hecho => {
            // Precisión de segundos: un f64 con los bytes de un disco entero
            // sigue siendo exacto de sobra para una estimación.
            #[allow(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss
            )]
            let s = ((total - hecho) as f64 / ritmo).ceil() as u64;
            Some(s)
        }
        _ => None,
    };
    let una = vivas.len() == 1 && rafaga.ids.len() == 1;
    let pausada = !vivas.is_empty()
        && vivas
            .iter()
            .all(|tarea| tarea.progress.state == TaskState::Paused);
    StripView {
        phase: if pausada {
            StripPhase::Paused
        } else {
            StripPhase::Running
        },
        count: vivas.len(),
        percent,
        kind: rafaga.kind,
        name: if una {
            vivas
                .first()
                .and_then(|tarea| tarea.operand)
                .and_then(nombre)
                .or_else(|| rafaga.nombre.clone())
        } else {
            None
        },
        // Parada no tiene ritmo ni hora de acabar: el último medido
        // mentiría sobre ahora.
        rate: if pausada {
            String::new()
        } else {
            crate::tasks::human_rate(bps)
        },
        eta: if pausada {
            String::new()
        } else {
            crate::tasks::human_eta(eta)
        },
    }
}

/// Una forma del item: su texto y si lleva la barra detrás.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    /// El texto.
    pub text: String,
    /// Si detrás va la barra de [`BAR_CELLS`] celdas.
    pub bar: bool,
}

/// Las formas del item, de la más larga a la más corta. La barra de estado
/// usa la más larga que quepa, y solo cuando ni la más corta cabe quita el
/// item (`statusbar::fit`).
#[must_use]
pub fn forms(v: &StripView, lang: Lang) -> Vec<Form> {
    let clase = match v.kind {
        Some(TaskKind::Copy) => "copy",
        Some(TaskKind::Move) => "move",
        Some(TaskKind::Delete) => "delete",
        _ => "other",
    };
    let f = |text: String, bar: bool| Form { text, bar };
    match v.phase {
        StripPhase::Running | StripPhase::Paused => {
            let pausada = v.phase == StripPhase::Paused;
            let icono = if pausada { '⏸' } else { '⟳' };
            let verbo = if pausada {
                t_in(lang, "strip-paused")
            } else {
                t_in(lang, &format!("strip-running-{clase}"))
            };
            let pct = v.percent.map(|p| format!(" {p} %")).unwrap_or_default();
            let cabeza = match &v.name {
                Some(n) if v.count == 1 => format!("{icono} {verbo} {n}"),
                _ => format!("{icono} {}", v.count),
            };
            let corta = match &v.name {
                Some(n) if v.count == 1 => format!("{icono} {n}"),
                _ => format!("{icono} {}", v.count),
            };
            // Lo que se añade al final: el ritmo con una tarea, lo que queda
            // con varias (con varias el ritmo es una suma, y lo que el lector
            // quiere saber es cuándo acaba el lote).
            let cola = if v.count == 1 { &v.rate } else { &v.eta };
            let mut out = Vec::new();
            if !cola.is_empty() {
                out.push(f(format!("{cabeza}{pct} · {cola}"), true));
            }
            out.push(f(format!("{cabeza}{pct}"), true));
            out.push(f(format!("{corta}{pct}"), true));
            out.push(f(format!("{icono} {}{pct}", v.count), true));
            out.push(f(format!("{icono} {}{pct}", v.count), false));
            out.dedup();
            out
        }
        StripPhase::Done => {
            let larga = match &v.name {
                Some(n) => format!("✓ {} {n}", t_in(lang, &format!("strip-done-{clase}"))),
                None => format!(
                    "✓ {}",
                    ta_in(lang, "strip-done-many", &[("n", &v.count.to_string())])
                ),
            };
            vec![f(larga, false), f("✓".to_owned(), false)]
        }
        StripPhase::Failed => vec![
            f(
                format!(
                    "✗ {}",
                    ta_in(lang, "strip-failed", &[("n", &v.count.to_string())])
                ),
                false,
            ),
            f(format!("✗ {}", v.count), false),
        ],
    }
}

/// La barra en celdas de terminal: `▕` + [`BAR_CELLS`] celdas + `▏`, con
/// octavos de bloque para ver moverse una copia lenta. Sin porcentaje, un
/// pulso que recorre la barra con el reloj: «no se sabe cuánto» no es 0 %.
#[must_use]
pub fn bar_glyphs(percent: Option<u8>, now_ms: i64) -> String {
    const OCTAVOS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let mut out = String::with_capacity(BAR_CELLS * 3 + 6);
    out.push('▕');
    if let Some(p) = percent {
        let octavos = usize::from(p.min(100)) * BAR_CELLS * 8 / 100;
        for i in 0..BAR_CELLS {
            let lleno = octavos.saturating_sub(i * 8).min(8);
            out.push(if lleno == 8 { '█' } else { OCTAVOS[lleno] });
        }
    } else {
        // Un paso cada 120 ms, ida y vuelta.
        let paso = usize::try_from(now_ms.max(0) / 120).unwrap_or(0) % (2 * (BAR_CELLS - 1));
        let pos = if paso < BAR_CELLS {
            paso
        } else {
            2 * (BAR_CELLS - 1) - paso
        };
        for i in 0..BAR_CELLS {
            out.push(match i.abs_diff(pos) {
                0 => '▓',
                1 => '▒',
                _ => ' ',
            });
        }
    }
    out.push('▏');
    out
}

/// Celdas que ocupa una [`Form`], con su barra y el espacio que la separa.
#[must_use]
pub fn form_cells(f: &Form) -> usize {
    crate::display::cells(&f.text) + if f.bar { BAR_CELLS + 3 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::TaskId;

    fn p(id: u64, kind: TaskKind, state: TaskState, done: u64, total: Option<u64>) -> TaskProgress {
        TaskProgress {
            task_id: TaskId::new(id),
            kind,
            state,
            bytes_done: done,
            bytes_total: total,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        }
    }

    fn t(p: &TaskProgress) -> StripTask<'_> {
        StripTask {
            progress: p,
            operand: None,
            bps: None,
        }
    }

    const R: TaskState = TaskState::Running;
    const OK: TaskState = TaskState::Completed;

    /// Antes del umbral no hay barra; después, sí.
    #[test]
    fn la_barra_espera_al_umbral() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 10, Some(100));
        s.update(0, [t(&a)]);
        assert_eq!(s.view(0), None);
        s.update(UMBRAL_MS - 1, [t(&a)]);
        assert_eq!(s.view(UMBRAL_MS - 1), None, "aún no");
        s.update(UMBRAL_MS, [t(&a)]);
        let v = s.view(UMBRAL_MS).expect("ya");
        assert_eq!(
            (v.phase, v.count, v.percent),
            (StripPhase::Running, 1, Some(10))
        );
    }

    /// Una copia más corta que el umbral no pinta barra, pero SÍ deja el ✓:
    /// si no, no daría ninguna señal de haber ocurrido.
    #[test]
    fn una_copia_rapida_deja_el_hecho() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 0, Some(100));
        s.update(0, [t(&a)]);
        let a = p(1, TaskKind::Copy, OK, 100, Some(100));
        s.update(100, [t(&a)]);
        let v = s.view(100).expect("el ✓");
        assert_eq!((v.phase, v.count), (StripPhase::Done, 1));
        assert!(s.view(100 + HECHO_MS - 1).is_some(), "se queda");
        assert_eq!(s.view(100 + HECHO_MS), None, "y se va");
        assert_eq!(s.next_change_ms(100), Some(100 + HECHO_MS));
    }

    /// Un fallo se queda más que un éxito.
    #[test]
    fn un_fallo_se_queda_mas() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 0, Some(100));
        let b = p(2, TaskKind::Copy, R, 0, Some(100));
        s.update(0, [t(&a), t(&b)]);
        let a = p(1, TaskKind::Copy, OK, 100, Some(100));
        let b = p(2, TaskKind::Copy, TaskState::Cancelled, 5, Some(100));
        s.update(50, [t(&a), t(&b)]);
        let v = s.view(50).expect("el ✗");
        assert_eq!((v.phase, v.count), (StripPhase::Failed, 1));
        assert!(s.view(50 + HECHO_MS).is_some(), "más que un ✓");
        assert_eq!(s.view(50 + FALLO_MS), None);
    }

    /// Con varias tareas, UNA barra: la del total por bytes, que no
    /// retrocede cuando una acaba.
    #[test]
    fn varias_tareas_una_barra_del_total() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 50, Some(100));
        let b = p(2, TaskKind::Copy, R, 0, Some(300));
        s.update(0, [t(&a), t(&b)]);
        s.update(UMBRAL_MS, [t(&a), t(&b)]);
        let v = s.view(UMBRAL_MS).expect("barra");
        assert_eq!((v.count, v.percent), (2, Some(12)));
        let a = p(1, TaskKind::Copy, OK, 100, Some(100));
        s.update(UMBRAL_MS + 10, [t(&a), t(&b)]);
        let v = s.view(UMBRAL_MS + 10).expect("barra");
        assert_eq!(
            (v.count, v.percent),
            (1, Some(25)),
            "la acabada sigue sumando"
        );
    }

    /// Sin totales no hay porcentaje: `None`, no un 0 fingido.
    #[test]
    fn sin_totales_no_hay_porcentaje() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Delete, R, 0, None);
        s.update(0, [t(&a)]);
        s.update(UMBRAL_MS, [t(&a)]);
        assert_eq!(s.view(UMBRAL_MS).expect("barra").percent, None);
    }

    /// Una búsqueda no es trabajo de barra.
    #[test]
    fn una_busqueda_no_cuenta() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Search, R, 0, None);
        s.update(0, [t(&a)]);
        s.update(UMBRAL_MS * 10, [t(&a)]);
        assert_eq!(s.view(UMBRAL_MS * 10), None);
        assert!(!s.wants_panel(UMBRAL_MS * 10));
    }

    /// El panel automático espera a que la ráfaga dure.
    #[test]
    fn el_panel_espera() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 0, None);
        s.update(0, [t(&a)]);
        assert!(!s.wants_panel(PANEL_MS - 1));
        assert!(s.wants_panel(PANEL_MS));
        assert_eq!(s.next_change_ms(0), Some(UMBRAL_MS));
        assert_eq!(s.next_change_ms(UMBRAL_MS), Some(PANEL_MS));
        assert_eq!(s.next_change_ms(PANEL_MS), None);
    }

    /// Una copia que empieza y acaba entre dos tics nunca se ve en marcha,
    /// y aun así deja su ✓.
    #[test]
    fn la_que_nunca_se_vio_en_marcha_deja_el_hecho() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, OK, 5, Some(5));
        s.update(0, [t(&a)]);
        assert_eq!(s.view(0).map(|v| v.phase), Some(StripPhase::Done));
        // Y no se vuelve a contar en el tic siguiente.
        s.update(HECHO_MS, [t(&a)]);
        assert_eq!(s.view(HECHO_MS), None);
    }

    /// Una ráfaga cuyas tareas desaparecen sin terminar (un relevo del
    /// daemon) se cierra en silencio: ni barra eterna ni «✓ 0».
    #[test]
    fn una_rafaga_que_se_queda_sin_tareas_se_cierra_callada() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, R, 0, Some(10));
        s.update(0, [t(&a)]);
        assert!(s.wants_panel(PANEL_MS));
        s.update(PANEL_MS, []);
        assert_eq!(s.view(PANEL_MS), None);
        assert!(!s.wants_panel(PANEL_MS));
        assert_eq!(s.next_change_ms(PANEL_MS), None);
    }

    /// Trabajo nuevo tapa el desenlace anterior.
    #[test]
    fn trabajo_nuevo_tapa_el_hecho() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, OK, 1, Some(1));
        let a0 = p(1, TaskKind::Copy, R, 0, Some(1));
        s.update(0, [t(&a0)]);
        s.update(10, [t(&a)]);
        assert!(s.view(10).is_some());
        let b = p(2, TaskKind::Copy, R, 0, Some(1));
        s.update(20, [t(&a), t(&b)]);
        assert_eq!(s.view(20), None, "la ráfaga nueva aún no llega al umbral");
    }

    /// El nombre del operando llega enmascarado.
    #[test]
    fn el_nombre_se_enmascara() {
        let mut s = TaskStrip::default();
        let hostil = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("fixture del corpus");
        let seg = norte_proto::Segment::new(hostil.bytes.clone()).expect("segmento");
        let ruta = VPath::parse("file:///d").expect("vpath").join(seg);
        let a = p(1, TaskKind::Copy, R, 0, Some(10));
        let tarea = StripTask {
            progress: &a,
            operand: Some(&ruta),
            bps: None,
        };
        s.update(0, [tarea]);
        s.update(UMBRAL_MS, [tarea]);
        let n = s.view(UMBRAL_MS).and_then(|v| v.name).expect("nombre");
        assert!(!n.chars().any(norte_encoding::is_terminal_hazard), "{n:?}");
    }

    /// Las formas van de más larga a más corta, y la última cabe en poco.
    #[test]
    fn las_formas_se_acortan() {
        let v = StripView {
            phase: StripPhase::Running,
            count: 1,
            percent: Some(62),
            kind: Some(TaskKind::Copy),
            name: Some("foto.jpg".to_owned()),
            rate: "48 MiB/s".to_owned(),
            eta: String::new(),
        };
        let fs = forms(&v, Lang::Es);
        let anchos: Vec<usize> = fs.iter().map(form_cells).collect();
        assert!(anchos.windows(2).all(|w| w[0] >= w[1]), "{anchos:?}");
        assert!(fs[0].text.contains("foto.jpg") && fs[0].text.contains("48 MiB/s"));
        assert_eq!(fs.last().map(|f| f.text.as_str()), Some("⟳ 1 62 %"));
    }

    /// ADR 0147: con todo lo vivo pausado la barra lo dice —⏸, sin ritmo— y
    /// se queda en su porcentaje.
    #[test]
    fn todo_pausado_se_dice_y_no_tiene_ritmo() {
        let mut s = TaskStrip::default();
        let a = p(1, TaskKind::Copy, TaskState::Paused, 40, Some(100));
        let tarea = StripTask {
            progress: &a,
            operand: None,
            bps: Some(1_000_000.0),
        };
        let a0 = p(1, TaskKind::Copy, R, 40, Some(100));
        s.update(0, [t(&a0)]);
        s.update(UMBRAL_MS, [tarea]);
        let v = s.view(UMBRAL_MS).expect("barra");
        assert_eq!((v.phase, v.percent), (StripPhase::Paused, Some(40)));
        assert!(v.rate.is_empty() && v.eta.is_empty());
        assert!(forms(&v, Lang::Es)[0].text.starts_with('⏸'));
    }

    /// La barra ocupa siempre lo mismo, llena, vacía o sin saber.
    #[test]
    fn la_barra_tiene_ancho_fijo() {
        for pct in [Some(0), Some(37), Some(100), None] {
            for now in [0, 1_000, 7_777] {
                let b = bar_glyphs(pct, now);
                assert_eq!(crate::display::cells(&b), BAR_CELLS + 2, "{pct:?} {now}");
            }
        }
        assert!(bar_glyphs(Some(100), 0).contains(&"█".repeat(BAR_CELLS)));
    }
}
