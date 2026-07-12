//! Estado puro del TUI: panes, cursor y presentación de nombres. Máquina
//! testeable sin terminal — el render (`ui`) y el I/O (`main`) viven aparte.

use norte_proto::{Entry, EntryKind, VPath};

/// Un panel: directorio actual y sus entradas YA ordenadas.
#[derive(Debug)]
pub struct Pane {
    /// Directorio listado.
    pub dir: VPath,
    /// Entradas ordenadas ([`sort_entries`]).
    pub entries: Vec<Entry>,
    /// Índice bajo el cursor (0 incluso con lista vacía).
    pub cursor: usize,
}

impl Pane {
    /// Pane sobre `dir` con `entries` (ordénalas antes con [`sort_entries`]).
    #[must_use]
    pub fn new(dir: VPath, entries: Vec<Entry>) -> Self {
        Self {
            dir,
            entries,
            cursor: 0,
        }
    }

    /// La entrada bajo el cursor, si la hay.
    #[must_use]
    pub fn selected(&self) -> Option<&Entry> {
        self.entries.get(self.cursor)
    }

    /// Sube el cursor `n` posiciones (con tope en 0).
    pub fn move_up(&mut self, n: usize) {
        self.cursor = self.cursor.saturating_sub(n);
    }

    /// Baja el cursor `n` posiciones (con tope en la última entrada).
    pub fn move_down(&mut self, n: usize) {
        let max = self.entries.len().saturating_sub(1);
        self.cursor = (self.cursor + n).min(max);
    }

    /// Cursor a la primera entrada.
    pub fn move_to_start(&mut self) {
        self.cursor = 0;
    }

    /// Cursor a la última entrada.
    pub fn move_to_end(&mut self) {
        self.cursor = self.entries.len().saturating_sub(1);
    }

    /// Reemplaza el contenido tras un cd/refresh, reseteando el cursor.
    pub fn set_listing(&mut self, dir: VPath, entries: Vec<Entry>) {
        self.dir = dir;
        self.entries = entries;
        self.cursor = 0;
    }
}

/// Orden del listado (presentación): directorios primero; dentro de cada
/// grupo, por la forma NFC del nombre (spec §6.1: `unicode_compare = nfc`
/// por defecto — SOLO como clave de orden, los bytes jamás se mutan) con
/// desempate por bytes crudos. Nombres no-UTF8: bytes tal cual.
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_by_cached_key(|e| {
        let name = name_bytes(e);
        (e.kind != EntryKind::Dir, nfc_key(name), name.to_vec())
    });
}

fn nfc_key(name: &[u8]) -> Vec<u8> {
    use unicode_normalization::UnicodeNormalization;
    match std::str::from_utf8(name) {
        Ok(s) => s.nfc().collect::<String>().into_bytes(),
        Err(_) => name.to_vec(),
    }
}

fn name_bytes(e: &Entry) -> &[u8] {
    e.path.file_name().map_or(b"", |n| n.as_bytes())
}

/// ¿Debe enmascararse en un terminal? Cc (controles: `\n`, ESC — ratatui
/// los BORRA en silencio y un frontend directo los ejecutaría) y los
/// overrides bidi Cf (spoofing RTL del orden visual).
fn must_mask(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Nombre listo para pintar: `(texto, hostil)`. `hostil = true` cuando el
/// texto pintado DIFIERE del nombre real: bytes no-UTF8 (lossy `�`),
/// controles o bidi enmascarados a `�` (spec §6: display siempre lossy y
/// MARCADO — jamás pérdida silenciosa, jamás controles crudos).
#[must_use]
pub fn display_name(bytes: &[u8]) -> (String, bool) {
    let (raw, lossy) = match std::str::from_utf8(bytes) {
        Ok(s) => (std::borrow::Cow::Borrowed(s), false),
        Err(_) => (String::from_utf8_lossy(bytes), true),
    };
    let mut masked = false;
    let texto: String = raw
        .chars()
        .map(|c| {
            if must_mask(c) {
                masked = true;
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect();
    (texto, lossy || masked)
}

/// Path completo listo para pintar: `display_lossy` + marca si CUALQUIER
/// segmento saldría alterado (mismo criterio que [`display_name`]).
#[must_use]
pub fn path_display(p: &VPath) -> (String, bool) {
    let hostil = p.segments().any(|s| display_name(s).1);
    (p.display_lossy(), hostil)
}

/// Estado completo del TUI: dos panes y el foco.
pub struct App {
    /// Los dos paneles (izquierda, derecha).
    pub panes: [Pane; 2],
    /// Índice del pane con foco (invariante 0|1: privado, ver [`Self::focus`]).
    focus: usize,
    /// `true` cuando el usuario pidió salir.
    pub quit: bool,
    /// Secuencia de teclas pendiente, ya formateada (status bar).
    pub pending: String,
    /// Diálogo modal activo (bloquea el keymap hasta resolverse).
    pub modal: Option<Modal>,
    /// Último mensaje para la barra (error por categoría o resultado).
    pub message: Option<String>,
    /// Panel de tasks vivo.
    pub board: crate::tasks::TaskBoard,
    /// Viewer abierto (F3); None = navegando.
    pub viewer: Option<crate::viewer::Viewer>,
    /// Ayuda abierta (F1): líneas ya construidas + scroll. Se construye
    /// del keymap EFECTIVO al abrir (extensible: preset y capas del
    /// usuario incluidos, jamás una lista a mano).
    pub help: Option<Help>,
    /// Colisiones a la espera de diálogo: JAMÁS se pisa un modal abierto
    /// (una tecla en vuelo respondería a la pregunta equivocada); se
    /// atienden en orden al cerrarse el modal actual.
    pub pending_collisions: std::collections::VecDeque<crate::tasks::RetrySpec>,
}

impl App {
    /// App con foco en el pane izquierdo.
    #[must_use]
    pub fn new(left: Pane, right: Pane) -> Self {
        Self {
            panes: [left, right],
            focus: 0,
            quit: false,
            pending: String::new(),
            modal: None,
            message: None,
            board: crate::tasks::TaskBoard::default(),
            viewer: None,
            help: None,
            pending_collisions: std::collections::VecDeque::new(),
        }
    }

    /// Índice del pane con foco (0 = izquierda, 1 = derecha).
    #[must_use]
    pub fn focus(&self) -> usize {
        self.focus
    }

    /// El pane con foco.
    #[must_use]
    pub fn focused(&self) -> &Pane {
        &self.panes[self.focus]
    }

    /// El pane con foco, mutable.
    pub fn focused_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.focus]
    }

    /// Alterna el foco entre los dos panes (Tab, keymap mc).
    pub fn switch_focus(&mut self) {
        self.focus ^= 1;
    }

    /// Si no hay modal abierto, abre el diálogo de la siguiente colisión
    /// encolada. Llamar tras cerrar un modal y en cada tick.
    pub fn open_next_collision(&mut self) {
        if self.modal.is_none()
            && let Some(retry) = self.pending_collisions.pop_front()
        {
            self.modal = Some(Modal::Collision { retry });
        }
    }
}

/// Estado de la ayuda (F1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Help {
    /// Contenido ya renderizable (secciones y bindings formateados).
    pub lines: Vec<String>,
    /// Primera línea visible.
    pub scroll: usize,
}

impl Help {
    /// Baja `n` líneas (tope al final).
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.lines.len().saturating_sub(1));
    }

    /// Sube `n` líneas.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }
}

/// Tipo de transferencia pendiente de confirmación/colisión.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// Copia (F5).
    Copy,
    /// Movimiento (F6).
    Move,
}

/// Diálogo modal activo. Sus teclas van HARDCODEADAS (son la semántica del
/// diálogo, no bindings del usuario); el contexto `dialog` del keymap es
/// deuda anotada (issue #24).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Modal {
    /// Confirmación de borrado (F8). `permanent = false` → papelera.
    ConfirmDelete {
        /// Lo que se borraría.
        target: VPath,
        /// `true` = borrado PERMANENTE (sin papelera aquí, o elección
        /// explícita): el diálogo AVISA (ADR 0009).
        permanent: bool,
    },
    /// Confirmación de copy/move (F5/F6).
    ConfirmTransfer {
        /// Copy o Move.
        kind: TransferKind,
        /// Origen (la entrada seleccionada).
        from: VPath,
        /// Destino (el dir del otro pane + el nombre).
        to: VPath,
    },
    /// Colisión: elegir política y REENVIAR la operación entera (ADR 0005:
    /// el engine trata Ask como Fail; el TUI pregunta a nivel de task).
    /// Porta el `RetrySpec` COMPLETO: el reintento conserva las opciones
    /// originales, solo cambia la política de colisión.
    Collision {
        /// La transferencia que colisionó, lista para reenviar.
        retry: crate::tasks::RetrySpec,
    },
}

/// Resultado de una tecla sobre un modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogOutcome {
    /// Tecla irrelevante: el diálogo sigue abierto.
    Open,
    /// Cerrado sin hacer nada.
    Cancelled,
    /// Confirmado (Enter/y).
    Confirmed,
    /// Reintentar la transferencia con esta política.
    Retry(norte_proto::CollisionPolicy),
}

/// Teclas de diálogo: Esc SIEMPRE cancela; confirmaciones aceptan Enter/y
/// y rechazan n; la colisión elige o/s/r/n (sin default en Enter: no hay
/// respuesta inocua que merezca dispararse sola).
#[must_use]
pub fn dialog_key(modal: &Modal, code: crossterm::event::KeyCode) -> DialogOutcome {
    use crossterm::event::KeyCode as K;
    use norte_proto::CollisionPolicy as P;
    if code == K::Esc {
        return DialogOutcome::Cancelled;
    }
    match modal {
        Modal::ConfirmDelete { .. } | Modal::ConfirmTransfer { .. } => match code {
            K::Enter | K::Char('y') => DialogOutcome::Confirmed,
            K::Char('n') => DialogOutcome::Cancelled,
            _ => DialogOutcome::Open,
        },
        Modal::Collision { .. } => match code {
            K::Char('o') => DialogOutcome::Retry(P::Overwrite),
            K::Char('s') => DialogOutcome::Retry(P::Skip),
            K::Char('r') => DialogOutcome::Retry(P::RenameAuto),
            K::Char('n') => DialogOutcome::Retry(P::Newer),
            _ => DialogOutcome::Open,
        },
    }
}
