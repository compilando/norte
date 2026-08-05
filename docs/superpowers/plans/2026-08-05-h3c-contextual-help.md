# H3c — contextual help and the palette bridge

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `F1` opens the page about where the reader IS — the viewer's page from the viewer, the copying page from a collision dialog, the agents page from an approval prompt — and a palette row can jump to the page that documents its command.

**Architecture:** The context→topic mapping stays in the corpus (`context:` front matter), as the design says; the TUI owns a closed vocabulary of context ids with a compile anchor over `Modal`, so a new modal cannot ship without someone deciding which page explains it. Key ownership between the help and a modal is decided by which arrived first, recorded on `HelpView`.

**Tech Stack:** Rust, `norte-help` (corpus + lookup), ratatui, Fluent, nextest, the documentation gate.

**Spec:** `docs/superpowers/specs/2026-08-04-help-system-redesign-design.md`, phase H3c. Two decisions this plan makes that the spec leaves open are written down in it as an amendment by Task 1.

---

## The two decisions, and why

**1. Context granularity: per modal, not per screen.** Today the vocabulary is three ids tied to `Screen` (`browse`/`viewer`/`dialog`) with a compile anchor in `crates/norte-tui/tests/help_gate.rs`. The spec promises the approval modal opens *Agents & policy* and the collision dialog opens *Copying* — different pages from the same `Screen::Dialog`. So the vocabulary grows to one id per modal, anchored on `Modal` rather than `Screen`: a new modal variant then fails to compile until someone names its page. Contexts with no page yet go on a shrinking allowlist, the idiom the documentation gate already uses.

**2. Key ownership when the help covers a modal: whoever arrived first.** `modal_wins` currently gives every key to an open modal, so `F1` over an approval prompt never reaches the help at all. The missing distinction is direction:

- help opened FROM a modal → the help owns the keys; `Esc` closes only the help and the modal is untouched; the modal's own verbs (`y`/`n`) are unreachable until the help closes;
- a modal ARRIVING over an open help → the help closes and the modal takes the screen, exactly as the palette and the settings overlay already do. An agent's approval prompt may never sit hidden under a help page.

Consequence to document, not to fix: leave the help open over an approval and its TTL (60s) expires, denying the agent. That is fail-closed, which is the right direction, and it is the reason the second half of the rule exists.

## File structure

| File | Responsibility |
| --- | --- |
| `crates/norte-help/src/corpus.rs` | `topic_for_context(lang, context)` — the corpus half of the lookup. |
| `crates/norte-help/src/check.rs` | The missing half of `check_contexts_in`: a known context with NO topic is an issue (`Issue::ContextWithoutTopic`). |
| `crates/norte-tui/src/help_context.rs` (new) | The TUI's closed context vocabulary, the `Modal`→context map, its compile anchor, and `help_context(app)`. One file so the anchor sits next to the table it guards. |
| `crates/norte-tui/src/app.rs` | `HelpView::over_modal`; `HelpView::opened_at`. |
| `crates/norte-tui/src/main.rs` | `Command::AppHelp` opens the contextual topic; key ownership; a modal arriving closes a non-`over_modal` help. |
| `crates/norte-tui/tests/help_gate.rs` | The context vocabulary comes from `help_context`; the allowlist for contexts without a page. |
| `crates/norte-help/topics/{en,es}/*.md` | `context:` entries for the pages that exist. |

---

### Task 1: the corpus can answer "which page explains this context", and the gate notices when nothing does

**Files:**
- Modify: `crates/norte-help/src/corpus.rs`, `crates/norte-help/src/check.rs`, `crates/norte-help/src/lib.rs`
- Modify: `docs/superpowers/specs/2026-08-04-help-system-redesign-design.md` (amendment)

- [ ] **Step 1: Write the failing tests**

In `crates/norte-help/src/corpus.rs`'s test module:

```rust
    #[test]
    fn un_contexto_declarado_resuelve_a_su_tema() {
        // `panes` declara `context = ["browse"]`: F1 en el pane abre esa
        // página, y el mapa vive en el CORPUS, no en código de frontend.
        let t = topic_for_context(Lang::En, "browse").expect("browse tiene página");
        assert_eq!(t.id.as_str(), "panes");
        // Y en el otro locale resuelve al MISMO id: la paridad es estructural.
        let es = topic_for_context(Lang::Es, "browse").expect("browse en es");
        assert_eq!(es.id, t.id);
    }

    #[test]
    fn un_contexto_sin_tema_es_none_no_un_panico() {
        assert!(topic_for_context(Lang::En, "no-existe-este-contexto").is_none());
    }
```

In `crates/norte-help/src/check.rs`'s test module:

```rust
    #[test]
    fn un_contexto_conocido_sin_tema_es_un_hallazgo() {
        // La mitad que faltaba (deuda registrada en H3a): `check_contexts`
        // cazaba contextos inventados y duplicados, pero NO que un contexto
        // que la app sabe abrir se quedara sin página. F1 ahí no abriría
        // nada y ninguna puerta lo decía.
        let issues = check_contexts_in(
            Lang::En,
            crate::corpus::topics(Lang::En),
            &["browse", "un-contexto-huerfano"],
        );
        assert!(
            issues.iter().any(|i| matches!(
                i,
                Issue::ContextWithoutTopic { context, .. } if context == "un-contexto-huerfano"
            )),
            "{issues:?}"
        );
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p norte-help 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Expected: FAIL — `cannot find function topic_for_context`, `no variant ContextWithoutTopic`.

- [ ] **Step 3: Implement the lookup**

In `crates/norte-help/src/corpus.rs`:

```rust
/// The topic that explains `context`, if any topic claims it.
///
/// The mapping lives in the corpus — a topic's `context:` front matter — and
/// not in the frontend, so moving the explanation of a screen from one page
/// to another is an edit to prose rather than a code change. The frontend
/// owns only the vocabulary of context IDS (which screens exist), which is
/// the half it is the authority on.
///
/// `None` is a normal answer: a context whose page has not been written yet.
/// `check_contexts` is what refuses to let that state ship unnoticed.
///
/// ```
/// use norte_help::{Lang, topic_for_context};
///
/// assert_eq!(
///     topic_for_context(Lang::En, "browse").map(|t| t.id.as_str()),
///     Some("panes")
/// );
/// assert!(topic_for_context(Lang::En, "no-such-context").is_none());
/// ```
#[must_use]
pub fn topic_for_context(lang: Lang, context: &str) -> Option<&'static Topic> {
    topics(lang)
        .iter()
        .find(|t| t.context.iter().any(|c| c == context))
}
```

Export it from `lib.rs` next to `topic`.

- [ ] **Step 4: Implement the gate's missing half**

Add the variant to `Issue` in `crates/norte-help/src/check.rs`, following the shape of `UnknownContext` (which carries `topic`, `context`, `lang` and has a `Display`):

```rust
    /// A context the frontend knows how to open, that NO topic claims: `F1`
    /// there would open nothing.
    ///
    /// The mirror of [`Issue::UnknownContext`], and the half H3a left owed.
    /// One of them catches a topic pointing at a screen that does not exist;
    /// this one catches a screen with nothing to say.
    ContextWithoutTopic {
        /// The context id, from the frontend's vocabulary.
        context: String,
        /// Locale swept.
        lang: Lang,
    },
```

and, at the end of `check_contexts_in`, after the existing loops:

```rust
    // The other direction: every context the caller says it can open must
    // have exactly one page. `claimed` already holds who claimed what, so
    // this is a set difference rather than a second sweep of the corpus.
    for context in known {
        if !claimed.iter().any(|(c, _)| c == context) {
            issues.push(Issue::ContextWithoutTopic {
                context: (*context).to_owned(),
                lang,
            });
        }
    }
```

Give it a `Display` arm in the same style as its neighbours (they read like `[En] no topic documents …`), and check whether `Issue` is `#[non_exhaustive]` — if it is not, adding a variant breaks callers' matches, and the ones in `crates/norte-tui/tests/help_gate.rs` and `norte doctor` must be updated in this task.

- [ ] **Step 5: Record the amendment in the spec**

Append to `docs/superpowers/specs/2026-08-04-help-system-redesign-design.md`:

```markdown
## Amendment 2026-08-05 (H3c): context granularity and key ownership

Two things the H3c section left open, decided during implementation.

**Contexts are per MODAL, not per screen.** "F1 in the approval modal opens
*Agents & policy*, in the collision dialog opens *Copying*" asks two different
pages of one `Screen::Dialog`, so the vocabulary is one id per modal, anchored
on `Modal` in `norte-tui/src/help_context.rs`: a new modal variant does not
compile until someone names the page that explains it. Contexts whose page is
not written yet sit on a shrinking allowlist in the documentation gate.

**Key ownership between the help and a modal goes to whoever arrived first.**
Help opened FROM a modal owns the keys, and `Esc` closes only the help — the
modal is untouched, and its own verbs are unreachable until the help closes. A
modal ARRIVING over an open help closes the help, as the palette and settings
overlays already do: an agent's approval prompt may never sit hidden under a
help page. The consequence is deliberate and fail-closed: a help page left
open over an approval lets its TTL expire, which denies the agent.
```

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run -p norte-help 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"` — PASS.
Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"` — the gate may now report `ContextWithoutTopic` for `viewer` and `dialog`, which Task 4 handles. Note what it says; do not silence it here.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-help docs/superpowers/specs
git commit -m "feat(help): topic_for_context, and the gate half that catches a context with no page"
```

---

### Task 2: the TUI's context vocabulary, anchored on `Modal`

**Files:**
- Create: `crates/norte-tui/src/help_context.rs`
- Modify: `crates/norte-tui/src/lib.rs` (`pub mod help_context;`)

- [ ] **Step 1: Write the failing tests**

Create the file with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn app_en_pane() -> App {
        let d = norte_proto::VPath::parse("file:///x").expect("wire de test");
        App::new(
            crate::app::Pane::new(d.clone(), Vec::new()),
            crate::app::Pane::new(d, Vec::new()),
        )
    }

    #[test]
    fn el_pane_es_el_contexto_por_defecto() {
        assert_eq!(help_context(&app_en_pane()), "browse");
    }

    #[test]
    fn un_modal_gana_al_pane_y_cada_uno_tiene_el_suyo() {
        let mut app = app_en_pane();
        app.modal = Some(collision_modal_de_test());
        assert_eq!(help_context(&app), "dialog.collision");
        app.modal = Some(approval_modal_de_test());
        assert_eq!(
            help_context(&app),
            "dialog.approval",
            "la aprobación de un agente no se explica con la página de copiar"
        );
    }

    #[test]
    fn el_visor_gana_al_pane_y_pierde_contra_un_modal() {
        // El orden importa: lo que está ENCIMA es lo que el lector está
        // mirando, y es de eso de lo que necesita que le hablen.
        let mut app = app_en_pane();
        app.viewer = Some(visor_de_test());
        assert_eq!(help_context(&app), "viewer");
        app.modal = Some(collision_modal_de_test());
        assert_eq!(help_context(&app), "dialog.collision");
    }

    #[test]
    fn el_vocabulario_no_tiene_duplicados_ni_huecos() {
        // Un id repetido haría que dos modales compartieran página sin que
        // nadie lo hubiera decidido; uno vacío abriría la nada.
        let mut vistos = std::collections::BTreeSet::new();
        for id in CONTEXTS {
            assert!(!id.is_empty(), "id vacío en el vocabulario");
            assert!(vistos.insert(*id), "id duplicado: {id}");
        }
    }
}
```

Build the three `*_de_test()` helpers from whatever the existing test modules in `crates/norte-tui/src/app.rs` already use for those modals (`grep -n "Modal::Collision {" crates/norte-tui/src/app.rs`) — do not invent new shapes.

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Expected: FAIL — `cannot find function help_context`.

- [ ] **Step 3: Implement**

```rust
//! Which help page belongs to where the reader is standing (H3c).
//!
//! The mapping context→page lives in the CORPUS (`context:` front matter).
//! What lives here is the other half: the closed vocabulary of context ids —
//! which places the app has — and the rule for deciding which one the reader
//! is in right now. The corpus cannot own that: it does not know what a modal
//! is.
//!
//! The anchor below is the point of the module. A new `Modal` variant does not
//! compile until someone extends the map, which is the moment to decide which
//! page explains it — not months later when a reader presses F1 and gets the
//! index.

use crate::app::{App, Modal};

/// Every context id the TUI can be in. The documentation gate cross-checks
/// this against the corpus in both directions: an id no page claims is a
/// finding, and a page claiming an id that is not here is a finding.
pub const CONTEXTS: &[&str] = &[
    "browse",
    "viewer",
    "dialog.confirm",
    "dialog.collision",
    "dialog.approval",
    "dialog.trust-host",
    "dialog.trust-lua",
    "dialog.quit",
    "dialog.mark-pattern",
    "dialog.transfer-name",
    "dialog.mkdir",
    "dialog.ai-rename",
    "dialog.semantic-search",
];

/// The context of `modal`.
///
/// Exhaustive on purpose — see the module docs. Variants that are the same
/// KIND of question share an id (the three confirmations are one page), and
/// that sharing is a decision recorded here rather than an accident of a
/// wildcard arm.
fn modal_context(modal: &Modal) -> &'static str {
    match modal {
        Modal::ConfirmDelete { .. } | Modal::ConfirmTransfer { .. } => "dialog.confirm",
        Modal::ConfirmQuit => "dialog.quit",
        Modal::Collision { .. } => "dialog.collision",
        Modal::ApproveAgentOp { .. } => "dialog.approval",
        Modal::TrustHostKey { .. } => "dialog.trust-host",
        Modal::TrustLuaInit { .. } => "dialog.trust-lua",
        Modal::MarkPattern { .. } => "dialog.mark-pattern",
        Modal::TransferName { .. } => "dialog.transfer-name",
        Modal::Mkdir { .. } => "dialog.mkdir",
        Modal::AiRenameInstruction { .. } | Modal::AiRenamePlan { .. } => "dialog.ai-rename",
        Modal::SemanticQuery { .. } | Modal::SemanticHits { .. } => "dialog.semantic-search",
    }
}

/// Where the reader is: the topmost thing on screen, because that is what
/// they are looking at and what they need explained.
#[must_use]
pub fn help_context(app: &App) -> &'static str {
    if let Some(modal) = app.modal.as_ref() {
        return modal_context(modal);
    }
    if app.viewer.is_some() {
        return "viewer";
    }
    "browse"
}
```

Match the real `Modal` variant list — `awk '/^pub enum Modal/,/^}/' crates/norte-tui/src/app.rs` — and adjust both the `match` and `CONTEXTS` if it differs from the list above. A variant you cannot decide on is not a licence for a wildcard: give it its own id and let Task 4's allowlist carry it.

- [ ] **Step 4: Run the tests**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"` — the four new tests PASS.

- [ ] **Step 5: Commit**

```bash
just c norte-tui && cargo fmt --all
git add crates/norte-tui/src/help_context.rs crates/norte-tui/src/lib.rs
git commit -m "feat(tui): a closed context vocabulary anchored on Modal (H3c)"
```

---

### Task 3: F1 opens the contextual page, and the modal underneath survives

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (`HelpView`)
- Modify: `crates/norte-tui/src/main.rs` (`Command::AppHelp`, the key branches, the modal-arrival path)

- [ ] **Step 1: Write the failing tests**

In `crates/norte-tui/src/main.rs`'s `help_key_tests`:

```rust
    /// F1 sobre un modal abre la página de ESE modal, no el índice.
    #[test]
    fn f1_sobre_un_modal_abre_la_pagina_del_modal() {
        let mut app = app_with_help_closed();
        app.modal = Some(collision_modal_de_test());
        abrir_ayuda(&mut app);
        let help = app.help.as_ref().expect("la ayuda se abrió");
        assert_eq!(help.state.current().as_str(), "copying");
        assert!(help.over_modal, "se abrió ENCIMA de un modal");
        assert!(app.modal.is_some(), "y el modal sigue ahí");
    }

    /// La ayuda abierta desde un modal se queda las teclas, y `Esc` cierra
    /// SOLO la ayuda: el modal no se responde por accidente.
    #[test]
    fn esc_cierra_la_ayuda_y_deja_el_modal_intacto() {
        let mut app = app_with_help_closed();
        app.modal = Some(approval_modal_de_test());
        abrir_ayuda(&mut app);
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Esc);
        assert!(app.help.is_none(), "la ayuda se cerró");
        assert!(
            app.modal.is_some(),
            "una aprobación de agente NO se contesta cerrando una ayuda"
        );
    }

    /// Mientras la ayuda tapa el modal, los verbos del modal son inertes: se
    /// aprueba con la ayuda cerrada, mirándolo.
    #[test]
    fn los_verbos_del_modal_no_se_alcanzan_por_debajo_de_la_ayuda() {
        let mut app = app_with_help_closed();
        app.modal = Some(approval_modal_de_test());
        abrir_ayuda(&mut app);
        let mut r = dialog_resolver();
        press(&mut app, &mut r, KeyCode::Char('y')); // dialog.approve
        assert!(app.modal.is_some(), "no se aprobó nada a ciegas");
    }
```

and in the same module, the other direction:

```rust
    /// Un modal que LLEGA sobre una ayuda abierta la cierra: una aprobación
    /// jamás puede quedarse escondida debajo de una página.
    #[test]
    fn un_modal_que_llega_cierra_la_ayuda() {
        let mut app = app_with_help_closed();
        abrir_ayuda(&mut app); // sin modal: over_modal == false
        assert!(!app.help.as_ref().expect("abierta").over_modal);
        app.modal = Some(approval_modal_de_test());
        cerrar_overlays_obsoletos(&mut app);
        assert!(app.help.is_none(), "la ayuda cede la pantalla");
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"`
Expected: FAIL — `no field over_modal`, `cannot find function abrir_ayuda`.

- [ ] **Step 3: Implement**

`HelpView` gains the flag and a constructor that starts on a topic:

```rust
    /// `true` when the help was opened while a modal was already on screen.
    ///
    /// It decides who owns the keys, and the two directions are different
    /// events: a help opened FROM a modal owns them (the reader asked to read
    /// about the question in front of them, and `Esc` must put them back in
    /// front of it, not answer it), while a modal ARRIVING over an open help
    /// closes the help — an agent's approval prompt may never sit hidden
    /// under a page.
    pub over_modal: bool,
```

```rust
    /// Opens the help on the page for `context`, falling back to the index
    /// when no page claims it.
    ///
    /// The fallback is not a papering-over: `check_contexts` fails the build
    /// for a context with no page, so reaching it means someone shipped past
    /// the gate. The index is the least surprising place to land.
    #[must_use]
    pub fn new_at(lang: norte_help::Lang, keys_lines: Vec<String>, context: &str, over_modal: bool) -> Self {
        let mut view = Self::new(lang, keys_lines);
        view.over_modal = over_modal;
        if let Some(t) = norte_help::topic_for_context(lang, context) {
            view.state.open(&t.id);
            // Opening the contextual page is not navigation the reader did:
            // `Esc` should close the overlay, not walk back to the index they
            // never asked for. `HelpState::open` pushed history, so undo it.
            let _ = view.state.back_len_reset_for_context_open();
        }
        view
    }
```

`back_len_reset_for_context_open` does not exist — decide how to express "arrive at this page with an empty history" in `norte_frontend::help::HelpState` (a `open_as_root(&TopicId)` is the honest name) and implement it there, with a test in that crate: after opening as root, `back()` is `false`.

`Command::AppHelp` becomes:

```rust
        Command::AppHelp => {
            let context = crate::help_context::help_context(app);
            app.help = Some(HelpView::new_at(
                lang,
                help_lines.to_vec(),
                context,
                app.modal.is_some(),
            ));
        }
```

The key branch: the help arm currently sits behind `!modal_wins(app)`. It must run when the help is open AND (`!modal_wins(app)` OR `help.over_modal`). Read the `if`/`else if` chain around `crates/norte-tui/src/main.rs:1725` before editing — the order of those arms is load-bearing, and the modal arm below must NOT also run for the same key.

The modal-arrival path is the arm that already does `app.palette = None; app.settings = None;` when a modal is present. Extract it as `cerrar_overlays_obsoletos(app)` and add: close `app.help` too, but ONLY when `!over_modal`.

- [ ] **Step 4: Run the tests**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"` — PASS.

- [ ] **Step 5: Commit**

```bash
just c norte-tui && cargo fmt --all
git add crates/norte-tui crates/norte-frontend
git commit -m "feat(tui): F1 opens the page for where the reader is (H3c)"
```

---

### Task 4: the gate learns the new vocabulary, and the corpus claims what it can

**Files:**
- Modify: `crates/norte-tui/tests/help_gate.rs`
- Modify: `crates/norte-help/topics/{en,es}/*.md` (the `context:` entries)

- [ ] **Step 1: Point the gate at the real vocabulary**

Replace `CONTEXTOS` and its `Screen` anchor with the vocabulary from Task 2:

```rust
/// Los contextos que la TUI sabe abrir. UNA fuente: el vocabulario cerrado de
/// `norte_tui::help_context`, anclado a `Modal` allí. Duplicar la lista aquí
/// sería la tercera copia que se desincroniza.
fn contextos() -> Vec<&'static str> {
    norte_tui::help_context::CONTEXTS.to_vec()
}

/// Contextos que todavía no tienen página. Se borran, uno a uno, conforme
/// H3h escribe el corpus — a mano y con techo, como [`PENDIENTES`]: una lista
/// derivada del propio corpus taparía la regresión por construcción.
const CONTEXTOS_PENDIENTES: &[&str] = &[
    // …los que Task 4 no pueda mapear a una página existente…
];

const _: () = assert!(
    CONTEXTOS_PENDIENTES.len() <= 13,
    "la allowlist de contextos solo puede MENGUAR: escribe la página en vez \
     de añadir el contexto aquí"
);
```

and make `los_contextos_del_corpus_son_pantallas_que_la_tui_tiene` assert BOTH directions, filtering `ContextWithoutTopic` through the allowlist and reporting a stale allowlist entry as a failure — the shape `todo_comando_del_vocabulario_esta_documentado_o_en_pendientes` already has. Delete the note in that test that says the "zero topics" half is owed: it is owed no longer.

- [ ] **Step 2: Run it and see the real gap**

Run: `just t norte-tui --test help_gate 2>&1 | tail -20; echo "EXIT=$pipestatus[1]"`
Expected: FAIL, listing every context with no page. Write that list down — it is the work.

- [ ] **Step 3: Claim the contexts that have a page today**

For each context whose page EXISTS, add the `context:` entry to that topic's front matter in BOTH locales (the sets must match; locale parity is structural):

- `dialog.collision` → `copying.md` (the collision dialog is the copying page's own subject)
- `dialog.confirm` → `copying.md`? Decide: a delete confirmation and a transfer confirmation are both explained there. If one topic ends up claiming three contexts, that is fine — a topic may claim several; the constraint is that a CONTEXT has exactly one topic.
- `viewer` → no page exists. Leave it on the allowlist; do NOT write the viewer page here (H3h owns the corpus).
- `dialog.approval` → no page exists (the agents/policy page is H3h's). Allowlist.

Whatever you decide, the rule is: a context gets claimed only if the page really explains it. A page that does not mention approvals must not claim `dialog.approval` to silence a gate.

- [ ] **Step 4: Set the allowlist to exactly what is left, and the ceiling to its length**

- [ ] **Step 5: Verify**

Run: `just t norte-tui 2>&1 | tail -5; echo "EXIT=$pipestatus[1]"` and `cargo nextest run -p norte-help 2>&1 | tail -3; echo "EXIT=$pipestatus[1]"` — both green.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui/tests/help_gate.rs crates/norte-help/topics
git commit -m "feat(help): the gate checks both directions of the context map (H3c)"
```

---

### Task 5: the palette can jump to the page that documents a row

**Files:**
- Modify: `crates/norte-help/src/corpus.rs` (`topic_for_command`)
- Modify: `crates/norte-tui/src/main.rs` (the palette key branch)
- Modify: `crates/norte-i18n/i18n/{en,es}.ftl` (the palette hint)

- [ ] **Step 1: Write the failing tests**

In `crates/norte-help/src/corpus.rs`:

```rust
    #[test]
    fn un_comando_resuelve_a_la_pagina_que_lo_documenta() {
        let t = topic_for_command(Lang::En, "pane.copy").expect("pane.copy está documentado");
        assert_eq!(t.id.as_str(), "copying");
        assert!(topic_for_command(Lang::En, "no.such.command").is_none());
    }
```

In `crates/norte-tui/src/main.rs`'s palette tests (or a new module if there are none):

```rust
    /// F1 sobre una fila de la palette abre la página que documenta ese
    /// comando: los dos son vistas del mismo modelo a dos densidades, así
    /// que cruzar de la rápida a la que explica no debería costar re-teclear.
    #[test]
    fn f1_en_la_palette_abre_la_pagina_del_comando_bajo_el_cursor() {
        let mut app = app_with_palette_on("pane.copy");
        let abierto = palette_help_target(&app).expect("pane.copy tiene página");
        assert_eq!(abierto.as_str(), "copying");
    }

    /// Una fila SIN página no abre nada y lo dice: mejor que abrir el índice
    /// y dejar al lector buscando qué tenía que ver con lo que pidió.
    #[test]
    fn una_fila_sin_pagina_lo_dice() {
        let app = app_with_palette_on("app.theme"); // en PENDIENTES hoy
        assert!(palette_help_target(&app).is_none());
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p norte-help 2>&1 | tail -3` then `just t norte-tui 2>&1 | tail -5`; both FAIL on the missing functions.

- [ ] **Step 3: Implement**

```rust
/// The topic that documents `command`, if one names it.
///
/// The reverse of what the documentation gate walks: it checks that every
/// command IS named by some topic, and this is the lookup that makes the
/// check pay off at runtime — the palette's row for a command can open its
/// page. First claimant wins; the gate guarantees at least one exists for
/// every command outside its allowlist.
#[must_use]
pub fn topic_for_command(lang: Lang, command: &str) -> Option<&'static Topic> {
    topics(lang)
        .iter()
        .find(|t| t.commands.iter().any(|c| c == command))
}
```

In the palette key branch, add `F1` (and only `F1` — the palette's keys are fixed, it is a free-text filter): read the selected row's command, resolve it, and if there is a page, close the palette and open the help on that page; otherwise set `app.message` to a new Fluent string `msg-palette-no-help`. Add that string to both locales, and extend `palette-hint` so the affordance is discoverable.

- [ ] **Step 4: Run the tests, lint, commit**

```bash
just t norte-tui && just c norte-tui && cargo nextest run -p norte-help
cargo fmt --all
git add crates/norte-help crates/norte-tui crates/norte-i18n
git commit -m "feat(tui): F1 on a palette row opens the page for that command (H3c)"
```

---

### Task 6: drive it, write it down, gate it

- [ ] **Step 1: Drive the real app**

```bash
cargo build -q -p norte-tui
tmux kill-session -t h3c 2>/dev/null
tmux new-session -d -s h3c -x 113 -y 30 -c "$PWD" "NORTE_LANG=es target/debug/norte-tui"
sleep 2
```

Then, capturing after each step: `F1` from the pane (must open *Two panes*), `Esc`, `F3` to open the viewer and `F1` there (the viewer context — allowlisted, so the index is the honest fallback), `Esc` twice, mark a file and press the copy key onto a colliding name to raise the collision dialog, `F1` there (must open *Copying*), `Esc` (the collision dialog must still be there), then answer it. Finally `Ctrl+P`, cursor to a documented command, `F1` (must open its page).

Paste the captures. Any anomaly becomes a failing test first, then a fix.

- [ ] **Step 2: Changelog**

Add to `CHANGELOG.md` under `## [Unreleased]` → `### Added`, in the file's voice, covering: F1 opens the page about where you are; from a dialog it explains that dialog and `Esc` puts you back in front of it without answering it; a dialog that arrives while you are reading closes the help rather than hiding behind it (and why: an approval you cannot see is an approval you cannot answer); and F1 on a palette row opens that command's page. Say plainly which contexts do not have their own page yet.

- [ ] **Step 3: Full gate**

Run: `just ci > /tmp/ci.log 2>&1; echo "EXIT=$status"; grep -E "^error|Summary" /tmp/ci.log | tail -4`
Expected `EXIT=0`. Do not trust a background run's reported exit code — grep the log.

- [ ] **Step 4: Reviewers**

Dispatch `rust-reviewer` and `security-reviewer` over the branch diff. For security, the question to put plainly: can a help page opened over an approval prompt cause that prompt to be answered, hidden, or dropped without the reader deciding? Apply BLOCKER and MAJOR findings before merging.

---

## Self-review notes

- **Spec coverage.** Contextual F1: Tasks 2 and 3. `Esc` returns to the modal: Task 3. Palette bridge: Task 5 (the filter half already shipped in H3b — `Ctrl+P` from the help carries the filter). The corpus keeps owning the mapping: Task 1. The gate half H3a owed: Tasks 1 and 4.
- **Deliberately not here:** writing the pages for `viewer` and `dialog.approval` (H3h owns the corpus; they sit on a shrinking allowlist), and the GUI's help view (H3f).
- **Known risk.** Task 3 changes who owns a key while a modal is open, which is the mechanism H1 built to stop an async approval being swallowed by an overlay. That is why the security review in Task 6 is not optional, and why the modal-arrival direction has its own test.
