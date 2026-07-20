# GUI-b mutaciones dual-pane — plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** dual-pane OPERATIVO en `norte-gui`: copy/move/delete ortodoxo (pane activo → pane destino), marcas multi-select, progreso y cancelación de tasks en una franja al pie, y resolución de conflictos — todo por el daemon (reglas 7/9).

**Architecture:** un `RemoteBackend` PERSISTENTE vive toda la sesión en un hilo con runtime tokio propio (`session.rs`), con un canal de comandos (GPUI→tokio) y uno de eventos (tokio→GPUI); los `cd` (listados) migran a este backend único, cerrando la deuda T4 de GUI-a. Las marcas viven en el `PaneState` PURO de `norte-frontend` (la TUI las hereda, deuda #82). Los modales y el mapeo de teclas son fn puras testeables en `norte-gui` (política spike: sin tests de render). Spec: `docs/superpowers/specs/2026-07-20-gui-b-mutaciones-dual-pane-design.md`.

**Tech Stack:** norte-frontend (norte-proto); norte-gui (GPUI rev f14fea9, `norte_core::backend::{Backend, RemoteBackend, TaskRef, TaskCanceller}`, `norte_core::TransferOptions`, `norte_proto::{DeleteMode, CollisionPolicy, TaskProgress, TaskState, TaskKind, TaskId, ConflictKind, Error}`).

**Convenciones (cada task):** TDD donde hay lógica pura; `just ci` verde tras cada task que toca el WORKSPACE (solo T1 toca `norte-frontend`); `norte-gui` (T2–T6) se verifica con `cargo build -p norte-gui`/`cargo clippy`/`cargo nextest` DENTRO de `crates/norte-gui/` (excluido del workspace, política spike) + verificación manual contra `norte daemon run`. Español; commits `feat(frontend):`/`feat(gui):`/`refactor(gui):`.

**Datos verificados (del código, 2026-07-20):**
- `norte_frontend::PaneState` (`crates/norte-frontend/src/pane.rs`): campos privados `dir/entries/cursor/loading/quick`; `selected() -> Option<&Entry>` (respeta filtro quick); `set_listing`/`begin_loading` resetean cursor y matan quick. NO tiene marcas: las añade T1.
- `norte_proto::Entry { path: VPath, kind: EntryKind, size, mtime_ms }`. El `path` es el VPath ABSOLUTO de la entrada (p. ej. `mem:///alfa`), no solo el nombre.
- `VPath` deriva `Clone, PartialEq, Eq, Hash`; `join(Segment) -> Self`, `parent() -> Option<Self>`, `file_name() -> Option<&Segment>`, `is_root()`. `Segment::new(impl Into<Vec<u8>>) -> Result<Segment, VPathError>` (acepta `0xFF`; rechaza `/` y NUL), deriva `Clone`.
- `norte_core::backend`: `RemoteBackend` (`impl Clone`, `connect(PathBuf, spawn: Option<..>, ClientInfo) -> Result<Self, Error>`); `Backend::Remote(RemoteBackend)`; `Backend::{list, copy, move_, delete}` (async, `copy/move_` toman `opts: TransferOptions` por valor, `delete` toma `mode: DeleteMode`; todas devuelven `Result<TaskRef, Error>` salvo `list -> Result<Vec<Entry>, Error>`); `TaskRef::{id() -> TaskId, progress() -> watch::Receiver<TaskProgress>, canceller() -> TaskCanceller}`; `TaskCanceller: Clone` con `cancel(&self)`.
- `norte_core::TransferOptions { on_collision: CollisionPolicy, symlinks, resume, verify }` (deriva `Clone`, `Default`; `on_collision` default `Fail`). `norte_proto::CollisionPolicy::{Fail, Skip, Overwrite, ...}`. `norte_proto::DeleteMode::{Trash (default), Permanent}`.
- `norte_proto::TaskProgress { task_id, kind: TaskKind, state: TaskState, bytes_done, bytes_total: Option<u64>, entries_done, entries_total: Option<u64>, current: Option<VPath> }`. `TaskState::{Pending, Running, Completed, Cancelled, Failed{error: Error}, ...}` con `is_terminal()`. `Error::Conflict { conflict: ConflictKind, .. }`.
- `norte-gui` actual: `main.rs` (root view `NorteGui` con `panes: [PaneState;2]`, `focus`, `query`, `errors`, `generation`, `theme`, `socket`, `focus_handle`; `cd` lanza `fs.list` por `oneshot` per-cd; `on_key` despacha `input::Action`; `render_pane`/`render_row`; helpers `row_label`, `entry_color`, `HOSTILE_BADGE="⚠"`, `PAGE=10`, constantes de color). `input.rs` (`Action` enum + `key_to_action(key, quick_active)`). `backend_task.rs` (`LoadConfig::from_env`, `PaneListOutcome`, `spawn_list` — SE REEMPLAZA en T4). `theme_map.rs`.
- Nombres de tecla GPUI (rev f14fea9): imprimibles minúscula (`"a"`), `"tab"`, `"up"`, `"down"`, `"home"`, `"end"`, `"pageup"`, `"pagedown"`, `"backspace"`, `"escape"`, `"enter"`, `"space"`, `"insert"`, `"delete"`, `"f5"`, `"f6"`, `"f8"`, `"f9"`.

---

### Task 1: marcas multi-select en `norte_frontend::PaneState`

**Files:**
- Modify: `crates/norte-frontend/src/pane.rs`

Identidad de una marca = el `VPath` ABSOLUTO de la entrada (`Entry.path`): estable bajo re-sort dentro del mismo `dir`, se limpia al re-listar. (Sustituye la redacción "bytes del nombre" del spec: `Entry` ya lleva el path absoluto, y `VPath` es `Hash+Eq` — más simple y sin ambigüedad.)

- [ ] **Step 1: escribe los tests que fallan** — añade al `mod tests` de `pane.rs`:

```rust
    use std::collections::HashSet;

    #[test]
    fn toggle_marca_y_desmarca_la_entrada_bajo_cursor() {
        let mut p = pane(&["a", "b", "c"]);
        p.cursor_down(); // cursor en "b"
        assert_eq!(p.marks_len(), 0);
        p.toggle_mark();
        assert_eq!(p.marks_len(), 1);
        assert!(p.is_marked(&e("mem:///b", EntryKind::File)));
        assert!(!p.is_marked(&e("mem:///a", EntryKind::File)));
        p.toggle_mark(); // desmarca
        assert_eq!(p.marks_len(), 0);
        assert!(!p.is_marked(&e("mem:///b", EntryKind::File)));
    }

    #[test]
    fn marked_paths_sin_marcas_devuelve_el_target_del_cursor() {
        let mut p = pane(&["a", "b", "c"]);
        p.cursor_down(); // "b"
        assert_eq!(p.marked_paths(), vec![VPath::parse("mem:///b").unwrap()]);
    }

    #[test]
    fn marked_paths_con_marcas_en_orden_de_entries() {
        let mut p = pane(&["a", "b", "c"]);
        p.cursor_down();
        p.cursor_down();
        p.toggle_mark(); // marca "c"
        p.home();
        p.toggle_mark(); // marca "a"
        // Orden = el de `entries` (determinista), no el de inserción.
        assert_eq!(
            p.marked_paths(),
            vec![
                VPath::parse("mem:///a").unwrap(),
                VPath::parse("mem:///c").unwrap(),
            ]
        );
    }

    #[test]
    fn set_listing_limpia_las_marcas() {
        let mut p = pane(&["a", "b"]);
        p.toggle_mark();
        assert_eq!(p.marks_len(), 1);
        p.set_listing(
            VPath::parse("mem:///otro").unwrap(),
            vec![e("mem:///otro/x", EntryKind::File)],
        );
        assert_eq!(p.marks_len(), 0);
    }

    #[test]
    fn begin_loading_limpia_las_marcas() {
        let mut p = pane(&["a", "b"]);
        p.toggle_mark();
        p.begin_loading(VPath::parse("mem:///nuevo").unwrap());
        assert_eq!(p.marks_len(), 0);
    }

    #[test]
    fn toggle_bajo_filtro_marca_la_seleccion_visible() {
        let mut p = pane(&["alfa", "beta", "alto"]);
        p.quick_start(crate::nav::Mode::Filter);
        p.quick_char('a'); // "alfa" y "alto" visibles; selección = "alfa"
        p.toggle_mark();
        assert!(p.is_marked(&e("mem:///alfa", EntryKind::File)));
        assert!(!p.is_marked(&e("mem:///beta", EntryKind::File)));
    }

    #[test]
    fn marca_identidad_por_bytes_del_path_nombre_hostil() {
        // Un nombre con bytes NO-UTF8 (0xFF): la marca lo distingue por su
        // VPath exacto, sin degradar a lossy (regla 1).
        let hostile = VPath::parse("mem:///")
            .unwrap()
            .join(norte_proto::Segment::new(vec![0xFF, 0xFE]).unwrap());
        let benign = VPath::parse("mem:///a").unwrap();
        let mut p = PaneState::new(
            VPath::parse("mem:///").unwrap(),
            vec![
                Entry { path: hostile.clone(), kind: EntryKind::File, size: None, mtime_ms: None },
                Entry { path: benign.clone(), kind: EntryKind::File, size: None, mtime_ms: None },
            ],
        );
        p.toggle_mark(); // marca la hostil (cursor 0)
        assert!(p.marks.contains(&hostile));
        assert!(!p.marks.contains(&benign));
        assert_eq!(p.marked_paths(), vec![hostile]);
    }
```

- [ ] **Step 2: corre los tests, deben fallar** — `cd crates/norte-frontend && cargo nextest run pane`
  Esperado: FAIL (`no method named toggle_mark`, campo `marks` inexistente).

- [ ] **Step 3: implementación** — en `pane.rs`:
  1. Añade al `use`: `use std::collections::HashSet;`.
  2. En `struct PaneState` añade el campo: `marks: HashSet<VPath>,`.
  3. En `new`, `set_listing`, `begin_loading`: inicializa/limpia `marks`. En `new`: `marks: HashSet::new(),`. En `set_listing` y `begin_loading`: añade `self.marks.clear();`.
  4. Añade los métodos (tras `quick_visible`):

```rust
    /// Togglea la marca de la entrada seleccionada (respeta el filtro quick:
    /// marca la entrada VISIBLE bajo la selección). No-op si no hay selección.
    pub fn toggle_mark(&mut self) {
        let Some(path) = self.selected().map(|e| e.path.clone()) else {
            return;
        };
        if !self.marks.remove(&path) {
            self.marks.insert(path);
        }
    }

    /// ¿Está marcada esta entrada? (por su `VPath` absoluto).
    #[must_use]
    pub fn is_marked(&self, entry: &Entry) -> bool {
        self.marks.contains(&entry.path)
    }

    /// Cuántas entradas marcadas.
    #[must_use]
    pub fn marks_len(&self) -> usize {
        self.marks.len()
    }

    /// Los VPaths sobre los que opera la acción: las marcas (en el ORDEN de
    /// `entries`, determinista), o el target del cursor si no hay marcas (vacío
    /// si tampoco hay selección). Fuente única de "sobre qué opera la op".
    #[must_use]
    pub fn marked_paths(&self) -> Vec<VPath> {
        if self.marks.is_empty() {
            return self.selected().map(|e| e.path.clone()).into_iter().collect();
        }
        self.entries
            .iter()
            .filter(|e| self.marks.contains(&e.path))
            .map(|e| e.path.clone())
            .collect()
    }

    /// Limpia todas las marcas.
    pub fn clear_marks(&mut self) {
        self.marks.clear();
    }
```

- [ ] **Step 4: verde** — `cd crates/norte-frontend && cargo nextest run pane && cargo clippy --all-targets -- -D warnings`
  Esperado: PASS. Luego desde la raíz `cargo nextest run -p norte-tui` (la TUI re-exporta/consume `PaneState`? NO lo consume aún — solo confirma que el crate compila para sus dependientes) y `cargo fmt --all`.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/pane.rs
git commit -m "feat(frontend): marcas multi-select en PaneState (GUI-b T1)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: acciones de mutación en `norte-gui/src/input.rs`

**Files:**
- Modify: `crates/norte-gui/src/input.rs`

Extiende el `Action` enum PURO con las teclas de mutación; el handler de `main.rs` (T5) las ejecuta. Trabaja DENTRO de `crates/norte-gui/`.

- [ ] **Step 1: escribe los tests que fallan** — añade al `mod tests` de `input.rs`:

```rust
    #[test]
    fn teclas_de_mutacion_mapean_igual_con_o_sin_quick() {
        for &(key, action) in &[
            ("insert", Action::ToggleMark),
            ("f5", Action::Copy),
            ("f6", Action::Move),
            ("f8", Action::Delete),
            ("delete", Action::Delete),
            ("f9", Action::CancelTask),
        ] {
            assert_eq!(key_to_action(key, false), action, "{key} sin quick");
            assert_eq!(key_to_action(key, true), action, "{key} con quick");
        }
    }
```

- [ ] **Step 2: corre, deben fallar** — `cd crates/norte-gui && cargo nextest run --bin norte-gui input`
  Esperado: FAIL (variantes `ToggleMark`/`Copy`/`Move`/`Delete`/`CancelTask` inexistentes).

- [ ] **Step 3: implementación** — en `input.rs`:
  1. Añade al `enum Action` (antes de `None`):

```rust
    /// Togglea la marca de la entrada bajo el cursor (Insert).
    ToggleMark,
    /// Copia la selección/marcas al pane destino (F5).
    Copy,
    /// Mueve la selección/marcas al pane destino (F6).
    Move,
    /// Borra la selección/marcas (F8/Delete).
    Delete,
    /// Cancela la task seleccionada en la franja (F9).
    CancelTask,
```
  2. En `key_to_action`, dentro del `match key`, antes del brazo `"space"`:

```rust
        "insert" => Action::ToggleMark,
        "f5" => Action::Copy,
        "f6" => Action::Move,
        "f8" | "delete" => Action::Delete,
        "f9" => Action::CancelTask,
```

- [ ] **Step 4: verde** — `cd crates/norte-gui && cargo nextest run --bin norte-gui input && cargo clippy --bin norte-gui -- -D warnings && cargo fmt`
  Esperado: PASS. (Las variantes nuevas se "usan" en el test; el `match` de `main.rs` las cablea en T5 — hasta entonces `main.rs` sigue con su `match` exhaustivo sobre las viejas, así que añade un brazo `_ => {}` temporal SOLO si el build de `main.rs` se queja de exhaustividad. Verifica: `cargo build -p norte-gui`.)

  > NOTA: `Action` es `#[derive(...)]` sin `#[non_exhaustive]`; el `match action` de `main.rs::on_key` DEBE cubrir las variantes nuevas o no compila. Para mantener T2 verde sin adelantar T5, añade en ese `match` (main.rs) los brazos placeholder `Action::ToggleMark | Action::Copy | Action::Move | Action::Delete | Action::CancelTask => {}` — T5 los reemplaza por la lógica real.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-gui/src/input.rs crates/norte-gui/src/main.rs
git commit -m "feat(gui): acciones de mutación en el mapeo de teclas (GUI-b T2)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: máquina de modales pura `norte-gui/src/modal.rs`

**Files:**
- Create: `crates/norte-gui/src/modal.rs`
- Modify: `crates/norte-gui/src/main.rs` (declara `mod modal;`)

Define los tipos de operación compartidos (`TransferKind`, `PendingOp`) y la máquina de modales, todo PURO (sin GPUI). `session.rs` (T4/T5) importará `PendingOp`/`TransferKind` de aquí.

- [ ] **Step 1: escribe `modal.rs` con sus tests** — crea `crates/norte-gui/src/modal.rs`:

```rust
//! Modales de confirmación y resolución de conflicto (máquina de estado PURA,
//! testeable sin GPUI). El handler de `main.rs` enruta la tecla aquí con
//! [`on_key`] y actúa sobre el [`ModalOutcome`]; el render pinta el [`Modal`].
//!
//! Aquí también viven los tipos de OPERACIÓN mutante ([`TransferKind`],
//! [`PendingOp`]) que `session.rs` ejecuta: un modal confirmado produce
//! `PendingOp`s que la GUI manda por el canal de comandos.

use norte_core::TransferOptions;
use norte_proto::{CollisionPolicy, ConflictKind, DeleteMode, VPath};

/// Copia o movimiento (la clase de una transferencia).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// Copia (origen intacto).
    Copy,
    /// Movimiento (origen desaparece al terminar).
    Move,
}

/// Una operación mutante concreta lista para mandar al daemon.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingOp {
    /// Copia/movimiento de `from` a `to` (ambos ABSOLUTOS; `to` ya incluye el
    /// nombre destino) con la política de colisión de `opts`.
    Transfer {
        /// Copia o movimiento.
        kind: TransferKind,
        /// Origen (path absoluto de la entrada).
        from: VPath,
        /// Destino absoluto (dir destino + nombre del origen).
        to: VPath,
        /// Opciones (colisión, symlinks, resume).
        opts: TransferOptions,
    },
    /// Borrado de `path` (papelera o permanente).
    Delete {
        /// Entrada a borrar.
        path: VPath,
        /// Papelera (default) o permanente.
        mode: DeleteMode,
    },
}

/// Modal activo. Uno a la vez; el render lo pinta como overlay.
#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    /// Confirmar copia/movimiento de `items` al dir `to`.
    ConfirmTransfer {
        /// Copia o movimiento.
        kind: TransferKind,
        /// Orígenes (paths absolutos; marcas o el target del cursor).
        items: Vec<VPath>,
        /// DIRECTORIO destino (el `dir` del pane inactivo).
        to: VPath,
    },
    /// Confirmar borrado de `items`; `permanent` toggleable.
    ConfirmDelete {
        /// Entradas a borrar.
        items: Vec<VPath>,
        /// `false` = papelera (default), `true` = permanente.
        permanent: bool,
    },
    /// Un transfer chocó con el destino: reintentar/saltar/cancelar.
    ConflictResolve {
        /// La transferencia que falló (para reemitir).
        pending: PendingTransfer,
        /// Subtipo de conflicto (para pintar el motivo).
        conflict: ConflictKind,
    },
}

/// La transferencia que originó un conflicto (para reemitir con otra política).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingTransfer {
    /// Copia o movimiento.
    pub kind: TransferKind,
    /// Origen.
    pub from: VPath,
    /// Destino absoluto.
    pub to: VPath,
}

/// Qué debe hacer el handler tras una tecla en el modal.
#[derive(Debug, Clone, PartialEq)]
pub enum ModalOutcome {
    /// Tecla irrelevante: no pasa nada (el modal sigue abierto).
    Ignored,
    /// El estado del modal cambió (p. ej. toggle permanent): re-render, sigue abierto.
    StayOpen,
    /// Cierra el modal sin hacer nada.
    Dismiss,
    /// Cierra el modal y manda estas operaciones al daemon.
    Submit(Vec<PendingOp>),
}

/// Destino absoluto de un item copiado/movido a `to_dir`: `to_dir` + nombre del
/// origen. `None` si el origen no tiene nombre (raíz — no debería marcarse).
#[must_use]
pub fn dest_for(to_dir: &VPath, item: &VPath) -> Option<VPath> {
    item.file_name().map(|n| to_dir.join(n.clone()))
}

/// Enruta una tecla (nombre GPUI) al modal, mutándolo si hace falta. `main.rs`
/// llama a esto cuando hay un modal abierto, ANTES del mapeo de navegación.
pub fn on_key(modal: &mut Modal, key: &str) -> ModalOutcome {
    match modal {
        Modal::ConfirmTransfer { kind, items, to } => match key {
            "y" => {
                let ops = items
                    .iter()
                    .filter_map(|from| {
                        dest_for(to, from).map(|dest| PendingOp::Transfer {
                            kind: *kind,
                            from: from.clone(),
                            to: dest,
                            opts: TransferOptions::default(),
                        })
                    })
                    .collect();
                ModalOutcome::Submit(ops)
            }
            "n" | "escape" => ModalOutcome::Dismiss,
            _ => ModalOutcome::Ignored,
        },
        Modal::ConfirmDelete { items, permanent } => match key {
            "y" => {
                let mode = if *permanent { DeleteMode::Permanent } else { DeleteMode::Trash };
                let ops = items
                    .iter()
                    .map(|path| PendingOp::Delete { path: path.clone(), mode })
                    .collect();
                ModalOutcome::Submit(ops)
            }
            "p" | "tab" => {
                *permanent = !*permanent;
                ModalOutcome::StayOpen
            }
            "n" | "escape" => ModalOutcome::Dismiss,
            _ => ModalOutcome::Ignored,
        },
        Modal::ConflictResolve { pending, .. } => {
            let policy = match key {
                "o" => CollisionPolicy::Overwrite,
                "s" => CollisionPolicy::Skip,
                "c" | "escape" => return ModalOutcome::Dismiss,
                _ => return ModalOutcome::Ignored,
            };
            ModalOutcome::Submit(vec![PendingOp::Transfer {
                kind: pending.kind,
                from: pending.from.clone(),
                to: pending.to.clone(),
                opts: TransferOptions { on_collision: policy, ..Default::default() },
            }])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(s: &str) -> VPath {
        VPath::parse(s).unwrap()
    }

    #[test]
    fn dest_for_une_dir_destino_con_nombre_origen() {
        assert_eq!(
            dest_for(&vp("mem:///dst"), &vp("mem:///src/foo.txt")),
            Some(vp("mem:///dst/foo.txt"))
        );
    }

    #[test]
    fn confirm_transfer_y_produce_una_op_por_item() {
        let mut m = Modal::ConfirmTransfer {
            kind: TransferKind::Copy,
            items: vec![vp("mem:///src/a"), vp("mem:///src/b")],
            to: vp("mem:///dst"),
        };
        let out = on_key(&mut m, "y");
        assert_eq!(
            out,
            ModalOutcome::Submit(vec![
                PendingOp::Transfer {
                    kind: TransferKind::Copy,
                    from: vp("mem:///src/a"),
                    to: vp("mem:///dst/a"),
                    opts: TransferOptions::default(),
                },
                PendingOp::Transfer {
                    kind: TransferKind::Copy,
                    from: vp("mem:///src/b"),
                    to: vp("mem:///dst/b"),
                    opts: TransferOptions::default(),
                },
            ])
        );
    }

    #[test]
    fn confirm_transfer_n_o_escape_descartan() {
        let mut m = Modal::ConfirmTransfer {
            kind: TransferKind::Move,
            items: vec![vp("mem:///src/a")],
            to: vp("mem:///dst"),
        };
        assert_eq!(on_key(&mut m, "n"), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape"), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "x"), ModalOutcome::Ignored);
    }

    #[test]
    fn confirm_delete_togglea_permanent_y_confirma_con_el_modo() {
        let mut m = Modal::ConfirmDelete { items: vec![vp("mem:///a")], permanent: false };
        assert_eq!(on_key(&mut m, "p"), ModalOutcome::StayOpen);
        // Ahora permanent=true → la op sale Permanent.
        assert_eq!(
            on_key(&mut m, "y"),
            ModalOutcome::Submit(vec![PendingOp::Delete {
                path: vp("mem:///a"),
                mode: DeleteMode::Permanent,
            }])
        );
    }

    #[test]
    fn confirm_delete_default_es_papelera() {
        let mut m = Modal::ConfirmDelete { items: vec![vp("mem:///a")], permanent: false };
        assert_eq!(
            on_key(&mut m, "y"),
            ModalOutcome::Submit(vec![PendingOp::Delete {
                path: vp("mem:///a"),
                mode: DeleteMode::Trash,
            }])
        );
    }

    #[test]
    fn conflict_overwrite_reemite_con_overwrite() {
        let mut m = Modal::ConflictResolve {
            pending: PendingTransfer {
                kind: TransferKind::Copy,
                from: vp("mem:///src/a"),
                to: vp("mem:///dst/a"),
            },
            conflict: ConflictKind::Exists,
        };
        let out = on_key(&mut m, "o");
        match out {
            ModalOutcome::Submit(ops) => {
                assert_eq!(ops.len(), 1);
                match &ops[0] {
                    PendingOp::Transfer { opts, .. } => {
                        assert_eq!(opts.on_collision, CollisionPolicy::Overwrite);
                    }
                    other => panic!("esperaba Transfer, vino {other:?}"),
                }
            }
            other => panic!("esperaba Submit, vino {other:?}"),
        }
    }

    #[test]
    fn conflict_cancelar_descarta() {
        let mut m = Modal::ConflictResolve {
            pending: PendingTransfer {
                kind: TransferKind::Copy,
                from: vp("mem:///src/a"),
                to: vp("mem:///dst/a"),
            },
            conflict: ConflictKind::Exists,
        };
        assert_eq!(on_key(&mut m, "c"), ModalOutcome::Dismiss);
        assert_eq!(on_key(&mut m, "escape"), ModalOutcome::Dismiss);
    }
}
```

- [ ] **Step 2: declara el módulo** — en `main.rs`, junto a `mod input;`: añade `mod modal;`.

- [ ] **Step 3: corre los tests** — `cd crates/norte-gui && cargo nextest run --bin norte-gui modal`
  Esperado: PASS (todo el módulo es puro).

  > Si `cargo build -p norte-gui` avisa de `dead_code` en variantes/campos de `PendingOp`/`Modal` (aún no las construye `main.rs`), añade `#![allow(dead_code)]` TEMPORAL arriba de `modal.rs` con un `// TODO(T5): quitar cuando main.rs use estos tipos`. T5 lo retira.

- [ ] **Step 4: clippy + fmt** — `cargo clippy --bin norte-gui -- -D warnings && cargo fmt`
  Esperado: limpio.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-gui/src/modal.rs crates/norte-gui/src/main.rs
git commit -m "feat(gui): máquina de modales pura (confirmar transfer/delete + conflicto) (GUI-b T3)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: backend de sesión persistente (`session.rs`, solo listado) + migrar `cd`

**Files:**
- Create: `crates/norte-gui/src/session.rs`
- Delete: `crates/norte-gui/src/backend_task.rs`
- Modify: `crates/norte-gui/src/main.rs`

Reemplaza el reconnect-per-cd por UN `RemoteBackend` persistente con canal de comandos + canal de eventos. En esta task SOLO el listado (`List`) para mantener el read-only VERDE; las mutaciones entran en T5. Cierra la deuda T4 de GUI-a (los `cd` dejan de reconectar).

- [ ] **Step 1: crea `session.rs` (solo List)** — `crates/norte-gui/src/session.rs`:

```rust
//! Backend de SESIÓN persistente: UN `RemoteBackend` vivo toda la sesión, en
//! un hilo con runtime tokio propio. Reemplaza el reconnect-per-cd del spike
//! (GUI-a deuda T4): la conexión se establece una vez y todos los listados
//! (y en T5 las mutaciones) van por ella — condición NECESARIA para seguir el
//! progreso de una task (`task.progress` llega por la BOMBA del `RemoteBackend`,
//! que exige conexión viva).
//!
//! # Puente GPUI ↔ tokio
//! GPUI (executor propio) manda [`SessionCmd`] por un `mpsc` sin bloquear; el
//! hilo tokio ejecuta y devuelve [`SessionEvent`] por otro `mpsc` que la GUI
//! drena en UN `cx.spawn` (los `mpsc` de tokio son awaitables en cualquier
//! executor: solo registran un waker). Un daemon caído en `connect` emite
//! [`SessionEvent::ConnectFailed`], jamás panic.

use std::path::{Path, PathBuf};

use norte_core::backend::{Backend, remote::RemoteBackend};
use norte_proto::methods::ClientInfo;
use norte_proto::{Entry, VPath};
use tokio::sync::mpsc;

/// Config resuelta del entorno (socket + dir inicial). Idéntica a la del spike
/// (movida de `backend_task.rs`).
pub struct LoadConfig {
    /// Socket UDS del daemon.
    pub socket: PathBuf,
    /// Directorio inicial de ambos panes.
    pub dir: VPath,
}

impl LoadConfig {
    /// Resuelve desde el entorno: `NORTE_SOCKET` o el default del daemon;
    /// `NORTE_DIR` (wire) o el `cwd`.
    ///
    /// # Errors
    /// `NORTE_DIR` no parsea, o el `cwd` no se puede leer/convertir.
    pub fn from_env() -> anyhow::Result<Self> {
        let socket = match std::env::var_os("NORTE_SOCKET") {
            Some(s) => PathBuf::from(s),
            None => norte_core::daemon::default_socket_path(None),
        };
        let dir = match std::env::var("NORTE_DIR") {
            Ok(wire) => VPath::parse(&wire)
                .map_err(|e| anyhow::anyhow!("NORTE_DIR no es un VPath válido: {e}"))?,
            Err(_) => {
                let cwd = std::env::current_dir()?;
                norte_vfs_local::vpath_from_native(&cwd)
                    .map_err(|e| anyhow::anyhow!("cwd → VPath: {e}"))?
            }
        };
        Ok(Self { socket, dir })
    }
}

/// Comando de la GUI hacia el hilo de sesión.
pub enum SessionCmd {
    /// Lista `dir` en `pane`; `generation` es el guard anti-stale de GUI-a.
    List {
        /// Pane destino (0|1).
        pane: usize,
        /// Generación del cd (ver `main::generation_is_current`).
        generation: u64,
        /// Directorio a listar.
        dir: VPath,
    },
}

/// Evento del hilo de sesión hacia la GUI.
pub enum SessionEvent {
    /// Resultado de un `List` (etiquetado con pane/generación/dir).
    Listed {
        /// Pane destino.
        pane: usize,
        /// Generación del cd que lo pidió.
        generation: u64,
        /// Directorio listado.
        dir: VPath,
        /// Entradas o error aplanado a String (ya renderizable).
        outcome: Result<Vec<Entry>, String>,
    },
    /// La conexión inicial con el daemon falló (mensaje ya renderizable).
    ConnectFailed(String),
}

/// Arranca el hilo de sesión: conecta al `socket` UNA vez y sirve `cmd_rx`,
/// emitiendo por `event_tx`. Si `connect` falla, emite `ConnectFailed` y el
/// hilo termina (la GUI queda en estado de error, usable).
pub fn spawn(
    socket: PathBuf,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCmd>,
    event_tx: mpsc::UnboundedSender<SessionEvent>,
) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = event_tx.send(SessionEvent::ConnectFailed(format!("runtime tokio: {e}")));
                return;
            }
        };
        rt.block_on(async move {
            let remote = match connect(&socket).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = event_tx.send(SessionEvent::ConnectFailed(format!("{e}")));
                    return;
                }
            };
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    SessionCmd::List { pane, generation, dir } => {
                        let backend = Backend::Remote(remote.clone());
                        let tx = event_tx.clone();
                        tokio::spawn(async move {
                            let outcome = backend.list(&dir).await.map_err(|e| format!("{e}"));
                            let _ = tx.send(SessionEvent::Listed { pane, generation, dir, outcome });
                        });
                    }
                }
            }
        });
    });
}

/// Conecta al daemon (sin autoarrancarlo).
async fn connect(socket: &Path) -> Result<RemoteBackend, norte_proto::Error> {
    RemoteBackend::connect(
        socket.to_path_buf(),
        None,
        ClientInfo {
            name: "norte-gui".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    )
    .await
}
```

- [ ] **Step 2: borra `backend_task.rs` y su `mod`** — `git rm crates/norte-gui/src/backend_task.rs`; en `main.rs` quita `mod backend_task;` y añade `mod session;`; quita `use backend_task::PaneListOutcome;`.

- [ ] **Step 3: migra el estado y `cd` de `main.rs`**:
  1. En `use ... backend_task::...` → sustituye por `use session::{LoadConfig, SessionCmd, SessionEvent};`.
  2. En `struct NorteGui`: añade el campo `cmds: tokio::sync::mpsc::UnboundedSender<SessionCmd>,` (reemplaza el uso de `socket` para relistar; conserva `socket` NO — ya no se usa: bórralo del struct y de los dos constructores).
  3. En `new` (rama OK): antes de construir `Self`, crea los canales y arranca la sesión:

```rust
        let backend_task_removed = (); // (marcador: ya no existe backend_task)
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        session::spawn(socket, cmd_rx, event_tx);
```
   (usa el `socket` de `LoadConfig` aquí, antes de moverlo). Construye `Self { ..., cmds: cmd_tx, ... }` SIN el campo `socket`. Tras construir `gui`, arranca el drenador de eventos y lanza las dos cargas:

```rust
        gui.spawn_event_loop(event_rx, cx);
        gui.cd(0, dir.clone(), cx);
        gui.cd(1, dir, cx);
        gui
```
  4. En `new` (rama Err de config): el struct necesita un `cmds`. Crea un canal cuyo `cmd_rx` se descarta (no hay sesión): 

```rust
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
```
   y construye `Self { ..., cmds: cmd_tx, ... }` sin `socket`. (No se arranca sesión: ambos panes ya nacen en error.)
  5. Reescribe `cd` para mandar el comando en vez de spawnear:

```rust
    /// Cambia el directorio de `pane` a `dir`: lo marca como cargando y manda
    /// un `List` al hilo de sesión; el resultado llega por el drenador de
    /// eventos (`spawn_event_loop`) y se aplica con el guard de generación.
    fn cd(&mut self, pane: usize, dir: VPath, _cx: &mut Context<Self>) {
        self.generation[pane] = self.generation[pane].wrapping_add(1);
        let generation = self.generation[pane];
        self.panes[pane].begin_loading(dir.clone());
        self.errors[pane] = None;
        self.query[pane].clear();
        let _ = self.cmds.send(SessionCmd::List { pane, generation, dir });
    }
```
   (nota: `cd` ya no usa `cx`; renómbralo `_cx`. Todos los call-sites de `cd` siguen pasando `cx`.)

- [ ] **Step 4: añade el drenador de eventos y `apply_event`** — en `impl NorteGui`:

```rust
    /// Drena los eventos del hilo de sesión y los aplica al estado (UN solo
    /// `cx.spawn` para toda la vida de la ventana). Sale si la entidad muere.
    fn spawn_event_loop(
        &self,
        mut rx: tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Some(ev) = rx.recv().await {
                let alive = this
                    .update(cx, |view, cx| {
                        view.apply_event(ev);
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// Aplica UN evento de sesión al estado.
    fn apply_event(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::Listed { pane, generation, dir, outcome } => {
                if !generation_is_current(self.generation[pane], generation) {
                    return; // stale: un cd más nuevo ya avanzó la generación.
                }
                match outcome {
                    Ok(entries) => {
                        let mut entries = entries;
                        norte_frontend::sort_entries(&mut entries);
                        self.panes[pane].set_listing(dir, entries);
                        self.errors[pane] = None;
                        self.query[pane].clear();
                    }
                    Err(msg) => {
                        self.panes[pane].set_listing(dir, Vec::new());
                        self.errors[pane] = Some(msg);
                    }
                }
            }
            SessionEvent::ConnectFailed(msg) => {
                self.errors[0] = Some(msg.clone());
                self.errors[1] = Some(msg);
            }
        }
    }
```
   (El bloque `cx.spawn(async move |this, cx| { rx.await; this.update(...) })` de la vieja `cd` se BORRA; su lógica de guard+apply vive ahora en `apply_event`.)

- [ ] **Step 5: build + clippy + tests** — `cd crates/norte-gui && cargo build -p norte-gui && cargo clippy --bin norte-gui --all-targets -- -D warnings && cargo nextest run --bin norte-gui`
  Esperado: compila limpio; los tests de `input`/`modal`/`row_label`/`generation` siguen verdes. Quita el marcador `let backend_task_removed = ();` (era ilustrativo — no lo dejes).

- [ ] **Step 6: verificación manual (read-only, regresión)** — arranca el daemon y la GUI:

```bash
# Terminal 1: daemon
cargo run -p norte-cli -- daemon run    # anota el socket que imprime
# Terminal 2: la GUI (excluida; corre en su dir)
cd crates/norte-gui
NORTE_SOCKET=<socket> NORTE_DIR=file:///home/oscar NORTE_GUI_DEBUG=1 cargo run
```
   Verifica (con NORTE_GUI_DEBUG en stderr, y visualmente si hay display): ambos panes listan; Tab conmuta foco; Enter en un dir hace `cd` (llega un `Listed` nuevo); Backspace sube al padre; un `NORTE_DIR` malo → banner de error sin panic; mata el daemon y arranca la GUI → `ConnectFailed` en ambos panes, ventana usable. **Una sola conexión** (no reconecta por cd — verifica en los logs del daemon que hay UN cliente, no uno por navegación).

- [ ] **Step 7: Commit**

```bash
git add -A crates/norte-gui/src/
git commit -m "refactor(gui): backend de sesión persistente — un cd por canal (GUI-b T4, cierra deuda T4)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: mutaciones en `session.rs` + cableado en `main.rs` (marcas, modales, submit, conflicto)

**Files:**
- Modify: `crates/norte-gui/src/session.rs`, `crates/norte-gui/src/main.rs`

Añade `Submit`/`Cancel` a la sesión con el forwarder de progreso, y cablea en `main.rs`: toggle de marca, F5/F6/F8 abren modales, el modal captura teclas, confirmar manda `PendingOp`s, y un conflicto abre el modal de resolución. La franja visual y el read-after-write son T6 (aquí basta con que las ops se lancen y el progreso llegue al estado).

- [ ] **Step 1: extiende `session.rs`** con las mutaciones:
  1. Amplía los `use`:

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use norte_core::backend::{Backend, TaskCanceller, TaskRef, remote::RemoteBackend};
use norte_proto::{DeleteMode, Entry, Error, TaskId, TaskProgress, TaskState, VPath};

use crate::modal::{PendingOp, TransferKind};
```
  2. Añade variantes a `SessionCmd`:

```rust
    /// Lanza una operación mutante (copy/move/delete) como task.
    Submit(PendingOp),
    /// Cancela una task en curso (`task.cancel`, fire-and-forget).
    Cancel(TaskId),
```
  3. Añade variantes a `SessionEvent`:

```rust
    /// La op fue aceptada; su task corre con este id (para mapear progreso →
    /// operación en la GUI).
    Submitted {
        /// Id de la task recién creada.
        task_id: TaskId,
        /// La operación que la originó (para read-after-write y reintento).
        op: PendingOp,
    },
    /// La op fue RECHAZADA antes de crear task (path inválido, unsupported…).
    SubmitFailed {
        /// La operación rechazada.
        op: PendingOp,
        /// El error tipado (la GUI decide: banner o modal de conflicto).
        error: Error,
    },
    /// Snapshot de progreso de una task (incl. estado terminal).
    Task(TaskProgress),
```
  4. En el `while let Some(cmd)` del `spawn`, crea el mapa de cancellers ANTES del loop:

```rust
            let cancellers: Arc<Mutex<HashMap<TaskId, TaskCanceller>>> =
                Arc::new(Mutex::new(HashMap::new()));
```
   y añade los brazos:

```rust
                    SessionCmd::Submit(op) => {
                        submit(&remote, op, &event_tx, &cancellers).await;
                    }
                    SessionCmd::Cancel(id) => {
                        if let Some(c) = cancellers.lock().unwrap().get(&id) {
                            c.cancel();
                        }
                    }
```
  5. Añade las funciones `submit` y `forward_progress`:

```rust
/// Lanza una op mutante y arranca su forwarder de progreso, o emite
/// `SubmitFailed` si el daemon la rechaza de entrada.
async fn submit(
    remote: &RemoteBackend,
    op: PendingOp,
    event_tx: &mpsc::UnboundedSender<SessionEvent>,
    cancellers: &Arc<Mutex<HashMap<TaskId, TaskCanceller>>>,
) {
    let backend = Backend::Remote(remote.clone());
    let res = match &op {
        PendingOp::Transfer { kind: TransferKind::Copy, from, to, opts } => {
            backend.copy(from, to, opts.clone()).await
        }
        PendingOp::Transfer { kind: TransferKind::Move, from, to, opts } => {
            backend.move_(from, to, opts.clone()).await
        }
        PendingOp::Delete { path, mode } => backend.delete(path, *mode).await,
    };
    match res {
        Ok(task) => {
            let id = task.id();
            cancellers.lock().unwrap().insert(id, task.canceller());
            let _ = event_tx.send(SessionEvent::Submitted { task_id: id, op });
            let tx = event_tx.clone();
            let cancellers = Arc::clone(cancellers);
            tokio::spawn(async move { forward_progress(task, &tx, &cancellers).await });
        }
        Err(error) => {
            let _ = event_tx.send(SessionEvent::SubmitFailed { op, error });
        }
    }
}

/// Reenvía cada snapshot de progreso de `task` como `SessionEvent::Task` hasta
/// el terminal; de-registra el canceller al salir. Si la conexión muere sin
/// desenlace, sintetiza un terminal `Failed{ProviderUnavailable}` reusando el
/// último snapshot (mismos id/kind).
async fn forward_progress(
    task: TaskRef,
    tx: &mpsc::UnboundedSender<SessionEvent>,
    cancellers: &Arc<Mutex<HashMap<TaskId, TaskCanceller>>>,
) {
    let id = task.id();
    let mut rx = task.progress();
    loop {
        let snap = rx.borrow().clone();
        let terminal = snap.state.is_terminal();
        let _ = tx.send(SessionEvent::Task(snap.clone()));
        if terminal {
            break;
        }
        if rx.changed().await.is_err() {
            let mut dead = snap;
            dead.state = TaskState::Failed {
                error: Error::ProviderUnavailable { retryable: true },
            };
            let _ = tx.send(SessionEvent::Task(dead));
            break;
        }
    }
    cancellers.lock().unwrap().remove(&id);
}
```
   (Nota: `TaskProgress` deriva `Clone`; `snap.clone()` en el `send` conserva el snapshot para el caso de muerte de conexión.)

- [ ] **Step 2: build de session** — `cd crates/norte-gui && cargo build -p norte-gui`
  Esperado: compila. Si `modal.rs` tenía `#![allow(dead_code)]`, aún hace falta hasta que `main.rs` (siguiente step) construya modales — no lo quites todavía.

- [ ] **Step 3: estado nuevo en `main.rs`** — añade a `struct NorteGui`:

```rust
    /// Modal activo (confirmación/conflicto), o `None`.
    modal: Option<modal::Modal>,
    /// Tasks en curso/terminadas por id, con la op que las originó (para el
    /// read-after-write de T6 y el reintento de conflicto).
    inflight: std::collections::HashMap<norte_proto::TaskId, modal::PendingOp>,
    /// Último snapshot de progreso por task (para la franja de T6 y el motivo).
    task_progress: std::collections::HashMap<norte_proto::TaskId, norte_proto::TaskProgress>,
```
   Inicialízalos en AMBOS constructores: `modal: None, inflight: HashMap::new(), task_progress: HashMap::new(),`. Añade `use` para `modal` si hace falta (`use modal::{Modal, ModalOutcome, PendingOp, PendingTransfer, TransferKind};`).

- [ ] **Step 4: enruta teclas al modal + acciones de mutación en `on_key`** — al PRINCIPIO de `on_key`, antes del cálculo de `action`, intercepta el modal:

```rust
        // Con un modal abierto, la tecla va al modal (captura fija).
        if let Some(m) = &mut self.modal {
            match modal::on_key(m, &ks.key) {
                ModalOutcome::Ignored | ModalOutcome::StayOpen => {}
                ModalOutcome::Dismiss => self.modal = None,
                ModalOutcome::Submit(ops) => {
                    self.modal = None;
                    for op in ops {
                        let _ = self.cmds.send(SessionCmd::Submit(op));
                    }
                }
            }
            cx.notify();
            return;
        }
```
   Luego, en el `match action`, reemplaza los brazos placeholder de T2 por:

```rust
            Action::ToggleMark => self.panes[f].toggle_mark(),
            Action::Copy => self.open_transfer_modal(TransferKind::Copy),
            Action::Move => self.open_transfer_modal(TransferKind::Move),
            Action::Delete => self.open_delete_modal(),
            Action::CancelTask => {} // la franja (T6) lo cablea; aquí no-op.
```

- [ ] **Step 5: helpers para abrir modales** — en `impl NorteGui`:

```rust
    /// Abre el modal de copia/movimiento: origen = marcas/cursor del pane
    /// activo, destino = dir del pane inactivo. No-op si no hay nada que mover.
    fn open_transfer_modal(&mut self, kind: TransferKind) {
        let f = self.focus;
        let items = self.panes[f].marked_paths();
        if items.is_empty() {
            return;
        }
        let to = self.panes[1 - f].dir().clone();
        self.modal = Some(Modal::ConfirmTransfer { kind, items, to });
    }

    /// Abre el modal de borrado sobre las marcas/cursor del pane activo.
    fn open_delete_modal(&mut self) {
        let f = self.focus;
        let items = self.panes[f].marked_paths();
        if items.is_empty() {
            return;
        }
        self.modal = Some(Modal::ConfirmDelete { items, permanent: false });
    }
```

- [ ] **Step 6: maneja los eventos de mutación en `apply_event`** — añade los brazos (junto a `Listed`/`ConnectFailed`):

```rust
            SessionEvent::Submitted { task_id, op } => {
                self.inflight.insert(task_id, op);
            }
            SessionEvent::SubmitFailed { op, error } => {
                // Rechazo inmediato: banner en el pane activo (los conflictos
                // reales llegan por Task terminal Failed, ver abajo).
                self.errors[self.focus] = Some(format!("operación rechazada: {error}"));
                let _ = op; // la op no se reintenta automáticamente.
            }
            SessionEvent::Task(p) => {
                let id = p.task_id;
                let terminal = p.state.is_terminal();
                let failed_conflict = matches!(
                    &p.state,
                    norte_proto::TaskState::Failed { error: norte_proto::Error::Conflict { .. } }
                );
                self.task_progress.insert(id, p);
                if terminal {
                    self.on_task_terminal(id, failed_conflict);
                }
            }
```
   Y añade el manejador de terminal (el read-after-write real es T6; aquí solo el modal de conflicto y la limpieza):

```rust
    /// Una task llegó a estado terminal: si falló por conflicto, abre el modal
    /// de resolución con la op original; si no, retira la op de `inflight`.
    /// (El re-listado read-after-write lo añade T6.)
    fn on_task_terminal(&mut self, id: norte_proto::TaskId, failed_conflict: bool) {
        let Some(op) = self.inflight.remove(&id) else {
            return;
        };
        if failed_conflict {
            if let PendingOp::Transfer { kind, from, to, .. } = op {
                let conflict = match self.task_progress.get(&id).map(|p| &p.state) {
                    Some(norte_proto::TaskState::Failed {
                        error: norte_proto::Error::Conflict { conflict, .. },
                    }) => *conflict,
                    _ => norte_proto::ConflictKind::Exists,
                };
                self.modal = Some(Modal::ConflictResolve {
                    pending: PendingTransfer { kind, from, to },
                    conflict,
                });
            }
        }
    }
```

- [ ] **Step 7: build + clippy + tests** — `cd crates/norte-gui && cargo build -p norte-gui && cargo clippy --bin norte-gui --all-targets -- -D warnings && cargo nextest run --bin norte-gui`
  Esperado: compila limpio. Quita ya el `#![allow(dead_code)]` de `modal.rs` (todos los tipos se usan). Si clippy marca algún import sin usar, límpialo.

- [ ] **Step 8: verificación manual (mutaciones)** — con daemon + GUI (como T4, con un dir de PRUEBAS con archivos):

```bash
mkdir -p /tmp/norte-gui-test/{a,b}; echo hola > /tmp/norte-gui-test/a/f.txt
cd crates/norte-gui
NORTE_SOCKET=<socket> NORTE_DIR=file:///tmp/norte-gui-test/a NORTE_GUI_DEBUG=1 cargo run
```
   Con AMBOS panes: navega el pane 1 (Tab) a `.../b`; en el pane 0 pon el cursor en `f.txt`, F5 → modal de copia; `y` → la copia se lanza (log de `Submitted`/`Task`); verifica que `f.txt` aparece en `/tmp/norte-gui-test/b` (relista a mano con Enter/Backspace hasta T6). Prueba: Insert marca varias, F5 copia todas; F8 → modal de borrado (`y` = papelera; `p` togglea permanent); F6 mueve. Provoca un conflicto (copia dos veces al mismo destino) → modal ConflictResolve; `o` sobrescribe, `s` salta, `c` cancela. Nada de panics; errores en banner.

- [ ] **Step 9: Commit**

```bash
git add crates/norte-gui/src/
git commit -m "feat(gui): copy/move/delete con marcas + modales + conflicto (GUI-b T5)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: franja de tasks al pie + cancelación + read-after-write

**Files:**
- Modify: `crates/norte-gui/src/main.rs`

Pinta la franja de tasks bajo los panes (kind + % + estado), permite cancelar la task seleccionada (F9), y relista los dirs afectados al terminar una op.

- [ ] **Step 1: estado de la franja** — añade a `struct NorteGui`:

```rust
    /// Cursor de la franja de tasks (índice en el orden de `task_order`).
    task_cursor: usize,
    /// Orden de llegada de las tasks (para un render estable; `task_progress`
    /// es un mapa sin orden).
    task_order: Vec<norte_proto::TaskId>,
```
   Inicialízalos en ambos constructores: `task_cursor: 0, task_order: Vec::new(),`. En `apply_event`, brazo `SessionEvent::Task(p)`: antes de `self.task_progress.insert`, registra el orden la primera vez:

```rust
                if !self.task_progress.contains_key(&id) {
                    self.task_order.push(id);
                }
```

- [ ] **Step 2: read-after-write** — en `on_task_terminal`, tras el bloque de conflicto, si NO fue conflicto (op consumida OK/cancelada) relista los dirs afectados. Cambia la firma para no consumir `op` antes de tiempo; reescríbelo así:

```rust
    fn on_task_terminal(&mut self, id: norte_proto::TaskId, failed_conflict: bool) {
        let Some(op) = self.inflight.remove(&id) else {
            return;
        };
        if failed_conflict {
            if let PendingOp::Transfer { kind, from, to, .. } = op {
                let conflict = match self.task_progress.get(&id).map(|p| &p.state) {
                    Some(norte_proto::TaskState::Failed {
                        error: norte_proto::Error::Conflict { conflict, .. },
                    }) => *conflict,
                    _ => norte_proto::ConflictKind::Exists,
                };
                self.modal = Some(Modal::ConflictResolve {
                    pending: PendingTransfer { kind, from, to },
                    conflict,
                });
            }
            return;
        }
        // Éxito/cancelación: relista los dirs afectados (read-after-write).
        let affected: Vec<VPath> = match &op {
            PendingOp::Transfer { from, to, .. } => {
                [from.parent(), to.parent()].into_iter().flatten().collect()
            }
            PendingOp::Delete { path, .. } => path.parent().into_iter().collect(),
        };
        self.relist_dirs(&affected);
    }

    /// Relista cualquier pane cuyo `dir` esté en `dirs` (read-after-write).
    fn relist_dirs(&mut self, dirs: &[VPath]) {
        for pane in 0..2 {
            let cur = self.panes[pane].dir().clone();
            if dirs.contains(&cur) {
                self.cd(pane, cur, &mut placeholder_cx());
            }
        }
    }
```
   PROBLEMA: `cd` necesita `&mut Context<Self>` y `apply_event` no lo tiene (se llama dentro del `this.update` de `spawn_event_loop`, que SÍ tiene `cx`). Solución: pasa `cx` por la cadena. Cambia `apply_event(&mut self, ev)` a `apply_event(&mut self, ev, cx: &mut Context<Self>)`, `on_task_terminal(&mut self, id, failed_conflict, cx)` y `relist_dirs(&mut self, dirs, cx)`, y en `spawn_event_loop` llama `view.apply_event(ev, cx)`. Entonces `cd(pane, cur, cx)` funciona sin `placeholder_cx`. (Borra la referencia a `placeholder_cx` — era un marcador del error.)

- [ ] **Step 3: cancelación (F9)** — en `on_key`, brazo `Action::CancelTask`:

```rust
            Action::CancelTask => {
                if let Some(id) = self.task_order.get(self.task_cursor).copied() {
                    let _ = self.cmds.send(SessionCmd::Cancel(id));
                }
            }
```
   (El cursor de la franja se mueve con el ratón/rueda en el render; para el spike, F9 cancela la primera task no-terminal — simplifica: si prefieres, usa el primer id cuyo estado no sea terminal en vez de `task_cursor`. Implementación mínima: cancela `task_cursor`, que arranca en 0.)

- [ ] **Step 4: render de la franja** — cambia el layout raíz de `render` para apilar [fila de panes] sobre [franja]:

```rust
impl Render for NorteGui {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panes_row = div()
            .flex_1()
            .flex()
            .flex_row()
            .overflow_hidden()
            .gap(px(2.0))
            .child(self.render_pane(0, cx))
            .child(self.render_pane(1, cx));

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(FG))
            .p(px(4.0))
            .gap(px(2.0))
            .child(panes_row)
            .child(self.render_task_strip())
    }
}
```
   Y añade el render de la franja (sin GPUI interactivo complejo; una fila por task en orden de llegada):

```rust
    /// Franja de tasks al pie: una fila por task (kind + % + estado). El nombre
    /// de la entrada en curso se sanea con `display_name` (nunca crudo).
    fn render_task_strip(&self) -> impl IntoElement {
        let mut strip = div()
            .flex()
            .flex_col()
            .max_h(px(120.0))
            .overflow_hidden()
            .bg(rgb(HEADER_BG))
            .px(px(4.0))
            .py(px(2.0));
        if self.task_order.is_empty() {
            return strip.child(SharedString::from("(sin tasks)"));
        }
        for (i, id) in self.task_order.iter().enumerate() {
            let Some(p) = self.task_progress.get(id) else { continue };
            let sel = i == self.task_cursor;
            let mut row = div()
                .px(px(2.0))
                .child(SharedString::from(task_line(p)));
            if sel {
                row = row.bg(rgb(SEL_BG));
            }
            strip = strip.child(row);
        }
        strip
    }
```
   Y la fn PURA de la línea (testeable):

```rust
/// Línea de una task para la franja: `[copy] 42% running (X)`. `X` = la entrada
/// en curso saneada con `display_name` (jamás bytes crudos). PURA (sin GPUI).
#[must_use]
fn task_line(p: &norte_proto::TaskProgress) -> String {
    use norte_proto::{TaskKind, TaskState};
    let kind = match p.kind {
        TaskKind::Copy => "copy",
        TaskKind::Move => "move",
        TaskKind::Delete => "delete",
        TaskKind::Undo => "undo",
        TaskKind::Search => "search",
        TaskKind::Unknown => "task",
    };
    let pct = match p.entries_total {
        Some(t) if t > 0 => format!("{}%", p.entries_done.saturating_mul(100) / t),
        _ => "…".to_string(),
    };
    let state = match &p.state {
        TaskState::Pending => "pending",
        TaskState::Running => "running",
        TaskState::Paused => "paused",
        TaskState::Completed => "done",
        TaskState::Cancelled => "cancelled",
        TaskState::Failed { .. } => "failed",
        _ => "?",
    };
    let current = p
        .current
        .as_ref()
        .and_then(|v| v.file_name().map(norte_proto::Segment::as_bytes))
        .map(|b| {
            let (name, hostile) = norte_frontend::display_name(b);
            if hostile { format!("{HOSTILE_BADGE} {name}") } else { name }
        })
        .unwrap_or_default();
    format!("[{kind}] {pct} {state} {current}").trim_end().to_string()
}
```

- [ ] **Step 5: test de `task_line` (pure, corpus hostil)** — añade al `mod tests` de `main.rs`:

```rust
    #[test]
    fn task_line_nunca_deja_hazards_crudos_del_corpus_hostil() {
        use norte_proto::{Segment, TaskId, TaskKind, TaskProgress, TaskState, VPath};
        for fixture in norte_testkit::corpus::hostile_names() {
            let seg = match Segment::new(fixture.bytes.clone()) {
                Ok(s) => s,
                Err(_) => continue, // bytes no válidos como segmento (/, NUL)
            };
            let current = VPath::parse("mem:///").unwrap().join(seg);
            let p = TaskProgress {
                task_id: TaskId::new(1),
                kind: TaskKind::Copy,
                state: TaskState::Running,
                bytes_done: 0,
                bytes_total: None,
                entries_done: 1,
                entries_total: Some(2),
                current: Some(current),
            };
            let line = super::task_line(&p);
            assert!(
                !line.chars().any(norte_encoding::is_terminal_hazard),
                "{}: task_line dejó un hazard crudo en {line:?}",
                fixture.id,
            );
        }
    }
```

- [ ] **Step 6: build + clippy + tests** — `cd crates/norte-gui && cargo build -p norte-gui && cargo clippy --bin norte-gui --all-targets -- -D warnings && cargo nextest run --bin norte-gui && cargo fmt`
  Esperado: compila limpio; `task_line_nunca_deja_hazards_crudos_del_corpus_hostil` PASA.

- [ ] **Step 7: verificación manual (progreso + cancel + read-after-write)** — con daemon + GUI + un archivo GRANDE para ver progreso y cancelar:

```bash
head -c 500M /dev/urandom > /tmp/norte-gui-test/a/big.bin
cd crates/norte-gui
NORTE_SOCKET=<socket> NORTE_DIR=file:///tmp/norte-gui-test/a NORTE_GUI_DEBUG=1 cargo run
```
   Copia `big.bin` a `.../b` (F5, `y`) → la franja muestra `[copy] N% running big.bin`; F9 cancela → estado `cancelled`, y en `.../b` NO queda un archivo a medias sin marcar (o queda `.norte-partial`). Tras una copia normal, el pane destino se RELISTA solo (aparece el archivo sin navegar). Borra algo (F8) → desaparece del pane tras terminar.

- [ ] **Step 8: Commit**

```bash
git add crates/norte-gui/src/main.rs
git commit -m "feat(gui): franja de tasks + cancelación + read-after-write (GUI-b T6)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: cierre — reviewers + gate + spec + push

- [ ] **Step 1: reviewers** (el controller orquesta sobre el rango de GUI-b):
  - **rust-reviewer**: sobre `norte-frontend` (marcas) + `norte-gui` (session/modal/input/main). Foco: reglas duras (sin `unwrap` fuera de invariante comentada — el `cancellers.lock().unwrap()` es invariante de mutex no envenenado, coméntalo; `#![forbid(unsafe_code)]` en session.rs — hereda del crate), errores tipados propagados, sin I/O bloqueante en async (el `std::thread` + runtime propio es el patrón aprobado del spike).
  - **encoding-auditor**: sobre `modal.rs` (construye VPaths destino con `join`/`file_name` — que no degrade bytes), `session.rs` (paths por wire) y `task_line`/marcas (nombres hostiles saneados solo al PINTAR, nunca en la identidad de la marca ni en el destino). Debe confirmar: la marca es por VPath EXACTO (bytes), el destino de copia preserva los bytes del nombre origen, el render sanea con `display_name`.
  - **security-reviewer**: sobre `session.rs` (conexión persistente al daemon como `User`; que no haya un segundo canal de autoridad; `delete` default Trash; cancelación fire-and-forget). Confirmar que la GUI no introduce un atajo fuera de `policy` (regla 9) — todo va por `RemoteBackend`.
  Aplica los hallazgos con TDD donde toquen lógica pura.

- [ ] **Step 2: gate del workspace** — `just ci` EXIT=0 (cubre `norte-frontend` con las marcas + que la TUI compila). OJO rustdoc: enlaces `[`x`]` a items privados rompen el doc build — revisa los rustdoc nuevos de `pane.rs`/`modal.rs`/`session.rs`.

- [ ] **Step 3: gate de norte-gui (excluido)** — `cd crates/norte-gui && cargo build -p norte-gui && cargo clippy --bin norte-gui --all-targets -- -D warnings && cargo nextest run --bin norte-gui && cargo fmt --check`
  Esperado: todo verde.

- [ ] **Step 4: cierra el spec** — en `docs/superpowers/specs/2026-07-20-gui-b-mutaciones-dual-pane-design.md`, cambia Estado a **IMPLEMENTADO** con la fecha y anota desviaciones reales (marcas por VPath completo en vez de "bytes del nombre"; `SubmitFailed` va a banner sin reintento auto; conflicto detectado en el TERMINAL de la task, no en el submit; franja sin scroll fino; deudas del spec «Deuda esperada»).

- [ ] **Step 5: actualiza el índice de deuda** — si el reviewer o la implementación dejan deuda nueva (p. ej. cancel por cursor de franja limitado, batch de ops), abre issues con `gh issue create` y referéncialas en el commit de cierre.

- [ ] **Step 6: Commit + push**

```bash
git add -A
git commit -m "test(gui): cierre GUI-b — reviewers + gate + spec IMPLEMENTADO (GUI-b T7)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
git push
```

---

## Self-review del plan (hecho)

- **Cobertura del spec:** Backend persistente compartido → T4 (List) + T5 (mutaciones), cierra deuda T4 (§Decisión 1). Marcas multi-select en PaneState compartido → T1 (§Decisión 2, Componente B). Modal de conflicto → T3 (modal) + T5 (detección en terminal) (§Decisión 3). Franja de tasks al pie → T6 (§Decisión 4, Componente E). Confirmaciones UX no-policy → T3/T5 (GUI conecta como User, sin prompts de policy) (§Decisión 5). Cancelación → T5 (session Cancel) + T6 (F9) (§Decisión 6). Read-after-write → T6. Errores sin panic → todas las ramas emiten evento/banner. Testing: marcas TDD gate (T1), modal/input/task_line puras (T2/T3/T6), verificación manual (T4/T5/T6). Fuera de alcance (mkdir/rename, keymap, viewer, i18n, undo) no tocado.
- **Tipos consistentes:** `PendingOp`/`TransferKind`/`PendingTransfer` definidos en `modal.rs` (T3), consumidos por `session.rs` (T5) y `main.rs` (T5/T6). `SessionCmd`/`SessionEvent` en `session.rs` (T4 base + T5 amplía). `Modal`/`ModalOutcome`/`on_key`/`dest_for` en `modal.rs`. `apply_event(&mut self, ev, cx)` firma final fijada en T6 (T4 la introduce sin `cx`, T6 le añade `cx` para el read-after-write — anotado explícitamente en T6 Step 2). `marked_paths`/`toggle_mark`/`is_marked`/`marks_len`/`clear_marks` en `PaneState` (T1). `task_line` PURA en `main.rs` (T6).
- **Placeholders:** ninguno pendiente. Los dos marcadores ilustrativos (`let backend_task_removed = ();` en T4, `placeholder_cx()` en T6) llevan instrucción EXPLÍCITA de borrado en su propio step. El `#![allow(dead_code)]` de `modal.rs` es temporal con instrucción de retirada en T5 Step 7.
- **Riesgo acotado:** T1 toca el workspace (gate verde exigido); T2–T6 son `norte-gui` excluido (gate propio en su dir). El `Pane` de la TUI NO se toca (deuda #82). La migración de `cd` (T4) mantiene el read-only verde ANTES de añadir mutaciones (T5), con verificación manual de regresión en T4 Step 6.
