# H3a — `norte-help` crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `crates/norte-help` — an embedded, localized help corpus with a
markdown-lite parser (trusted and hostile modes), a render-agnostic model, and
integrity checks that fail the build when documentation drifts from the code.

**Note on code fences:** several code blocks below contain triple-backtick
markdown themselves. Those blocks are fenced with **four** backticks. Keep
that when editing this plan.

**Architecture:** Topics are markdown files with TOML front matter, embedded
with `include_str!` (same pattern as `norte-theme` presets and `norte-i18n`
catalogs). A line-based parser turns them into typed blocks and spans. Two
live marks — `{{cmd:id}}` and `[[topic]]` — stay unresolved in the model and
are resolved at render time by whichever frontend owns the effective keymap,
through a `ChordResolver` trait. The same parser reads plugin-supplied
`help.md` in a hostile mode with hard caps, lossy UTF-8 decoding and hazard
masking. No UI code and no frontend dependency in this phase.

**Tech Stack:** Rust 2024, `serde`/`toml` (front matter), `thiserror`,
`norte-i18n` (`Lang`), `norte-encoding` (hazard masking), `norte-testkit`
(hostile fixtures, dev-dep). No new external crates.

**Spec:** `docs/superpowers/specs/2026-08-04-help-system-redesign-design.md`

---

## File structure

| File | Responsibility |
|---|---|
| `docs/adr/0040-help-corpus-and-markdown-lite.md` | Decision record: new crate, corpus format, TOML front matter, two parse modes |
| `crates/norte-help/Cargo.toml` | Manifest; workspace lints; no new external deps |
| `crates/norte-help/src/lib.rs` | Crate docs, module wiring, public re-exports |
| `crates/norte-help/src/model.rs` | `TopicId`, `Topic`, `Block`, `Span`, `Callout`, `Origin`, `Availability`, `CommandRow` |
| `crates/norte-help/src/front_matter.rs` | Split `+++` fences, parse the TOML header into `FrontMatter` |
| `crates/norte-help/src/parse.rs` | Line-based block parser + inline span parser + `Limits` (hostile mode) |
| `crates/norte-help/src/corpus.rs` | `include_str!` table, lazy parse, lookup by `Lang`/id/tag |
| `crates/norte-help/src/resolve.rs` | `ChordResolver` trait + `resolve_row`/`rows_of` |
| `crates/norte-help/src/check.rs` | Integrity checks returning `Vec<Issue>` (corpus, links, commands, contexts) |
| `crates/norte-help/topics/{en,es}/*.md` | Seed corpus, six topics per locale |
| `crates/norte-help/tests/corpus.rs` | Corpus integrity suite |
| `crates/norte-help/tests/hostile.rs` | Hostile-mode parser matrix |
| `crates/norte-tui/tests/help_gate.rs` | The documentation gate: every `COMMANDS` entry documented, with a shrinking allowlist |
| `Cargo.toml` | Workspace member + workspace dependency entry |
| `ARCHITECTURE.md` | Repository map entry for the new crate |

---

### Task 1: ADR and crate scaffold

**Files:**
- Create: `docs/adr/0040-help-corpus-and-markdown-lite.md`
- Create: `crates/norte-help/Cargo.toml`
- Create: `crates/norte-help/src/lib.rs`
- Modify: `Cargo.toml` (workspace `members` list, `workspace.dependencies`)
- Modify: `ARCHITECTURE.md`

- [ ] **Step 1: Write the ADR**

Create `docs/adr/0040-help-corpus-and-markdown-lite.md` following the MADR
shape used by the other records in `docs/adr/` (read `0036-effects-schema-v1.md`
for the house format before writing). Content:

```markdown
# 0040 — Help corpus and markdown-lite

- Status: accepted
- Date: 2026-08-04
- Deciders: norte maintainers

## Context

Help is a flat list of key bindings in one frontend (`norte-tui/src/help.rs`).
The GUI has none, the CLI has none, and plugins cannot document themselves.
The redesign (spec `2026-08-04-help-system-redesign-design.md`) needs prose
that lives somewhere: localized, embedded in the binary, renderable by
ratatui, GPUI and plain text alike, and safe to accept from a third-party
plugin.

## Decision

1. A new workspace crate `norte-help` owns the help model. It depends on no
   frontend and on no part of `norte-core`; frontends and the CLI depend on
   it.
2. Topics are markdown files with **TOML front matter between `+++` fences**,
   embedded via an explicit `include_str!` table. TOML because the workspace
   already parses TOML everywhere; a YAML dependency for six header fields
   would not survive rule 8. An explicit table because that is what
   `norte-theme` presets and `norte-i18n` catalogs already do — a test
   cross-checks the table against the directory listing.
3. The markdown accepted is a **closed subset** (headings, paragraphs,
   bullets, fenced code, tables, callouts; inline strong/emph/code plus two
   custom marks). No HTML, no autolinked URLs, no images, no nesting beyond
   one level. A closed subset is what makes a third-party document safe to
   render in a terminal.
4. Two live marks stay **unresolved in the model**: `{{cmd:id}}` and
   `[[topic]]`. Resolution happens at render time against the effective
   keymap, so prose can never claim a key the user has rebound.
5. Two parse modes. Trusted (built-in corpus): errors are hard, and a test
   parses the whole corpus so a malformed topic cannot ship. Untrusted
   (plugin `help.md`): never fails, applies caps (64 KiB, block count, line
   length), decodes invalid UTF-8 lossily, and masks terminal hazards at parse
   time, recording `truncated`/`lossy` flags for the UI badge.
6. Masking reuses `norte_encoding::is_terminal_hazard`, already the single
   source of the hazard set (`norte-frontend`'s `must_mask` is a one-line
   delegate to it). `norte-help` must not depend on `norte-frontend`; the
   dependency runs the other way.

## Consequences

- New crate in the workspace map and in the build graph, with no new external
  dependencies.
- A documentation gate becomes possible: a test asserts every command in
  `COMMANDS` appears in at least one topic. Adding a command now costs a
  paragraph. That friction is deliberate.
- Plugin help is cosmetic and stays out of the approval digest (P1
  precedent); the safety argument rests on the closed subset, the caps and
  the masking, not on consent.
- A richer markdown feature later means extending a closed vocabulary, which
  is a deliberate act rather than an accidental capability.

## Alternatives considered

- **A module inside `norte-frontend`.** Rejected: the CLI and `doctor` need
  the corpus without pulling in presentation state, `norte-frontend` is
  already large, and the corpus needs its own asset directory.
- **Fluent for prose.** Rejected: Fluent is built for interface strings, not
  for pages of documentation; the catalog would grow by thousands of lines
  and lose all structure.
- **Files read from disk at runtime.** Rejected: breaks the self-contained
  binary, adds path resolution and I/O failures to a help screen.
- **A full CommonMark crate.** Rejected: a large dependency whose whole point
  is accepting everything, which is the opposite of what a hostile-input
  renderer wants.
```

- [ ] **Step 2: Create the crate manifest**

Create `crates/norte-help/Cargo.toml`:

```toml
[package]
name = "norte-help"
description = "Corpus de ayuda embebido y localizado: temas en markdown-lite con front matter TOML, modelo agnóstico del render y modo hostil para la ayuda de plugins"
license = "MIT OR Apache-2.0"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
repository.workspace = true
authors.workspace = true

[dependencies]
norte-encoding.workspace = true
norte-i18n.workspace = true
serde = { workspace = true, features = ["derive"] }
thiserror.workspace = true
toml.workspace = true

[dev-dependencies]
norte-testkit.workspace = true

[lints]
workspace = true
```

- [ ] **Step 3: Create the crate root**

Create `crates/norte-help/src/lib.rs`:

```rust
//! Corpus de ayuda de norte (ADR 0040): temas en markdown-lite con front
//! matter TOML, embebidos en el binario y localizados.
//!
//! Este crate NO pinta nada: devuelve un modelo tipado ([`Block`]/[`Span`])
//! que cada frontend renderiza con su propia tecnología (ratatui, GPUI,
//! texto plano). Las dos marcas vivas del corpus —`{{cmd:id}}` y
//! `[[tema]]`— llegan SIN resolver: el chord se resuelve al pintar contra
//! el keymap efectivo del usuario, así la prosa jamás miente sobre teclas.
//!
//! ```
//! use norte_help::{Lang, topic};
//! let t = topic(Lang::En, "index").expect("el índice existe");
//! assert_eq!(t.title, "Welcome to norte");
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod check;
mod corpus;
mod front_matter;
mod model;
mod parse;
mod resolve;

pub use check::{Issue, check_commands, check_contexts, check_corpus};
pub use corpus::{topic, topic_ids, topics};
pub use model::{
    Availability, Block, Callout, CommandRow, Origin, Reason, Span, Topic, TopicId,
};
pub use norte_i18n::Lang;
pub use parse::{Limits, ParseError, Parsed, parse_trusted, parse_untrusted};
pub use resolve::ChordResolver;
```

This will not compile until later tasks add the modules; Step 4 only checks
the manifest is wired into the workspace.

- [ ] **Step 4: Wire the workspace**

In the root `Cargo.toml`, add `"crates/norte-help",` to `[workspace] members`
immediately after `"crates/norte-frontend",`, and add to
`[workspace.dependencies]` immediately after the `norte-frontend` line:

```toml
norte-help = { path = "crates/norte-help", version = "0.3.0-alpha.2" }
```

In `ARCHITECTURE.md`, add a row for the crate next to the other frontend-side
crates, describing it as "help corpus and markdown-lite model, consumed by
TUI/GUI/CLI".

- [ ] **Step 5: Verify the manifest resolves**

Run: `cargo metadata --no-deps --format-version 1 | grep -c norte-help`
Expected: `1` (the crate is a workspace member; the code does not compile yet
because the modules are missing — that is Task 2's job).

- [ ] **Step 6: Commit**

```bash
git add docs/adr/0040-help-corpus-and-markdown-lite.md crates/norte-help/Cargo.toml crates/norte-help/src/lib.rs Cargo.toml ARCHITECTURE.md
git commit -m "docs(adr): 0040 help corpus and markdown-lite; scaffold norte-help"
```

---

### Task 2: The model types

**Files:**
- Create: `crates/norte-help/src/model.rs`

- [ ] **Step 1: Write the failing test**

Add at the bottom of `crates/norte-help/src/model.rs` (create the file with
this test first, implementation comes in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_id_normaliza_y_muestra() {
        let id = TopicId::new("copying");
        assert_eq!(id.as_str(), "copying");
        assert_eq!(id.to_string(), "copying");
    }

    #[test]
    fn una_fila_indisponible_lleva_su_motivo() {
        let row = CommandRow {
            command: "fs.copy".to_owned(),
            avail: Availability::Unavailable {
                reason: Reason::ReadOnlyBackend,
            },
        };
        assert!(!row.avail.is_available());
        assert_eq!(
            row.avail.reason(),
            Some(Reason::ReadOnlyBackend),
            "la UI necesita el motivo para explicarlo, no solo el hecho"
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p norte-help`
Expected: FAIL — compilation error, `TopicId`/`CommandRow`/`Availability` not
found.

- [ ] **Step 3: Write the implementation**

Put this above the test module in `crates/norte-help/src/model.rs`:

```rust
//! Modelo del corpus: lo que el parser produce y lo que cada frontend
//! renderiza. Deliberadamente sin nada de UI — ni colores, ni anchos, ni
//! tipos de ratatui/GPUI (regla 7).

use std::fmt;

/// Identificador de un tema (`id` del front matter), único por corpus.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TopicId(String);

impl TopicId {
    /// Construye un id a partir de cualquier cosa que sea texto.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// El id como `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TopicId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// De dónde sale un tema: del binario o de un plugin de terceros.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Tema del corpus embebido: texto CONFIABLE, jamás enmascarado.
    BuiltIn,
    /// Tema de un `help.md` de plugin: texto de TERCEROS, ya enmascarado y
    /// acotado por el parser (ver `parse_untrusted`).
    Plugin {
        /// Id del plugin en el catálogo.
        id: String,
        /// `publisher` del manifiesto, si lo declara.
        publisher: Option<String>,
        /// El contenido excedió algún tope y se recortó.
        truncated: bool,
        /// El fichero no era UTF-8 válido y se decodificó con pérdida.
        lossy: bool,
    },
}

/// Tipo de aviso de un [`Block::Callout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Callout {
    /// Nota neutra.
    Note,
    /// Advertencia (algo puede salir mal).
    Warn,
    /// Truco (algo va más rápido).
    Tip,
}

/// Fragmento en línea dentro de un bloque.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Span {
    /// Texto llano.
    Text(String),
    /// Énfasis fuerte (`**así**`).
    Strong(String),
    /// Énfasis (`*así*`).
    Emph(String),
    /// Código en línea (`` `así` ``).
    Code(String),
    /// Referencia a un comando (`{{cmd:fs.copy}}`), SIN resolver: el chord
    /// lo pone el frontend con [`crate::ChordResolver`].
    CommandRef(String),
    /// Salto a otro tema (`[[selection]]`), SIN resolver.
    TopicLink(TopicId),
}

/// Bloque de contenido. Vocabulario CERRADO (ADR 0040): que un `help.md`
/// hostil no pueda expresar más que esto es justo lo que lo hace seguro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// Encabezado de nivel 1..=3.
    Heading {
        /// Nivel, saturado a 1..=3.
        level: u8,
        /// Texto del encabezado.
        text: String,
    },
    /// Párrafo.
    Paragraph(Vec<Span>),
    /// Lista de puntos (un nivel, sin anidar).
    Bullets(Vec<Vec<Span>>),
    /// Bloque de código con lenguaje opcional.
    Code {
        /// Etiqueta de lenguaje de la valla, si la hay.
        lang: Option<String>,
        /// Contenido literal, sin interpretar marcas.
        text: String,
    },
    /// Tabla simple con cabecera.
    Table {
        /// Celdas de la cabecera.
        header: Vec<String>,
        /// Filas, cada una con el mismo número de celdas que la cabecera.
        rows: Vec<Vec<String>>,
    },
    /// Aviso destacado.
    Callout {
        /// Tipo de aviso.
        kind: Callout,
        /// Contenido del aviso.
        spans: Vec<Span>,
    },
}

/// Por qué un comando no puede ejecutarse ahora mismo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// El backend del pane activo es de solo lectura (p. ej. dentro de un zip).
    ReadOnlyBackend,
    /// El backend no ofrece esa capacidad.
    Unsupported,
    /// El plugin dueño del comando está desactivado o sin aprobar.
    PluginInactive,
    /// La policy lo niega para el actor actual.
    PolicyDenied,
    /// La conexión está degradada.
    ConnectionDegraded,
}

/// Disponibilidad de una fila de comando en el contexto ACTUAL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// Se puede ejecutar ahora.
    Available,
    /// No se puede, con motivo para explicarlo.
    Unavailable {
        /// Motivo mostrado junto a la fila atenuada.
        reason: Reason,
    },
}

impl Availability {
    /// `true` si la fila puede ejecutarse.
    #[must_use]
    pub fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    /// El motivo, si la fila está indisponible.
    #[must_use]
    pub fn reason(self) -> Option<Reason> {
        match self {
            Self::Available => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }
}

/// Fila ejecutable de un tema: un comando que el usuario puede lanzar desde
/// la ayuda con la MISMA vía de despacho que la palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandRow {
    /// Id del comando (`fs.copy`, `plugin:<id>:<cmd>`).
    pub command: String,
    /// Disponibilidad en el contexto actual (la inyecta el frontend).
    pub avail: Availability,
}

/// Un tema del corpus, ya parseado.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topic {
    /// Id único.
    pub id: TopicId,
    /// Título mostrado.
    pub title: String,
    /// Etiquetas de agrupación en el índice.
    pub tags: Vec<String>,
    /// Temas relacionados.
    pub see_also: Vec<TopicId>,
    /// Comandos que el tema documenta, en orden de aparición deseada.
    pub commands: Vec<String>,
    /// Contextos de UI que abren ESTE tema con F1.
    pub context: Vec<String>,
    /// Cuerpo.
    pub blocks: Vec<Block>,
    /// Procedencia.
    pub origin: Origin,
}
```

Add `mod model;` to `lib.rs` if Task 1 Step 3 has not been applied yet (it
has: the module list is already there).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p norte-help`
Expected: FAIL still — `lib.rs` declares modules that do not exist yet
(`check`, `corpus`, `front_matter`, `parse`, `resolve`). To keep this task
green on its own, temporarily comment out those `mod`/`pub use` lines in
`lib.rs`, leaving only `mod model;` and the `model` re-export. Each later task
uncomments its own line.

Re-run: `cargo nextest run -p norte-help`
Expected: PASS, 2 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-help/src/model.rs crates/norte-help/src/lib.rs
git commit -m "feat(help): typed model for topics, blocks, spans and availability"
```

---

### Task 3: Front matter

**Files:**
- Create: `crates/norte-help/src/front_matter.rs`
- Modify: `crates/norte-help/src/lib.rs` (uncomment `mod front_matter;`)

- [ ] **Step 1: Write the failing test**

Create `crates/norte-help/src/front_matter.rs` with only this test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "+++\n\
id = \"copying\"\n\
title = \"Copying across backends\"\n\
tags = [\"doing\"]\n\
see_also = [\"selection\"]\n\
commands = [\"fs.copy\"]\n\
+++\n\
Body starts here.\n";

    #[test]
    fn separa_cabecera_y_cuerpo() {
        let (fm, body) = split(SRC).expect("front matter válido");
        assert_eq!(fm.id, "copying");
        assert_eq!(fm.title, "Copying across backends");
        assert_eq!(fm.tags, vec!["doing".to_owned()]);
        assert_eq!(fm.see_also, vec!["selection".to_owned()]);
        assert_eq!(fm.commands, vec!["fs.copy".to_owned()]);
        assert!(fm.context.is_empty(), "campo opcional, por defecto vacío");
        assert_eq!(body, "Body starts here.\n");
    }

    #[test]
    fn sin_valla_de_apertura_es_error() {
        let err = split("id = \"x\"\nbody").unwrap_err();
        assert!(matches!(err, FrontMatterError::Missing));
    }

    #[test]
    fn valla_sin_cerrar_es_error() {
        let err = split("+++\nid = \"x\"\nbody\n").unwrap_err();
        assert!(matches!(err, FrontMatterError::Unterminated));
    }

    #[test]
    fn campo_desconocido_es_error() {
        let src = "+++\nid = \"x\"\ntitle = \"X\"\nbogus = 1\n+++\nbody\n";
        assert!(matches!(
            split(src).unwrap_err(),
            FrontMatterError::Toml(_)
        ));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Uncomment `mod front_matter;` in `lib.rs`, then run:
`cargo nextest run -p norte-help front_matter`
Expected: FAIL — `split`, `FrontMatterError` not found.

- [ ] **Step 3: Write the implementation**

Above the test module in `crates/norte-help/src/front_matter.rs`:

```rust
//! Cabecera de un tema: TOML entre vallas `+++` (ADR 0040 decisión 2). TOML
//! y no YAML porque el workspace ya parsea TOML en todas partes y meter un
//! crate de YAML por seis campos no pasaría la regla 8.

use serde::Deserialize;

/// Valla que abre y cierra la cabecera.
const FENCE: &str = "+++";

/// Cabecera declarada por un tema.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrontMatter {
    /// Id único del tema.
    pub id: String,
    /// Título mostrado.
    pub title: String,
    /// Etiquetas de agrupación.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Temas relacionados.
    #[serde(default)]
    pub see_also: Vec<String>,
    /// Comandos documentados por el tema.
    #[serde(default)]
    pub commands: Vec<String>,
    /// Contextos de UI que abren este tema con F1.
    #[serde(default)]
    pub context: Vec<String>,
}

/// Fallos al leer la cabecera.
#[derive(Debug, thiserror::Error)]
pub enum FrontMatterError {
    /// El fichero no empieza por `+++`.
    #[error("el tema no empieza con la valla `+++`")]
    Missing,
    /// Falta la valla de cierre.
    #[error("la cabecera `+++` no se cierra")]
    Unterminated,
    /// TOML inválido o campo desconocido.
    #[error("cabecera TOML inválida: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Separa `(cabecera, cuerpo)`. El cuerpo se devuelve tal cual, sin
/// interpretar: quien lo parsea es [`crate::parse`].
///
/// # Errors
/// [`FrontMatterError`] si faltan vallas o el TOML no valida.
pub fn split(source: &str) -> Result<(FrontMatter, &str), FrontMatterError> {
    let rest = source
        .strip_prefix(FENCE)
        .and_then(|r| r.strip_prefix('\n'))
        .ok_or(FrontMatterError::Missing)?;
    let end = rest
        .find("\n+++")
        .ok_or(FrontMatterError::Unterminated)?;
    let header = &rest[..end];
    let body = rest[end + 1 + FENCE.len()..]
        .strip_prefix('\n')
        .unwrap_or("");
    let fm: FrontMatter = toml::from_str(header)?;
    Ok((fm, body))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p norte-help front_matter`
Expected: PASS, 4 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-help/src/front_matter.rs crates/norte-help/src/lib.rs
git commit -m "feat(help): TOML front matter between +++ fences"
```

---

### Task 4: Inline span parser

**Files:**
- Create: `crates/norte-help/src/parse.rs` (spans only in this task)
- Modify: `crates/norte-help/src/lib.rs` (uncomment `mod parse;` and its re-export, minus the names not defined yet)

- [ ] **Step 1: Write the failing test**

Create `crates/norte-help/src/parse.rs` with this test module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Span, TopicId};

    #[test]
    fn texto_llano_es_un_solo_span() {
        assert_eq!(
            spans("plain text"),
            vec![Span::Text("plain text".to_owned())]
        );
    }

    #[test]
    fn reconoce_las_cuatro_marcas_en_linea() {
        assert_eq!(
            spans("a **b** c *d* e `f` g"),
            vec![
                Span::Text("a ".to_owned()),
                Span::Strong("b".to_owned()),
                Span::Text(" c ".to_owned()),
                Span::Emph("d".to_owned()),
                Span::Text(" e ".to_owned()),
                Span::Code("f".to_owned()),
                Span::Text(" g".to_owned()),
            ]
        );
    }

    #[test]
    fn reconoce_las_marcas_vivas_sin_resolverlas() {
        assert_eq!(
            spans("press {{cmd:fs.copy}} then see [[selection]]"),
            vec![
                Span::Text("press ".to_owned()),
                Span::CommandRef("fs.copy".to_owned()),
                Span::Text(" then see ".to_owned()),
                Span::TopicLink(TopicId::new("selection")),
            ]
        );
    }

    #[test]
    fn marca_sin_cerrar_queda_como_texto_literal() {
        assert_eq!(
            spans("{{cmd:fs.copy oops"),
            vec![Span::Text("{{cmd:fs.copy oops".to_owned())],
            "una marca rota jamás inventa un comando fantasma"
        );
    }

    #[test]
    fn el_codigo_en_linea_no_interpreta_marcas() {
        assert_eq!(
            spans("`{{cmd:x}}`"),
            vec![Span::Code("{{cmd:x}}".to_owned())]
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Uncomment `mod parse;` in `lib.rs` (leave the `pub use parse::…` line
commented for now), then run: `cargo nextest run -p norte-help parse`
Expected: FAIL — `spans` not found.

- [ ] **Step 3: Write the implementation**

Above the test module in `crates/norte-help/src/parse.rs`:

```rust
//! Parser markdown-lite (ADR 0040 decisión 3): vocabulario CERRADO. Lo que
//! este parser no sabe expresar, un `help.md` de terceros no puede pintarlo
//! en el terminal — esa es la barrera de seguridad, no una allowlist a
//! posteriori.

use crate::model::{Block, Callout, Span, TopicId};

/// Corta una línea en [`Span`]s. Una marca mal cerrada se queda en texto
/// literal: nunca produce una referencia a un comando que no se escribió.
fn spans(line: &str) -> Vec<Span> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut rest = line;
    while !rest.is_empty() {
        let taken = take_mark(rest).or_else(|| take_delim(rest));
        if let Some((span, len)) = taken {
            if !text.is_empty() {
                out.push(Span::Text(std::mem::take(&mut text)));
            }
            out.push(span);
            rest = &rest[len..];
            continue;
        }
        let ch = rest.chars().next().unwrap_or_default();
        text.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    if !text.is_empty() {
        out.push(Span::Text(text));
    }
    out
}

/// `{{cmd:…}}` y `[[…]]`, las dos marcas propias del corpus.
fn take_mark(rest: &str) -> Option<(Span, usize)> {
    if let Some(after) = rest.strip_prefix("{{cmd:") {
        let end = after.find("}}")?;
        let id = after[..end].trim();
        if id.is_empty() {
            return None;
        }
        return Some((Span::CommandRef(id.to_owned()), 6 + end + 2));
    }
    if let Some(after) = rest.strip_prefix("[[") {
        let end = after.find("]]")?;
        let id = after[..end].trim();
        if id.is_empty() {
            return None;
        }
        return Some((Span::TopicLink(TopicId::new(id)), 2 + end + 2));
    }
    None
}

/// `**fuerte**`, `*énfasis*` y `` `código` ``. El código en línea se toma
/// primero y su contenido NO se reinterpreta.
fn take_delim(rest: &str) -> Option<(Span, usize)> {
    for (open, close, build) in [
        ("`", "`", (|s: &str| Span::Code(s.to_owned())) as fn(&str) -> Span),
        ("**", "**", |s: &str| Span::Strong(s.to_owned())),
        ("*", "*", |s: &str| Span::Emph(s.to_owned())),
    ] {
        if let Some(after) = rest.strip_prefix(open) {
            if let Some(end) = after.find(close)
                && end > 0
            {
                return Some((build(&after[..end]), open.len() + end + close.len()));
            }
        }
    }
    None
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p norte-help parse`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-help/src/parse.rs crates/norte-help/src/lib.rs
git commit -m "feat(help): inline span parser with unresolved live marks"
```

---

### Task 5: Block parser and trusted mode

**Files:**
- Modify: `crates/norte-help/src/parse.rs`
- Modify: `crates/norte-help/src/lib.rs` (uncomment the `parse` re-export)

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `crates/norte-help/src/parse.rs`, and widen
its import line to `use crate::model::{Block, Callout, Span, TopicId};`:

````rust
    const DOC: &str = "+++\n\
id = \"t\"\n\
title = \"T\"\n\
+++\n\
# Heading\n\
\n\
A paragraph with {{cmd:fs.copy}}.\n\
\n\
- one\n\
- two\n\
\n\
```toml\n\
key = 1\n\
```\n\
\n\
> ⚠ careful\n\
\n\
| a | b |\n\
|---|---|\n\
| 1 | 2 |\n";

    #[test]
    fn parsea_los_seis_bloques() {
        let parsed = parse_trusted(DOC).expect("documento válido");
        let t = parsed.topic;
        assert_eq!(t.id.as_str(), "t");
        assert_eq!(t.blocks.len(), 6, "blocks: {:?}", t.blocks);
        assert_eq!(
            t.blocks[0],
            Block::Heading { level: 1, text: "Heading".to_owned() }
        );
        assert!(matches!(t.blocks[1], Block::Paragraph(_)));
        assert_eq!(
            t.blocks[2],
            Block::Bullets(vec![
                vec![Span::Text("one".to_owned())],
                vec![Span::Text("two".to_owned())],
            ])
        );
        assert_eq!(
            t.blocks[3],
            Block::Code { lang: Some("toml".to_owned()), text: "key = 1\n".to_owned() }
        );
        assert_eq!(
            t.blocks[4],
            Block::Callout { kind: Callout::Warn, spans: vec![Span::Text("careful".to_owned())] }
        );
        assert_eq!(
            t.blocks[5],
            Block::Table {
                header: vec!["a".to_owned(), "b".to_owned()],
                rows: vec![vec!["1".to_owned(), "2".to_owned()]],
            }
        );
    }

    #[test]
    fn el_modo_confiable_falla_ante_una_cabecera_rota() {
        assert!(parse_trusted("no fences here").is_err());
    }
````

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p norte-help parse`
Expected: FAIL — `parse_trusted`, `Parsed` not found.

- [ ] **Step 3: Write the implementation**

Add to `crates/norte-help/src/parse.rs`, above the tests:

````rust
use crate::front_matter::{self, FrontMatterError};
use crate::model::{Origin, Topic};

/// Topes del parser. El corpus embebido usa [`Limits::built_in`]; un
/// `help.md` de plugin usa [`Limits::untrusted`].
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Tope de bytes del fuente.
    pub max_bytes: usize,
    /// Tope de bloques producidos.
    pub max_blocks: usize,
    /// Tope de bytes por línea (una línea gigante es un ataque de render).
    pub max_line_bytes: usize,
}

impl Limits {
    /// Topes del corpus embebido: generosos, solo red de seguridad.
    #[must_use]
    pub fn built_in() -> Self {
        Self { max_bytes: 256 * 1024, max_blocks: 4096, max_line_bytes: 8 * 1024 }
    }

    /// Topes de contenido de terceros (spec: 64 KiB).
    #[must_use]
    pub fn untrusted() -> Self {
        Self { max_bytes: 64 * 1024, max_blocks: 512, max_line_bytes: 2 * 1024 }
    }
}

/// Resultado de parsear: el tema más lo que hubo que recortar.
#[derive(Clone, Debug)]
pub struct Parsed {
    /// El tema.
    pub topic: Topic,
    /// Se alcanzó algún tope y el contenido se recortó.
    pub truncated: bool,
    /// El fuente no era UTF-8 válido y se decodificó con pérdida.
    pub lossy: bool,
}

/// Fallos del modo confiable.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// La cabecera no valida.
    #[error("cabecera: {0}")]
    FrontMatter(#[from] FrontMatterError),
}

/// Parsea un tema del corpus embebido. Un fallo aquí es un fallo de build:
/// la suite parsea el corpus entero (ver `tests/corpus.rs`).
///
/// # Errors
/// [`ParseError`] si la cabecera no valida.
pub fn parse_trusted(source: &str) -> Result<Parsed, ParseError> {
    let (fm, body) = front_matter::split(source)?;
    let limits = Limits::built_in();
    let (blocks, truncated) = blocks_of(body, limits, false);
    Ok(Parsed {
        topic: Topic {
            id: TopicId::new(fm.id),
            title: fm.title,
            tags: fm.tags,
            see_also: fm.see_also.into_iter().map(TopicId::new).collect(),
            commands: fm.commands,
            context: fm.context,
            blocks,
            origin: Origin::BuiltIn,
        },
        truncated,
        lossy: false,
    })
}

/// Convierte el cuerpo en bloques. `mask` enmascara riesgos de terminal en
/// todo texto producido (modo hostil).
fn blocks_of(body: &str, limits: Limits, mask: bool) -> (Vec<Block>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    let mut lines = body.lines().peekable();
    let mut para: Vec<String> = Vec::new();

    /// Vuelca el párrafo acumulado.
    fn flush(para: &mut Vec<String>, out: &mut Vec<Block>, mask: bool) {
        if !para.is_empty() {
            let joined = para.join(" ");
            out.push(Block::Paragraph(spans_masked(&joined, mask)));
            para.clear();
        }
    }

    while let Some(raw) = lines.next() {
        if out.len() >= limits.max_blocks {
            truncated = true;
            break;
        }
        let line = if raw.len() > limits.max_line_bytes {
            truncated = true;
            let mut cut = limits.max_line_bytes;
            while !raw.is_char_boundary(cut) {
                cut -= 1;
            }
            &raw[..cut]
        } else {
            raw
        };

        if line.trim().is_empty() {
            flush(&mut para, &mut out, mask);
        } else if let Some(rest) = line.strip_prefix("```") {
            flush(&mut para, &mut out, mask);
            let lang = (!rest.trim().is_empty()).then(|| rest.trim().to_owned());
            let mut text = String::new();
            for l in lines.by_ref() {
                if l.starts_with("```") {
                    break;
                }
                text.push_str(l);
                text.push('\n');
            }
            out.push(Block::Code { lang, text: mask_if(text, mask) });
        } else if let Some(rest) = line.strip_prefix('#') {
            flush(&mut para, &mut out, mask);
            let level = 1 + u8::try_from(
                rest.chars().take_while(|c| *c == '#').count().min(2),
            )
            .unwrap_or(2);
            let text = rest.trim_start_matches('#').trim();
            out.push(Block::Heading { level, text: mask_if(text.to_owned(), mask) });
        } else if let Some(rest) = line.strip_prefix("> ") {
            flush(&mut para, &mut out, mask);
            let (kind, body) = callout_kind(rest);
            out.push(Block::Callout { kind, spans: spans_masked(body, mask) });
        } else if let Some(rest) = line.strip_prefix("- ") {
            flush(&mut para, &mut out, mask);
            let mut items = vec![spans_masked(rest, mask)];
            // `to_owned` a propósito: `peek` presta `lines` durante TODO el
            // cuerpo del `while let`, así que el `next()` de dentro no
            // compilaría con una referencia viva al buffer.
            while let Some(next) = lines
                .peek()
                .and_then(|l| l.strip_prefix("- "))
                .map(str::to_owned)
            {
                items.push(spans_masked(&next, mask));
                lines.next();
            }
            out.push(Block::Bullets(items));
        } else if line.starts_with('|') {
            flush(&mut para, &mut out, mask);
            let header = cells(line, mask);
            // Segunda línea = separador `|---|`; se descarta.
            if lines.peek().is_some_and(|l| l.starts_with('|')) {
                lines.next();
            }
            let mut rows = Vec::new();
            while let Some(l) = lines
                .peek()
                .filter(|l| l.starts_with('|'))
                .map(|l| (*l).to_owned())
            {
                rows.push(cells(&l, mask));
                lines.next();
            }
            out.push(Block::Table { header, rows });
        } else {
            para.push(line.to_owned());
        }
    }
    flush(&mut para, &mut out, mask);
    (out, truncated)
}

/// Celdas de una fila de tabla.
fn cells(line: &str, mask: bool) -> Vec<String> {
    line.trim_matches('|')
        .split('|')
        .map(|c| mask_if(c.trim().to_owned(), mask))
        .collect()
}

/// Tipo de callout por su emoji inicial; sin emoji, nota.
fn callout_kind(rest: &str) -> (Callout, &str) {
    for (marker, kind) in [("⚠", Callout::Warn), ("💡", Callout::Tip)] {
        if let Some(body) = rest.strip_prefix(marker) {
            return (kind, body.trim_start());
        }
    }
    (Callout::Note, rest)
}

/// [`spans`] con enmascarado opcional del texto producido.
fn spans_masked(line: &str, mask: bool) -> Vec<Span> {
    let owned;
    let line = if mask {
        owned = mask_if(line.to_owned(), true);
        owned.as_str()
    } else {
        line
    };
    spans(line)
}

/// Enmascara riesgos de terminal cuando el contenido es de terceros.
/// `is_terminal_hazard` es la fuente ÚNICA del set (controles, overrides
/// bidi, invisibles Cf/Zl/Zp, tag chars; ZWJ permitido a sabiendas por los
/// emoji compuestos) — la misma que usa `norte-frontend` para los nombres.
fn mask_if(s: String, mask: bool) -> String {
    if mask {
        s.chars()
            .map(|c| {
                if norte_encoding::is_terminal_hazard(c) {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    } else {
        s
    }
}
````

Uncomment in `lib.rs`:

```rust
pub use parse::{Limits, ParseError, Parsed, parse_trusted};
```

(`parse_untrusted` joins the re-export in Task 6.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p norte-help parse`
Expected: PASS, 7 tests.

- [ ] **Step 5: Lint**

Run: `cargo clippy -p norte-help --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-help/src/parse.rs crates/norte-help/src/lib.rs
git commit -m "feat(help): block parser and trusted parse mode"
```

---

### Task 6: Hostile mode

**Files:**
- Modify: `crates/norte-help/src/parse.rs`
- Create: `crates/norte-help/tests/hostile.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/norte-help/tests/hostile.rs`:

```rust
//! Modo hostil: un `help.md` de plugin es texto de TERCEROS. Nunca falla,
//! siempre acota, siempre enmascara. Las cadenas hostiles salen del corpus
//! canónico de `norte-testkit` (regla de la casa: fixtures = código).

use norte_help::{Limits, Origin, Span, parse_untrusted};

#[test]
fn sin_cabecera_no_falla_y_usa_el_id_del_plugin() {
    let parsed = parse_untrusted(b"just a body", "acme-ftp", None);
    assert_eq!(parsed.topic.id.as_str(), "acme-ftp");
    assert_eq!(parsed.topic.title, "acme-ftp");
    assert!(matches!(parsed.topic.origin, Origin::Plugin { .. }));
}

#[test]
fn utf8_invalido_decodifica_con_perdida_y_marca_la_bandera() {
    let parsed = parse_untrusted(b"+++\nid = \"x\"\ntitle = \"X\"\n+++\n\xff\xfe body", "p", None);
    assert!(parsed.lossy, "la bandera alimenta el badge de la UI");
    assert!(!parsed.topic.blocks.is_empty());
}

#[test]
fn supera_el_tope_de_bytes_y_recorta() {
    let mut src = b"+++\nid = \"x\"\ntitle = \"X\"\n+++\n".to_vec();
    src.extend(std::iter::repeat_n(b'a', Limits::untrusted().max_bytes * 2));
    let parsed = parse_untrusted(&src, "p", None);
    assert!(parsed.truncated);
}

#[test]
fn enmascara_bidi_e_invisibles_del_corpus_hostil() {
    // `\u{202E}` (RIGHT-TO-LEFT OVERRIDE) es el clásico de suplantación de
    // nombres; en prosa de plugin haría lo mismo con el resto de la pantalla.
    let src = "+++\nid = \"x\"\ntitle = \"X\"\n+++\nhello \u{202E}dlrow\n";
    let parsed = parse_untrusted(src.as_bytes(), "p", None);
    let text: String = parsed
        .topic
        .blocks
        .iter()
        .flat_map(|b| match b {
            norte_help::Block::Paragraph(spans) => spans.clone(),
            _ => Vec::new(),
        })
        .map(|s| match s {
            Span::Text(t) => t,
            _ => String::new(),
        })
        .collect();
    assert!(!text.contains('\u{202E}'), "bidi crudo jamás llega al render: {text:?}");
    assert!(text.contains('\u{FFFD}'));
}

#[test]
fn una_marca_de_comando_de_plugin_sobrevive_como_referencia() {
    let src = "+++\nid = \"x\"\ntitle = \"X\"\n+++\nrun {{cmd:acme:sync}}\n";
    let parsed = parse_untrusted(src.as_bytes(), "acme", None);
    let has_ref = parsed.topic.blocks.iter().any(|b| match b {
        norte_help::Block::Paragraph(spans) => spans
            .iter()
            .any(|s| matches!(s, Span::CommandRef(c) if c == "acme:sync")),
        _ => false,
    });
    assert!(has_ref, "el plugin documenta SUS comandos: {:?}", parsed.topic.blocks);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p norte-help --test hostile`
Expected: FAIL — `parse_untrusted` not found.

- [ ] **Step 3: Implement `parse_untrusted`**

Add to `crates/norte-help/src/parse.rs`:

```rust
/// Recorta a `max` bytes sin partir un carácter UTF-8 (si los bytes lo son).
fn cut_at_boundary(bytes: &[u8], max: usize) -> &[u8] {
    let mut cut = max.min(bytes.len());
    // Retrocede mientras el byte de corte sea una continuación `10xxxxxx`.
    while cut > 0 && cut < bytes.len() && bytes[cut] & 0b1100_0000 == 0b1000_0000 {
        cut -= 1;
    }
    &bytes[..cut]
}

/// Parsea el `help.md` de un plugin. NUNCA falla: sin cabecera usa
/// `fallback_id` como id y título, decodifica UTF-8 con pérdida, acota por
/// [`Limits::untrusted`] y enmascara riesgos de terminal EN EL PARSEO (el
/// enmascarado vive donde se construye el dato, igual que en las filas de
/// la palette, no repartido por cada frontend).
#[must_use]
pub fn parse_untrusted(bytes: &[u8], fallback_id: &str, publisher: Option<String>) -> Parsed {
    let limits = Limits::untrusted();
    let mut truncated = bytes.len() > limits.max_bytes;
    let head = if truncated {
        cut_at_boundary(bytes, limits.max_bytes)
    } else {
        bytes
    };
    let text = String::from_utf8_lossy(head);
    let lossy = matches!(text, std::borrow::Cow::Owned(_));

    let (fm, body) = match crate::front_matter::split(&text) {
        Ok((fm, body)) => (Some(fm), body),
        Err(_) => (None, text.as_ref()),
    };
    let (blocks, cut) = blocks_of(body, limits, true);
    truncated |= cut;

    let masked = |s: String| mask_if(s, true);
    let id = fm.as_ref().map_or_else(|| fallback_id.to_owned(), |f| f.id.clone());
    let title = fm
        .as_ref()
        .map_or_else(|| fallback_id.to_owned(), |f| f.title.clone());
    Parsed {
        topic: Topic {
            id: TopicId::new(masked(id)),
            title: masked(title),
            tags: Vec::new(),
            see_also: Vec::new(),
            commands: fm.as_ref().map(|f| f.commands.clone()).unwrap_or_default(),
            context: Vec::new(),
            blocks,
            origin: Origin::Plugin {
                id: fallback_id.to_owned(),
                publisher: publisher.map(masked),
                truncated,
                lossy,
            },
        },
        truncated,
        lossy,
    }
}
```

Note the deliberate asymmetry: `tags`, `see_also` and `context` from a plugin
header are **dropped**. A plugin does not get to place itself in the host's
index groups, link into host topics, or claim a UI context. Only its own
`commands` survive; enforcing that those ids are namespaced to the plugin is
host-side work and lands with the registry in phase H3e — this crate only
guarantees the fields it drops.

Uncomment the full parse re-export in `lib.rs`:

```rust
pub use parse::{Limits, ParseError, Parsed, parse_trusted, parse_untrusted};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p norte-help`
Expected: PASS, 12 tests (7 parse + 5 hostile).

- [ ] **Step 5: Commit**

```bash
git add crates/norte-help/src/parse.rs crates/norte-help/src/lib.rs crates/norte-help/tests/hostile.rs
git commit -m "feat(help): hostile parse mode for plugin-supplied help"
```

---

### Task 7: Corpus — embedding, lookup and six seed topics

**Files:**
- Create: `crates/norte-help/src/corpus.rs`
- Create: `crates/norte-help/topics/en/{index,panes,selection,copying,remote,archives}.md`
- Create: `crates/norte-help/topics/es/{index,panes,selection,copying,remote,archives}.md`
- Create: `crates/norte-help/tests/corpus.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/norte-help/tests/corpus.rs`:

```rust
//! Integridad del corpus embebido. Cada aserción aquí es una regla que un
//! tema nuevo no puede saltarse sin poner la build en rojo.

use norte_help::{Lang, topic, topic_ids, topics};

#[test]
fn todos_los_temas_parsean_en_ambos_idiomas() {
    for lang in [Lang::En, Lang::Es] {
        let all = topics(lang);
        assert!(!all.is_empty(), "corpus vacío en {lang:?}");
    }
}

#[test]
fn paridad_estructural_entre_idiomas() {
    let en: Vec<_> = topic_ids(Lang::En);
    let es: Vec<_> = topic_ids(Lang::Es);
    assert_eq!(en, es, "cada tema existe en ambos idiomas, con el mismo id");
    for id in en {
        let a = topic(Lang::En, id.as_str()).unwrap();
        let b = topic(Lang::Es, id.as_str()).unwrap();
        assert_eq!(a.commands, b.commands, "{id}: los comandos no divergen");
        assert_eq!(a.see_also, b.see_also, "{id}: los enlaces no divergen");
        assert_eq!(a.context, b.context, "{id}: los contextos no divergen");
        assert_eq!(a.tags, b.tags, "{id}: las etiquetas no divergen");
        assert_ne!(a.title, "", "{id}: título vacío");
    }
}

#[test]
fn la_tabla_embebida_cubre_el_directorio() {
    // El corpus se embebe con una tabla `include_str!` explícita (ADR 0040);
    // este test es lo que impide que un fichero nuevo quede fuera en silencio.
    for lang in ["en", "es"] {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("topics")
            .join(lang);
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("directorio de temas")
            .map(|e| {
                e.expect("entrada")
                    .path()
                    .file_stem()
                    .expect("stem")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        on_disk.sort();
        let embedded_lang = if lang == "en" { Lang::En } else { Lang::Es };
        let mut embedded: Vec<String> = topic_ids(embedded_lang)
            .iter()
            .map(|i| i.as_str().to_owned())
            .collect();
        embedded.sort();
        assert_eq!(
            on_disk, embedded,
            "{lang}: fichero en topics/ que la tabla include_str! no lista (o al revés)"
        );
    }
}

#[test]
fn el_indice_existe_y_es_el_tema_raiz() {
    let idx = topic(Lang::En, "index").expect("index");
    assert!(!idx.see_also.is_empty(), "el índice enlaza a los demás temas");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p norte-help --test corpus`
Expected: FAIL — `topics`, `topic`, `topic_ids` not found.

- [ ] **Step 3: Write the corpus module**

Create `crates/norte-help/src/corpus.rs`:

```rust
//! Corpus embebido: tabla `include_str!` explícita (ADR 0040 decisión 2),
//! igual que los presets de `norte-theme` y los catálogos de `norte-i18n`.
//! `tests/corpus.rs` cruza esta tabla contra el directorio, así que un
//! fichero nuevo sin entrada aquí rompe la build.

use std::sync::OnceLock;

use norte_i18n::Lang;

use crate::model::{Topic, TopicId};
use crate::parse::parse_trusted;

/// Fuentes de los temas en inglés.
const EN: &[&str] = &[
    include_str!("../topics/en/index.md"),
    include_str!("../topics/en/panes.md"),
    include_str!("../topics/en/selection.md"),
    include_str!("../topics/en/copying.md"),
    include_str!("../topics/en/remote.md"),
    include_str!("../topics/en/archives.md"),
];

/// Fuentes de los temas en castellano.
const ES: &[&str] = &[
    include_str!("../topics/es/index.md"),
    include_str!("../topics/es/panes.md"),
    include_str!("../topics/es/selection.md"),
    include_str!("../topics/es/copying.md"),
    include_str!("../topics/es/remote.md"),
    include_str!("../topics/es/archives.md"),
];

/// Cachés perezosas: el parseo ocurre una vez por idioma y proceso.
static EN_PARSED: OnceLock<Vec<Topic>> = OnceLock::new();
static ES_PARSED: OnceLock<Vec<Topic>> = OnceLock::new();

/// Todos los temas de un idioma, en el orden de la tabla.
///
/// # Panics
/// Si un tema embebido no parsea. Es intencional: el corpus viaja en el
/// binario y `tests/corpus.rs` lo parsea entero, así que un tema roto no
/// llega a publicarse.
#[must_use]
pub fn topics(lang: Lang) -> &'static [Topic] {
    let (cell, src) = match lang {
        Lang::En => (&EN_PARSED, EN),
        Lang::Es => (&ES_PARSED, ES),
    };
    cell.get_or_init(|| {
        src.iter()
            .map(|s| {
                parse_trusted(s)
                    .unwrap_or_else(|e| panic!("tema embebido inválido: {e}"))
                    .topic
            })
            .collect()
    })
}

/// Ids de todos los temas de un idioma, en orden de tabla.
#[must_use]
pub fn topic_ids(lang: Lang) -> Vec<TopicId> {
    topics(lang).iter().map(|t| t.id.clone()).collect()
}

/// Un tema por id.
#[must_use]
pub fn topic(lang: Lang, id: &str) -> Option<&'static Topic> {
    topics(lang).iter().find(|t| t.id.as_str() == id)
}
```

Uncomment in `lib.rs`: `mod corpus;` and
`pub use corpus::{topic, topic_ids, topics};`.

- [ ] **Step 4: Write the six English topics**

Create `crates/norte-help/topics/en/index.md`:

```markdown
+++
id = "index"
title = "Welcome to norte"
tags = ["basics"]
see_also = ["panes", "selection", "copying", "remote", "archives"]
+++
norte is an orthodox file manager: two panes, keyboard first, and the same
commands whether the files are on this machine, on an SSH host, in an S3
bucket, or inside an archive.

Start here:

- [[panes]] — the two-pane model and how focus works
- [[selection]] — marking the files a command will act on
- [[copying]] — moving data between any two backends
- [[remote]] — SSH, FTP and S3 connections
- [[archives]] — reading inside `.zip` and `.tar` files

> 💡 Press the filter key in this screen to search every topic and command at
> once.
```

Create `crates/norte-help/topics/en/panes.md`:

```markdown
+++
id = "panes"
title = "Two panes, one job"
tags = ["basics"]
see_also = ["selection", "copying"]
commands = ["pane.focus-other", "pane.sync"]
context = ["browse"]
+++
Two panes are always on screen. One has focus; the other is the destination
for anything you do. That is the whole model, and it is why an orthodox
manager needs so few keys: the command never has to ask *where to*.

{{cmd:pane.focus-other}} swaps which one is active.
{{cmd:pane.sync}} points the inactive pane at the same directory as the
active one.

> 💡 The destination is a *pane*, not a *disk*. The other pane can be an SSH
> host or an S3 bucket, and every command still works the same way.
```

Create `crates/norte-help/topics/en/selection.md`:

```markdown
+++
id = "selection"
title = "Marking what to act on"
tags = ["basics"]
see_also = ["copying"]
commands = ["pane.select", "pane.select-all", "pane.invert-selection"]
+++
Commands act on the marked files, or on the file under the cursor when
nothing is marked.

{{cmd:pane.select}} marks the file under the cursor and moves on.
{{cmd:pane.select-all}} marks everything in the listing.
{{cmd:pane.invert-selection}} flips the marks.

Marks survive navigation, so you can mark, walk into a subdirectory, mark
more, and copy once.
```

Create `crates/norte-help/topics/en/copying.md`:

```markdown
+++
id = "copying"
title = "Copying across backends"
tags = ["doing"]
see_also = ["selection", "remote", "archives"]
commands = ["fs.copy", "fs.move"]
context = ["dialog.collision"]
+++
Mark files in the active pane, then {{cmd:fs.copy}}. The other pane is the
destination — always, whatever it holds.

{{cmd:fs.move}} does the same and removes the source once the copy is
verified.

Every copy is a task: it reports progress, it can be cancelled, and
cancelling leaves either a clean destination or a clearly marked
`.norte-partial` file. Never an unmarked half-file.

> ⚠ When names collide, norte asks. The answer applies to that file; hold the
> choice for the rest of the batch from the same dialog.
```

Create `crates/norte-help/topics/en/remote.md`:

```markdown
+++
id = "remote"
title = "Remote connections"
tags = ["remote"]
see_also = ["copying", "archives"]
commands = ["app.connect"]
+++
A pane can hold a remote location as easily as a local directory. norte
speaks SFTP, FTP and S3-compatible object storage.

{{cmd:app.connect}} opens the connection manager.

Credentials live in the system keyring, never in the configuration file —
`connections.toml` holds a *reference* to a secret, not the secret.

> ⚠ Object storage has no directories. norte shows prefixes as folders, which
> means an empty folder only exists if something created a marker object for
> it.
```

Create `crates/norte-help/topics/en/archives.md`:

```markdown
+++
id = "archives"
title = "Looking inside archives"
tags = ["remote"]
see_also = ["copying"]
commands = ["pane.enter"]
+++
Press {{cmd:pane.enter}} on a `.zip`, `.tar` or `.tar.gz` and the pane walks
into it as if it were a directory. The path grows a `!` marker at the archive
boundary.

Archives are **read-only**. Copying files *out* works exactly like any other
copy; copying *in* does not, and the help dims those commands with the reason
rather than letting you find out on Enter.

> ⚠ Filenames inside a zip are not always UTF-8. norte keeps the raw bytes and
> flags the ones it had to guess at, instead of silently mangling them.
```

- [ ] **Step 5: Write the six Spanish topics**

Create the same six files under `crates/norte-help/topics/es/` with identical
front matter values for `id`, `tags`, `see_also`, `commands` and `context`
(the parity test compares those exactly) and a translated `title` plus
translated body. For example, `crates/norte-help/topics/es/panes.md`:

```markdown
+++
id = "panes"
title = "Dos paneles, un trabajo"
tags = ["basics"]
see_also = ["selection", "copying"]
commands = ["pane.focus-other", "pane.sync"]
context = ["browse"]
+++
Siempre hay dos paneles en pantalla. Uno tiene el foco; el otro es el destino
de lo que hagas. Ese es el modelo entero, y por eso a un gestor ortodoxo le
bastan tan pocas teclas: el comando nunca tiene que preguntar *hacia dónde*.

{{cmd:pane.focus-other}} cambia cuál está activo.
{{cmd:pane.sync}} apunta el panel inactivo al mismo directorio que el activo.

> 💡 El destino es un *panel*, no un *disco*. El otro panel puede ser un host
> SSH o un bucket de S3, y cada comando sigue funcionando igual.
```

Translate the remaining five the same way: `index`, `selection`, `copying`,
`remote`, `archives`.

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo nextest run -p norte-help --test corpus`
Expected: PASS, 4 tests.

Run: `cargo nextest run -p norte-help`
Expected: PASS, 16 tests.

- [ ] **Step 7: Commit**

```bash
git add crates/norte-help/src/corpus.rs crates/norte-help/topics crates/norte-help/tests/corpus.rs crates/norte-help/src/lib.rs
git commit -m "feat(help): embedded corpus with six seed topics in EN and ES"
```

---

### Task 8: Integrity checks

**Files:**
- Create: `crates/norte-help/src/check.rs`
- Modify: `crates/norte-help/tests/corpus.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/norte-help/tests/corpus.rs`:

```rust
use norte_help::{Issue, check_commands, check_contexts, check_corpus};

#[test]
fn el_corpus_no_tiene_enlaces_colgantes_ni_temas_duplicados() {
    let issues = check_corpus();
    assert!(issues.is_empty(), "integridad del corpus: {issues:?}");
}

#[test]
fn un_comando_desconocido_se_reporta() {
    // El corpus documenta `fs.copy`; un vocabulario que no lo contenga debe
    // producir un `UnknownCommand`, no pasar en silencio.
    let issues = check_commands(&["pane.enter"], &[]);
    assert!(
        issues.iter().any(|i| matches!(i, Issue::UnknownCommand { command, .. } if command == "fs.copy")),
        "issues: {issues:?}"
    );
}

#[test]
fn un_comando_sin_documentar_se_reporta_salvo_allowlist() {
    let known = ["fs.copy", "app.quit"];
    let issues = check_commands(&known, &[]);
    assert!(
        issues.iter().any(|i| matches!(i, Issue::UndocumentedCommand { command } if command == "app.quit")),
        "issues: {issues:?}"
    );
    let with_allow = check_commands(&known, &["app.quit"]);
    assert!(
        !with_allow.iter().any(|i| matches!(i, Issue::UndocumentedCommand { .. })),
        "la allowlist silencia SOLO lo que enumera: {with_allow:?}"
    );
}

#[test]
fn un_contexto_desconocido_se_reporta() {
    let issues = check_contexts(&["browse"]);
    assert!(
        issues.iter().any(|i| matches!(i, Issue::UnknownContext { context, .. } if context == "dialog.collision")),
        "issues: {issues:?}"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p norte-help --test corpus`
Expected: FAIL — `check_corpus`, `check_commands`, `check_contexts`, `Issue`
not found.

- [ ] **Step 3: Write the implementation**

Create `crates/norte-help/src/check.rs`:

```rust
//! Comprobaciones de integridad del corpus. Devuelven DATOS, no `panic!`:
//! los consume la suite de tests (donde una lista no vacía rompe la build) y
//! más adelante `norte doctor`, que las enseña al usuario.

use std::collections::BTreeSet;

use norte_i18n::Lang;

use crate::corpus::{topic_ids, topics};
use crate::model::Span;

/// Un problema de integridad encontrado en el corpus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Issue {
    /// Un tema existe en un idioma y no en otro.
    MissingLocale {
        /// Id del tema.
        id: String,
        /// Idioma donde falta.
        lang: Lang,
    },
    /// Dos temas comparten id dentro de un idioma.
    DuplicateTopic {
        /// Id repetido.
        id: String,
    },
    /// Un `[[enlace]]` o `see_also` apunta a un tema inexistente.
    DanglingLink {
        /// Tema de origen.
        from: String,
        /// Id apuntado.
        to: String,
    },
    /// El corpus menciona un comando que no existe en el vocabulario.
    UnknownCommand {
        /// Tema que lo menciona.
        topic: String,
        /// Comando mencionado.
        command: String,
    },
    /// Un comando del vocabulario no aparece en ningún tema.
    UndocumentedCommand {
        /// Comando sin documentar.
        command: String,
    },
    /// Un tema declara un contexto que la UI no conoce.
    UnknownContext {
        /// Tema que lo declara.
        topic: String,
        /// Contexto declarado.
        context: String,
    },
    /// Dos temas se disputan el mismo contexto.
    DuplicateContext {
        /// Contexto disputado.
        context: String,
    },
}

/// Comandos mencionados por un tema: los de `commands` más los de las
/// marcas `{{cmd:…}}` del cuerpo.
fn mentioned(t: &crate::model::Topic) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = t.commands.iter().cloned().collect();
    for block in &t.blocks {
        let spans: &[Span] = match block {
            crate::model::Block::Paragraph(s) | crate::model::Block::Callout { spans: s, .. } => s,
            crate::model::Block::Bullets(items) => {
                for item in items {
                    out.extend(item.iter().filter_map(|s| match s {
                        Span::CommandRef(c) => Some(c.clone()),
                        _ => None,
                    }));
                }
                continue;
            }
            _ => continue,
        };
        out.extend(spans.iter().filter_map(|s| match s {
            Span::CommandRef(c) => Some(c.clone()),
            _ => None,
        }));
    }
    out
}

/// Paridad entre idiomas, ids únicos y enlaces que resuelven.
#[must_use]
pub fn check_corpus() -> Vec<Issue> {
    let mut issues = Vec::new();
    let en: BTreeSet<String> = topic_ids(Lang::En).iter().map(|i| i.as_str().to_owned()).collect();
    let es: BTreeSet<String> = topic_ids(Lang::Es).iter().map(|i| i.as_str().to_owned()).collect();
    for id in en.difference(&es) {
        issues.push(Issue::MissingLocale { id: id.clone(), lang: Lang::Es });
    }
    for id in es.difference(&en) {
        issues.push(Issue::MissingLocale { id: id.clone(), lang: Lang::En });
    }

    for lang in [Lang::En, Lang::Es] {
        let all = topics(lang);
        let ids: BTreeSet<&str> = all.iter().map(|t| t.id.as_str()).collect();
        if ids.len() != all.len() {
            let mut seen = BTreeSet::new();
            for t in all {
                if !seen.insert(t.id.as_str()) {
                    issues.push(Issue::DuplicateTopic { id: t.id.to_string() });
                }
            }
        }
        for t in all {
            let mut links: Vec<String> =
                t.see_also.iter().map(std::string::ToString::to_string).collect();
            for block in &t.blocks {
                if let crate::model::Block::Paragraph(spans)
                | crate::model::Block::Callout { spans, .. } = block
                {
                    links.extend(spans.iter().filter_map(|s| match s {
                        Span::TopicLink(id) => Some(id.to_string()),
                        _ => None,
                    }));
                }
                if let crate::model::Block::Bullets(items) = block {
                    for item in items {
                        links.extend(item.iter().filter_map(|s| match s {
                            Span::TopicLink(id) => Some(id.to_string()),
                            _ => None,
                        }));
                    }
                }
            }
            for to in links {
                if !ids.contains(to.as_str()) {
                    issues.push(Issue::DanglingLink { from: t.id.to_string(), to });
                }
            }
        }
    }
    issues
}

/// Cruza el corpus con el vocabulario de comandos del frontend: nada
/// mencionado que no exista, nada existente sin documentar (salvo lo que
/// `allow` enumera explícitamente).
#[must_use]
pub fn check_commands(known: &[&str], allow: &[&str]) -> Vec<Issue> {
    let known: BTreeSet<&str> = known.iter().copied().collect();
    let allow: BTreeSet<&str> = allow.iter().copied().collect();
    let mut issues = Vec::new();
    let mut documented: BTreeSet<String> = BTreeSet::new();

    for t in topics(Lang::En) {
        for cmd in mentioned(t) {
            if known.contains(cmd.as_str()) {
                documented.insert(cmd);
            } else {
                issues.push(Issue::UnknownCommand { topic: t.id.to_string(), command: cmd });
            }
        }
    }
    for cmd in known {
        if !documented.contains(cmd) && !allow.contains(cmd) {
            issues.push(Issue::UndocumentedCommand { command: (*cmd).to_owned() });
        }
    }
    issues
}

/// Cruza los `context` declarados con los contextos que la UI conoce, y
/// exige que ninguno esté disputado por dos temas.
#[must_use]
pub fn check_contexts(known: &[&str]) -> Vec<Issue> {
    let known: BTreeSet<&str> = known.iter().copied().collect();
    let mut issues = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for t in topics(Lang::En) {
        for ctx in &t.context {
            if !known.contains(ctx.as_str()) {
                issues.push(Issue::UnknownContext {
                    topic: t.id.to_string(),
                    context: ctx.clone(),
                });
            }
            if !seen.insert(ctx.clone()) {
                issues.push(Issue::DuplicateContext { context: ctx.clone() });
            }
        }
    }
    issues
}
```

Uncomment in `lib.rs`: `mod check;` and
`pub use check::{Issue, check_commands, check_contexts, check_corpus};`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p norte-help`
Expected: PASS, 20 tests.

- [ ] **Step 5: Lint**

Run: `cargo clippy -p norte-help --all-targets -- -D warnings`
Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-help/src/check.rs crates/norte-help/tests/corpus.rs crates/norte-help/src/lib.rs
git commit -m "feat(help): corpus integrity checks as data, not panics"
```

---

### Task 9: `ChordResolver` and the documentation gate

**Files:**
- Create: `crates/norte-help/src/resolve.rs`
- Create: `crates/norte-tui/tests/help_gate.rs`
- Modify: `crates/norte-tui/Cargo.toml` (dev-dependency on `norte-help`)

- [ ] **Step 1: Write the failing test for the resolver**

Create `crates/norte-help/src/resolve.rs` with this test module only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Availability, Span};

    struct Fake;

    impl ChordResolver for Fake {
        fn chord(&self, command: &str) -> Option<String> {
            (command == "fs.copy").then(|| "F5".to_owned())
        }

        fn label(&self, command: &str) -> String {
            format!("label of {command}")
        }

        fn availability(&self, _command: &str) -> Availability {
            Availability::Available
        }
    }

    #[test]
    fn una_referencia_se_resuelve_al_chord_del_usuario() {
        assert_eq!(
            render_span(&Span::CommandRef("fs.copy".to_owned()), &Fake),
            "F5"
        );
    }

    #[test]
    fn un_comando_sin_tecla_no_finge_tener_una() {
        assert_eq!(
            render_span(&Span::CommandRef("fs.move".to_owned()), &Fake),
            "label of fs.move",
            "sin binding, la prosa nombra el comando en vez de inventar una tecla"
        );
    }

    #[test]
    fn las_filas_llevan_etiqueta_chord_y_disponibilidad() {
        let topic = crate::corpus::topic(crate::Lang::En, "copying").expect("copying");
        let rows = rows_of(topic, &Fake);
        assert!(!rows.is_empty());
        let copy = rows.iter().find(|r| r.row.command == "fs.copy").expect("fs.copy");
        assert_eq!(copy.chord.as_deref(), Some("F5"));
        assert_eq!(copy.label, "label of fs.copy");
        assert!(copy.row.avail.is_available());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p norte-help resolve`
Expected: FAIL — `ChordResolver`, `render_span`, `rows_of` not found.

- [ ] **Step 3: Write the implementation**

Above the tests in `crates/norte-help/src/resolve.rs`:

```rust
//! Resolución de las marcas vivas. El corpus llega SIN teclas; quien las
//! sabe es el frontend, que posee el keymap efectivo del usuario. Por eso
//! esto es un trait y no una tabla: la prosa no puede quedar desfasada
//! respecto a un rebind porque nunca guarda la tecla.

use crate::model::{Availability, CommandRow, Span, Topic};

/// Lo que un frontend debe saber contestar para pintar la ayuda.
pub trait ChordResolver {
    /// Tecla efectiva del comando, o `None` si el usuario no lo tiene atado.
    fn chord(&self, command: &str) -> Option<String>;

    /// Descripción corta del comando (en el TUI, el catálogo Fluent
    /// `help-cmd-*`).
    fn label(&self, command: &str) -> String;

    /// Disponibilidad del comando en el contexto ACTUAL.
    fn availability(&self, command: &str) -> Availability;
}

/// Una fila lista para pintar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRow {
    /// Comando y disponibilidad.
    pub row: CommandRow,
    /// Descripción corta.
    pub label: String,
    /// Tecla efectiva, si la hay.
    pub chord: Option<String>,
}

/// Texto con el que se pinta un span en línea. Una referencia sin tecla cae
/// a la descripción del comando: mejor nombrar el comando que mentir con
/// una tecla que el usuario desató.
#[must_use]
pub fn render_span(span: &Span, r: &impl ChordResolver) -> String {
    match span {
        Span::Text(t) | Span::Strong(t) | Span::Emph(t) | Span::Code(t) => t.clone(),
        Span::CommandRef(c) => r.chord(c).unwrap_or_else(|| r.label(c)),
        Span::TopicLink(id) => id.to_string(),
    }
}

/// Filas ejecutables de un tema, en el orden declarado por `commands`.
#[must_use]
pub fn rows_of(topic: &Topic, r: &impl ChordResolver) -> Vec<ResolvedRow> {
    topic
        .commands
        .iter()
        .map(|c| ResolvedRow {
            row: CommandRow { command: c.clone(), avail: r.availability(c) },
            label: r.label(c),
            chord: r.chord(c),
        })
        .collect()
}
```

Uncomment in `lib.rs` and extend the re-export:

```rust
mod resolve;
pub use resolve::{ChordResolver, ResolvedRow, render_span, rows_of};
```

- [ ] **Step 4: Run the resolver tests**

Run: `cargo nextest run -p norte-help`
Expected: PASS, 23 tests.

- [ ] **Step 5: Write the documentation gate**

Add to `crates/norte-tui/Cargo.toml` under `[dev-dependencies]`:

```toml
norte-help.workspace = true
```

Create `crates/norte-tui/tests/help_gate.rs`:

```rust
//! Puerta de documentación (ADR 0040): todo comando de `COMMANDS` vive en
//! algún tema del corpus. Un comando nuevo cuesta un párrafo — es fricción
//! DELIBERADA, la misma idea que la suite de i18n que obliga a EN+ES.
//!
//! `PENDIENTES` es la deuda visible mientras H3h redacta el corpus completo.
//! Solo puede MENGUAR: la fase H3h la deja vacía y este fichero pasa a ser
//! el gate definitivo.

use norte_help::{Issue, check_commands, check_contexts, check_corpus};
use norte_tui::keymap::COMMANDS;

/// Comandos aún sin tema. Se borran conforme H3h redacta.
const PENDIENTES: &[&str] = &[
    // NOTA para quien ejecuta el plan: rellenar con la salida del primer
    // fallo de `gate_de_documentacion` (Step 6), un comando por línea y en
    // el mismo orden en que los imprime el test. No añadir comodines: la
    // lista es explícita a propósito.
];

/// Contextos que la TUI conoce hoy. Los `context` del corpus deben caer
/// dentro de este vocabulario.
const CONTEXTS: &[&str] = &["browse", "viewer", "dialog.collision"];

#[test]
fn el_corpus_esta_integro() {
    let issues = check_corpus();
    assert!(issues.is_empty(), "{issues:?}");
}

#[test]
fn gate_de_documentacion() {
    let issues = check_commands(COMMANDS, PENDIENTES);
    let unknown: Vec<&Issue> = issues
        .iter()
        .filter(|i| matches!(i, Issue::UnknownCommand { .. }))
        .collect();
    assert!(
        unknown.is_empty(),
        "el corpus menciona comandos que no existen: {unknown:?}"
    );
    let undocumented: Vec<String> = issues
        .iter()
        .filter_map(|i| match i {
            Issue::UndocumentedCommand { command } => Some(command.clone()),
            _ => None,
        })
        .collect();
    assert!(
        undocumented.is_empty(),
        "comandos sin documentar fuera de PENDIENTES:\n{}",
        undocumented.join("\n")
    );
}

#[test]
fn los_contextos_del_corpus_existen_en_la_tui() {
    let issues = check_contexts(CONTEXTS);
    assert!(issues.is_empty(), "{issues:?}");
}
```

- [ ] **Step 6: Run the gate and fill the allowlist**

Run: `cargo nextest run -p norte-tui --test help_gate`
Expected: FAIL on `gate_de_documentacion`, printing every command not yet
covered by the six seed topics. Copy that list verbatim into `PENDIENTES`,
one `"command",` per line.

Re-run: `cargo nextest run -p norte-tui --test help_gate`
Expected: PASS, 3 tests. If `los_contextos_del_corpus_existen_en_la_tui`
fails, the corpus declares a context string the TUI does not use — fix the
topic front matter, not the `CONTEXTS` constant (the constant is the TUI's
truth; H3c replaces it with the real context list from the keymap).

- [ ] **Step 7: Commit**

```bash
git add crates/norte-help/src/resolve.rs crates/norte-help/src/lib.rs crates/norte-tui/Cargo.toml crates/norte-tui/tests/help_gate.rs
git commit -m "feat(help): ChordResolver and the documentation gate with a shrinking allowlist"
```

---

### Task 10: Phase close — lints, docs, full CI

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `crates/norte-help/src/lib.rs` (final doctest)

- [ ] **Step 1: Verify the crate doctest**

The `lib.rs` doctest from Task 1 asserts
`topic(Lang::En, "index").title == "Welcome to norte"`. Confirm the title in
`topics/en/index.md` matches exactly.

Run: `cargo test -p norte-help --doc`
Expected: PASS, 1 doctest (the crate-level example in `lib.rs`).

- [ ] **Step 2: Add the changelog entry**

In `CHANGELOG.md`, under the unreleased section, add:

```markdown
- `norte-help`: embedded, localized help corpus (markdown-lite with TOML front
  matter, ADR 0040), with a hostile parse mode for plugin-supplied help and
  integrity checks that gate undocumented commands.
```

- [ ] **Step 3: Lint the whole workspace**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: no warnings. Workspace-wide and not `-p norte-help`: the new
dev-dependency in `norte-tui` and the new workspace member change the graph.

Run: `cargo fmt --all`

- [ ] **Step 4: Run the full gate**

Run: `just ci`
Expected: EXIT=0. This phase touches `norte-encoding` (a crate outside the
85% coverage gate) and adds a crate; the gate covers proto/vfs/core, so
coverage should be unaffected — confirm the reported number did not drop.

Note the recorded pitfall: run `cargo llvm-cov clean` first if the coverage
number looks stale.

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md crates/norte-help/src/lib.rs
git commit -m "docs(help): changelog entry for the norte-help crate"
```

- [ ] **Step 6: Request review**

Dispatch the house reviewers on the diff of this phase:

- `rust-reviewer` — hard rules, typed errors, no `unwrap` outside tests, the
  `OnceLock` panic-on-invalid-corpus decision.
- `encoding-auditor` — hostile-mode masking at parse time (does anything
  reach a block unmasked?), the UTF-8 boundary cut in `cut_at_boundary`, the
  lossy flag, and whether the hostile test set should grow a fixture in the
  canonical `norte-testkit` corpus.

Apply their findings before starting H3b.

---

## Notes for the executor

- **Do not** add a dependency in this phase. If something seems to need one,
  it is a design error — raise it instead of adding the crate (rule 8).
- **Language (decided 2026-08-04, mid-execution):** `norte-help` follows
  `norte-config`, the most recent crate, and is **English throughout** —
  rustdoc, implementation comments and test names. The code blocks in Tasks 2
  to 9 below were written in Spanish before that decision; translate them as
  you go, keeping every "why" note intact. The rest of the workspace keeps its
  existing Spanish comments; this is not a repo-wide migration.
- **Obligation carried into Task 5 (from the Task 2 review):** `Block::Table`
  now documents that every row arrives normalised to `header.len()` — the
  parser pads missing cells and drops the excess — so a renderer may index by
  column without a length check. Task 5's `cells`/table branch must enforce
  that, with a test for a ragged row, because those cells come from a plain
  split over hostile plugin `help.md`.
- **Correction to Task 3 (found in review):** the plan's `split` ended with
  `.strip_prefix('\n').unwrap_or("")`, which silently returns an EMPTY body
  when the closing-fence line carries trailing content — a stray space, a
  `++++` line, or a CRLF checkout produce a topic that parses and renders
  blank. The shipped implementation rejects trailing content with a dedicated
  `FrontMatterError` variant and accepts `\r\n` on both fences.
- The `lib.rs` module list is written in full in Task 1 and commented back in
  piece by piece. If a task fails to compile with "file not found for module",
  check that only the modules that exist are uncommented.
- Test counts in the "Expected" lines are cumulative for `-p norte-help` and
  assume no extra tests were added. A higher count is fine; a lower one means
  something was skipped.
