# GUI-c keymap configurable — plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** la GUI resuelve teclas→comandos vía un motor de keymap CONFIGURABLE (mismo formato que la TUI), reemplazando el `input::key_to_action` hardcodeado; preset + capas sistema/usuario/proyecto; sin Lua, sin hot-reload.

**Architecture:** extraer el motor de keymap de `norte-tui::keymap` a `norte-frontend::keymap`, con un tipo de tecla NEUTRO (sin crossterm) — ese es el crux. Cada frontend convierte su evento nativo a `Chord` neutro (TUI: crossterm→Chord; GUI: nombre-GPUI→Chord). La TUI se refactoriza para consumir el motor (re-export, `just ci` verde cada paso). La GUI define su `COMMANDS`, su preset `orthodox.toml`, un loader de capas y un `run_command`. Spec: `docs/superpowers/specs/2026-07-20-gui-c-keymap-configurable-design.md`.

**Tech Stack:** norte-frontend (+serde +toml, opcional schemars); norte-tui (crossterm adapter + re-export); norte-gui (GPUI, config XDG).

**Convenciones (cada task):** TDD donde hay lógica; `just ci` verde tras cada task que toca el WORKSPACE (T1 norte-frontend, T2 norte-tui); `norte-gui` (T3) se verifica en su dir + manual. Español; commits `feat(frontend):`/`refactor(tui):`/`feat(gui):`.

**Datos verificados (2026-07-20, de `crates/norte-tui/src/keymap.rs` + `config.rs` + `lua/api.rs`):**
- `Chord { mods: crossterm KeyModifiers, code: crossterm KeyCode }` (privados); `Chord::{new(mods,code), from_event(mods,code) [descarta SHIFT en Char], is_bare_esc [priv]}` + `Display` (→ `"ctrl+f5"`, `"g"`, `"space"`, `"esc"`…).
- `parse_chord(&str) -> Result<Chord, KeymapError>`: tokens `enter|tab|esc|space|backspace|up|down|left|right|home|end|pgup|pgdn|insert|delete|f1..f12|<char>`; mods `ctrl|alt|shift`; rechaza `shift+<char>` (`ShiftWithChar`).
- `KeymapError` (thiserror): `Toml, BadChord, EmptySequence, UnknownCommand, ShiftWithChar, WrongLayerKey, EscInSequence, AmbiguousPrefix`.
- `RawBinding{on: Vec<String>, run: String}`, `RawSection{keymap, prepend_keymap, append_keymap}`, `KeymapFile{global, pane, viewer: RawSection, project: bool [serde skip]}` (todos `deny_unknown_fields`, `cfg_attr(feature="schema", JsonSchema)`); `KeymapFile::{has_full_keymap, mark_project, is_project}`.
- `parse_keymap(&str) -> Result<KeymapFile>` usa `crate::config::toml_diag(raw, &toml::de::Error) -> String` (span→`"line N: msg"`).
- `Screen{Browse→pane, Viewer→viewer}`. `Resolution{Run(String), Pending(usize), Reset}`. `Lookup{Exact,Prefix,Miss}` (priv).
- `Effective{bindings: Vec<(Vec<Chord>,String)>, discarded_lua_bindings: usize}` + `merge_ctx` (priv) + `Effective::{build, build_layered, build_for(preset, layers, known_commands, screen)}` (valida: WrongLayerKey preset↔usuario, parse de cada chord, EmptySequence, EscInSequence, `lua:` valida charset con `crate::lua::valid_name` [NO contra known_commands], resto contra `known_commands`, dedup primero-gana, prefix-free) + `bindings() -> Vec<(String,&str)>` + `discarded_lua_bindings()`.
- `Resolver{eff, pending}` + `Resolver::{new, pending, push(chord)->Resolution}` (Esc con pendiente cancela).
- `COMMANDS` (TUI, 33 comandos incl. `viewer.*`/`app.extensions`/`pane.history`…), `help_id`, `presets() [orthodox/vim/cua, include_str!]` — TODO ESTO se QUEDA en norte-tui.
- `norte_tui::lua::valid_name(&str)->bool` (priv): `!empty && len<=64 && [a-z0-9._-]`.
- `config::{toml_diag [priv], Layers{dirs: Vec<PathBuf>}, standard_layers() [/etc/norte|ProgramData, XDG|APPDATA, ./.norte]}`.
- `norte-frontend` deps hoy: norte-proto, norte-encoding, unicode-normalization. Falta serde+toml.
- GUI actual (`norte-gui`): `input::key_to_action(key,quick_active)->Action`; `on_key` despacha `Action`; teclas GUI-b: Tab, ↑↓/Home/End/PgUp/PgDn, Enter, Backspace, Insert(mark), F5/F6/F8(copy/move/delete), F9(cancel), imprimible→quick.

---

### Task 1: `norte-frontend::keymap` — motor + tecla NEUTRA (con tests movidos)

**Files:**
- Create: `crates/norte-frontend/src/keymap.rs`
- Modify: `crates/norte-frontend/src/lib.rs` (`pub mod keymap;`), `crates/norte-frontend/Cargo.toml` (+serde, +toml, feature `schema` opcional), raíz `Cargo.toml` si hace falta la dep en `[workspace.dependencies]` (ya están serde/toml).

Crea el módulo con el motor COMPLETO sobre tipos neutros. NO se toca norte-tui aquí (duplicación transitoria hasta T2; ambos crates compilan y `just ci` sigue verde porque norte-tui mantiene su copia).

- [ ] **Step 1: deps** — en `crates/norte-frontend/Cargo.toml`, `[dependencies]`: `serde = { workspace = true, features = ["derive"] }`, `toml.workspace = true`, `thiserror.workspace = true` (si no está). Añade `[features] schema = ["dep:schemars"]` + `schemars = { workspace = true, optional = true }` (copia el patrón de `norte-tui/Cargo.toml`). Justificación PR: serde/toml para parsear el keymap.toml compartido (permisivas, ya en el workspace).

- [ ] **Step 2: escribe los tests que fallan** — en `crates/norte-frontend/src/keymap.rs`, `mod tests` (copia los del motor de `norte-tui/src/keymap.rs` — busca `#[cfg(test)]` allí y trae los de `parse_chord`, `build_for`/prefix-free/merge/`Resolver`/Esc). Ajusta a los tipos neutros: donde el test TUI construía `Chord::new(KeyModifiers::CONTROL, KeyCode::Char('k'))` (crossterm), ahora `Chord::new(Mods{ctrl:true, ..Default::default()}, KeyCode::Char('k'))` (neutro). Añade un test NUEVO de parametrización de comandos:
```rust
    #[test]
    fn build_valida_contra_el_known_commands_dado() {
        let preset = parse_keymap(r#"[pane]
keymap = [{ on = ["x"], run = "foo.bar" }]"#).unwrap();
        // Con "foo.bar" conocido: OK.
        assert!(Effective::build(&preset, None, &["foo.bar"]).is_ok());
        // Sin él: UnknownCommand (el motor NO conoce comandos concretos).
        assert!(matches!(
            Effective::build(&preset, None, &["otro.cmd"]),
            Err(KeymapError::UnknownCommand { .. })
        ));
    }
```

- [ ] **Step 3: tipos neutros + parser** — escribe en `keymap.rs`:
```rust
//! Motor de keymap PURO compartido por los frontends (ADR 0006/0007): mapa
//! `(contexto, secuencia) → comando`, capas estilo Yazi, efectivo prefix-free
//! validado al cargar. Tecla NEUTRA (sin crossterm/gpui): cada frontend
//! convierte su evento nativo a [`Chord`] con [`Chord::new`].

use std::collections::HashSet;
use serde::Deserialize;

/// Código de tecla NEUTRO (espeja el set que acepta [`parse_chord`]; sin
/// dependencia de crossterm ni gpui).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    /// Carácter imprimible (espacio = `Char(' ')`).
    Char(char),
    /// Tecla de función F1..=F12.
    F(u8),
    /// Enter/Return.
    Enter,
    /// Tabulador.
    Tab,
    /// Escape.
    Esc,
    /// Backspace.
    Backspace,
    /// Flecha arriba.
    Up,
    /// Flecha abajo.
    Down,
    /// Flecha izquierda.
    Left,
    /// Flecha derecha.
    Right,
    /// Inicio.
    Home,
    /// Fin.
    End,
    /// Página arriba.
    PageUp,
    /// Página abajo.
    PageDown,
    /// Insert.
    Insert,
    /// Delete/Supr.
    Delete,
}

/// Modificadores NEUTROS de una tecla.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods {
    /// Ctrl.
    pub ctrl: bool,
    /// Alt.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
}

/// Una tecla con modificadores, en forma canónica. Los frontends la construyen
/// con [`Chord::new`] desde su evento nativo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chord {
    mods: Mods,
    code: KeyCode,
}

impl Chord {
    /// Chord canónico. En una tecla `Char` el carácter YA codifica shift, así
    /// que el modificador `shift` se descarta (paridad con el `from_event`
    /// crossterm de la TUI); en el resto se conserva.
    #[must_use]
    pub fn new(mods: Mods, code: KeyCode) -> Self {
        let mods = if matches!(code, KeyCode::Char(_)) {
            Mods { shift: false, ..mods }
        } else {
            mods
        };
        Self { mods, code }
    }

    fn is_bare_esc(self) -> bool {
        self.code == KeyCode::Esc && self.mods == Mods::default()
    }
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.mods.ctrl { f.write_str("ctrl+")?; }
        if self.mods.alt { f.write_str("alt+")?; }
        if self.mods.shift { f.write_str("shift+")?; }
        match self.code {
            KeyCode::Char(' ') => f.write_str("space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "f{n}"),
            KeyCode::Enter => f.write_str("enter"),
            KeyCode::Tab => f.write_str("tab"),
            KeyCode::Esc => f.write_str("esc"),
            KeyCode::Backspace => f.write_str("backspace"),
            KeyCode::Up => f.write_str("up"),
            KeyCode::Down => f.write_str("down"),
            KeyCode::Left => f.write_str("left"),
            KeyCode::Right => f.write_str("right"),
            KeyCode::Home => f.write_str("home"),
            KeyCode::End => f.write_str("end"),
            KeyCode::PageUp => f.write_str("pgup"),
            KeyCode::PageDown => f.write_str("pgdn"),
            KeyCode::Insert => f.write_str("insert"),
            KeyCode::Delete => f.write_str("delete"),
        }
    }
}
```
   A continuación PEGA desde `norte-tui/src/keymap.rs` (verbatim salvo los cambios abajo): `KeymapError` (íntegro), `parse_chord` (cambia el cuerpo para construir `Mods`/`KeyCode` neutros — ver Step 4), `RawBinding`/`RawSection`/`KeymapFile` + sus impls (`has_full_keymap`/`mark_project`/`is_project`), `Screen`, `parse_keymap` (ver Step 4 para `toml_diag`), `Resolution`, `Effective` + `merge_ctx` + `build`/`build_layered`/`build_for` + `bindings`/`discarded_lua_bindings`/`lookup`, `Lookup`, `Resolver`. Estos NO tocan crossterm directamente (usan `Chord` abstracto), así que se mueven tal cual.

- [ ] **Step 4: adapta los 3 puntos que tocaban crossterm/TUI** —
  1. `parse_chord`: sustituye `KeyModifiers::NONE`/`|=`/`contains` por el struct `Mods`. Cuerpo neutro:
```rust
pub fn parse_chord(s: &str) -> Result<Chord, KeymapError> {
    let bad = || KeymapError::BadChord { chord: s.to_owned() };
    let parts: Vec<&str> = s.split('+').collect();
    let (mods_txt, key_txt) = parts.split_at(parts.len().saturating_sub(1));
    let key_txt = key_txt.first().copied().filter(|k| !k.is_empty()).ok_or_else(bad)?;
    let mut mods = Mods::default();
    for m in mods_txt {
        let slot = match *m {
            "ctrl" => &mut mods.ctrl,
            "alt" => &mut mods.alt,
            "shift" => &mut mods.shift,
            _ => return Err(bad()),
        };
        if *slot { return Err(bad()); } // modificador repetido
        *slot = true;
    }
    let code = match key_txt {
        "enter" => KeyCode::Enter, "tab" => KeyCode::Tab, "esc" => KeyCode::Esc,
        "space" => KeyCode::Char(' '), "backspace" => KeyCode::Backspace,
        "up" => KeyCode::Up, "down" => KeyCode::Down, "left" => KeyCode::Left,
        "right" => KeyCode::Right, "home" => KeyCode::Home, "end" => KeyCode::End,
        "pgup" => KeyCode::PageUp, "pgdn" => KeyCode::PageDown,
        "insert" => KeyCode::Insert, "delete" => KeyCode::Delete,
        f if f.len() >= 2 && f.starts_with('f') => {
            let n: u8 = f[1..].parse().map_err(|_| bad())?;
            if (1..=12).contains(&n) { KeyCode::F(n) } else { return Err(bad()); }
        }
        c => {
            let mut chars = c.chars();
            match (chars.next(), chars.next()) {
                (Some(ch), None) => KeyCode::Char(ch),
                _ => return Err(bad()),
            }
        }
    };
    if mods.shift && matches!(code, KeyCode::Char(_)) {
        return Err(KeymapError::ShiftWithChar { chord: s.to_owned() });
    }
    // OJO: NO uses Chord::new aquí (descartaría el shift antes del check);
    // el check de arriba ya rechaza shift+Char, y para el resto shift se
    // conserva. Construye el Chord directo:
    Ok(Chord { mods, code })
}
```
  2. `parse_keymap`: reemplaza `crate::config::toml_diag(s, &e)` por un helper LOCAL `toml_diag` copiado a este módulo (privado):
```rust
fn toml_diag(raw: &str, e: &toml::de::Error) -> String {
    match e.span() {
        Some(s) => {
            let line = 1 + raw[..s.start.min(raw.len())].matches('\n').count();
            format!("line {line}: {}", e.message())
        }
        None => e.message().to_owned(),
    }
}
```
  3. `build_for`: reemplaza `crate::lua::valid_name(lua_name)` por un `valid_lua_name` LOCAL (privado, mismo charset):
```rust
/// Charset de un nombre de comando Lua (`lua:<nombre>`). Espeja
/// `norte_tui::lua::valid_name`; el motor lo usa solo para VALIDAR el binding,
/// no ejecuta Lua (eso es del frontend que tenga host).
fn valid_lua_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
}
```
  `lib.rs`: `pub mod keymap;` + rustdoc.

- [ ] **Step 5: verde** — `cargo nextest run -p norte-frontend keymap` (tests movidos + el nuevo) + `cargo clippy -p norte-frontend --all-targets -- -D warnings` + `cargo fmt --all`. Todos verdes. (norte-tui sigue con su copia; `cargo nextest run -p norte-tui` también verde, sin cambios.)

- [ ] **Step 6: Commit** — `feat(frontend): motor de keymap con tecla neutra en norte-frontend (GUI-c T1)`

---

### Task 2: la TUI consume el motor compartido (borra su copia, adaptador crossterm)

**Files:** Modify `crates/norte-tui/src/keymap.rs`, `crates/norte-tui/src/lua/api.rs` (si `valid_name` se re-exporta), call-sites (`main.rs`, `help.rs`, `config.rs`) si nombran tipos movidos.

- [ ] **Step 1: reescribe `norte-tui/src/keymap.rs`** — BORRA lo movido (Chord/KeyCode neutro NO — el de la TUI era crossterm; borra `Chord`, `parse_chord`, `KeymapError`, `RawBinding/Section`, `KeymapFile`, `Screen`, `parse_keymap`, `Resolution`, `Effective`, `merge_ctx`, `Lookup`, `Resolver` y sus tests movidos). Deja SOLO lo TUI-específico y añade el re-export + el adaptador crossterm:
```rust
//! Keymap de la TUI: re-exporta el motor compartido y aporta el adaptador de
//! crossterm + la lista de comandos + presets de la TUI.
pub use norte_frontend::keymap::{
    Chord, Effective, KeyCode, KeymapError, KeymapFile, Mods, Resolution, Resolver, Screen,
    parse_chord, parse_keymap,
};

use crossterm::event::{KeyCode as CtCode, KeyModifiers as CtMods};

/// Adaptador: evento de crossterm → [`Chord`] neutro. En `Char` el carácter ya
/// codifica shift (lo descarta `Chord::new`); el resto conserva mods.
#[must_use]
pub fn chord_from_crossterm(mods: CtMods, code: CtCode) -> Option<Chord> {
    let neutral = match code {
        CtCode::Char(c) => KeyCode::Char(c),
        CtCode::F(n) => KeyCode::F(n),
        CtCode::Enter => KeyCode::Enter,
        CtCode::Tab => KeyCode::Tab,
        CtCode::Esc => KeyCode::Esc,
        CtCode::Backspace => KeyCode::Backspace,
        CtCode::Up => KeyCode::Up,
        CtCode::Down => KeyCode::Down,
        CtCode::Left => KeyCode::Left,
        CtCode::Right => KeyCode::Right,
        CtCode::Home => KeyCode::Home,
        CtCode::End => KeyCode::End,
        CtCode::PageUp => KeyCode::PageUp,
        CtCode::PageDown => KeyCode::PageDown,
        CtCode::Insert => KeyCode::Insert,
        CtCode::Delete => KeyCode::Delete,
        _ => return None, // teclas que el keymap no modela
    };
    let m = Mods {
        ctrl: mods.contains(CtMods::CONTROL),
        alt: mods.contains(CtMods::ALT),
        shift: mods.contains(CtMods::SHIFT),
    };
    Some(Chord::new(m, neutral))
}
```
   CONSERVA en este fichero: `COMMANDS`, `help_id`, `presets()` (usan `parse_keymap` re-exportado). Mantén el `mod tests` de ESTOS (presets válidos, help_id, COMMANDS coherente) — NO los del motor (se fueron a T1).

- [ ] **Step 2: migra los call-sites de crossterm→Chord** — busca en `norte-tui/src` los usos de `Chord::from_event(mods, code)` (el viejo) y sustituye por `chord_from_crossterm(mods, code)` (devuelve `Option` — si `None`, la tecla no va al resolver, cae al manejo directo). Probablemente en `main.rs` (el loop de input). Verifica con `grep -rn "from_event\|Chord::new" crates/norte-tui/src`. Ajusta el flujo: `if let Some(chord) = chord_from_crossterm(k.modifiers, k.code) { resolver.push(chord) ... }`.

- [ ] **Step 3: `valid_name` de lua** — el motor ya valida el charset `lua:` con su copia interna; `norte_tui::lua::valid_name` SIGUE existiendo (lo usa `norte.command` en runtime). Déjalo como está (no lo borres). Son dos copias del mismo charset (una en el motor para validar el binding, otra en lua para el runtime) — anota un comentario `// charset espejado en norte_frontend::keymap (validación de binding)` en `valid_name` para trazabilidad. (Unificar = deuda menor, no bloquea.)

- [ ] **Step 4: verde workspace** — `cargo nextest run -p norte-tui` (sin regresión: el keymap resuelve igual) + `cargo nextest run -p norte-frontend` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all`. OJO: `help.rs`/`config.rs` pueden nombrar `keymap::Effective`/`Screen` — el re-export los cubre; si algún test comparaba `KeymapError` por variante, sigue igual (mismo enum, ahora en frontend).

- [ ] **Step 5: `just ci`** — EXIT=0 (cobertura core/vfs/proto intacta; keymap movido es norte-frontend permisivo). Revisa rustdoc links a items ahora en otro crate.

- [ ] **Step 6: Commit** — `refactor(tui): la TUI consume el motor de keymap de norte-frontend (GUI-c T2)`

---

### Task 3: GUI — chord neutro + COMMANDS + preset + capas + `run_command`

**Files:** Create `crates/norte-gui/src/keymap_presets/orthodox.toml`, `crates/norte-gui/src/keymap.rs`; Modify `crates/norte-gui/src/main.rs`, `crates/norte-gui/src/input.rs`, `crates/norte-gui/Cargo.toml` (+dep `toml`? NO — el parse lo hace norte-frontend; la GUI solo necesita rutas → +`dirs`/std env).

Trabaja DENTRO de `crates/norte-gui/` (excluido). Edición 2021 (sin let-chains).

- [ ] **Step 1: preset orthodox de la GUI** — crea `crates/norte-gui/src/keymap_presets/orthodox.toml` con SOLO los comandos de la GUI (contexto Browse). Refleja las teclas de GUI-b:
```toml
# Preset orthodox de la GUI (contexto Browse). Comandos: solo los de norte-gui.
[global]
keymap = [
    { on = ["q"], run = "app.quit" },
    { on = ["ctrl+c"], run = "app.quit" },
    { on = ["tab"], run = "pane.switch" },
]

[pane]
keymap = [
    { on = ["up"], run = "cursor.up" },
    { on = ["down"], run = "cursor.down" },
    { on = ["pgup"], run = "cursor.page-up" },
    { on = ["pgdn"], run = "cursor.page-down" },
    { on = ["home"], run = "cursor.top" },
    { on = ["end"], run = "cursor.bottom" },
    { on = ["enter"], run = "nav.enter" },
    { on = ["backspace"], run = "nav.parent" },
    { on = ["insert"], run = "mark.toggle" },
    { on = ["f5"], run = "pane.copy" },
    { on = ["f6"], run = "pane.move" },
    { on = ["f8"], run = "pane.delete" },
    { on = ["delete"], run = "pane.delete" },
    { on = ["f9"], run = "task.cancel" },
]
```

- [ ] **Step 2: módulo keymap de la GUI** — crea `crates/norte-gui/src/keymap.rs`:
```rust
//! Keymap de la GUI (GUI-c): COMMANDS del contexto Browse, preset orthodox
//! embebido, carga de capas (sistema/usuario/proyecto) y el adaptador
//! nombre-de-tecla-GPUI → `Chord` neutro. El MOTOR es `norte_frontend::keymap`.

use norte_frontend::keymap::{Chord, Effective, KeyCode, KeymapError, KeymapFile, Mods, Screen, parse_keymap};
use std::path::PathBuf;

/// Comandos que la GUI sabe ejecutar (contexto Browse). Fuente ÚNICA de
/// validación del keymap; nombres alineados con la TUI donde coinciden.
pub const COMMANDS: &[&str] = &[
    "app.quit", "pane.switch",
    "cursor.up", "cursor.down", "cursor.top", "cursor.bottom",
    "cursor.page-up", "cursor.page-down",
    "nav.enter", "nav.parent",
    "mark.toggle", "pane.copy", "pane.move", "pane.delete", "task.cancel",
];

/// El preset orthodox de la GUI, parseado. Panics solo si el TOML embebido es
/// inválido (lo cubre un test).
fn orthodox() -> KeymapFile {
    parse_keymap(include_str!("keymap_presets/orthodox.toml"))
        .unwrap_or_else(|e| panic!("preset orthodox embebido inválido: {e}"))
}

/// Directorios de capa en precedencia ASCENDENTE (sistema → usuario → proyecto),
/// como la TUI: `/etc/norte` (o `%ProgramData%`), config dir XDG, `./.norte`.
/// `NORTE_CONFIG_DIR` fuerza la capa de usuario (tests/headless).
fn layer_dirs() -> Vec<(PathBuf, bool)> {
    // (dir, es_proyecto)
    let mut v = Vec::new();
    #[cfg(unix)]
    v.push((PathBuf::from("/etc/norte"), false));
    let user = std::env::var_os("NORTE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(|x| PathBuf::from(x).join("norte")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/norte")));
    if let Some(u) = user { v.push((u, false)); }
    v.push((PathBuf::from("./.norte"), true));
    v
}

/// Construye el `Effective` del contexto Browse: preset orthodox + capas
/// `keymap.toml` presentes. Un keymap.toml inválido NO tumba la GUI: devuelve
/// el error (el caller cae al preset con banner). Fail-SAFE, no fail-closed.
///
/// # Errors
/// El primer `KeymapError` de una capa (parse/chord/comando/prefijo).
pub fn build_effective() -> Result<Effective, KeymapError> {
    let preset = orthodox();
    let mut layers: Vec<KeymapFile> = Vec::new();
    for (dir, is_project) in layer_dirs() {
        let path = dir.join("keymap.toml");
        match std::fs::read_to_string(&path) {
            Ok(src) => {
                let mut kf = parse_keymap(&src)?;
                if is_project { kf.mark_project(); }
                layers.push(kf);
            }
            Err(_) => {} // ausente/no legible: la capa no aporta.
        }
    }
    Effective::build_for(&preset, &layers, COMMANDS, Screen::Browse)
}

/// Adaptador: nombre de tecla GPUI (+mods +key_char) → `Chord` neutro. `None`
/// si la tecla no la modela el keymap (p. ej. teclas raras). Reusa el
/// vocabulario de nombres de `input.rs`.
#[must_use]
pub fn gpui_chord(key: &str, ctrl: bool, alt: bool, shift: bool, key_char: Option<&str>) -> Option<Chord> {
    let code = match key {
        "enter" => KeyCode::Enter, "tab" => KeyCode::Tab, "escape" => KeyCode::Esc,
        "backspace" => KeyCode::Backspace, "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up, "down" => KeyCode::Down, "left" => KeyCode::Left, "right" => KeyCode::Right,
        "home" => KeyCode::Home, "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp, "pagedown" => KeyCode::PageDown,
        "insert" => KeyCode::Insert, "delete" => KeyCode::Delete,
        f if f.len() >= 2 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            let n: u8 = f[1..].parse().ok()?;
            if (1..=12).contains(&n) { KeyCode::F(n) } else { return None; }
        }
        _ => {
            // Un imprimible: prioriza key_char (fidelidad de layout/shift).
            let s = key_char.filter(|s| !s.is_empty()).unwrap_or(key);
            let mut it = s.chars();
            match (it.next(), it.next()) {
                (Some(c), None) if !c.is_control() => KeyCode::Char(c),
                _ => return None,
            }
        }
    };
    Some(Chord::new(Mods { ctrl, alt, shift }, code))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preset_orthodox_valido_y_construye() {
        assert!(build_effective_from(&orthodox(), &[]).is_ok());
    }
    fn build_effective_from(p: &KeymapFile, l: &[KeymapFile]) -> Result<Effective, KeymapError> {
        Effective::build_for(p, l, COMMANDS, Screen::Browse)
    }
    #[test]
    fn gpui_chord_nombres_y_char() {
        assert_eq!(gpui_chord("f5", false, false, false, None), Some(Chord::new(Mods::default(), KeyCode::F(5))));
        assert_eq!(gpui_chord("up", false, false, false, None), Some(Chord::new(Mods::default(), KeyCode::Up)));
        assert_eq!(gpui_chord("k", true, false, false, Some("k")), Some(Chord::new(Mods{ctrl:true,..Default::default()}, KeyCode::Char('k'))));
        assert_eq!(gpui_chord("f99", false, false, false, None), None);
    }
}
```

- [ ] **Step 3: `run_command` + estado en main.rs** — en `struct NorteGui` añade `resolver: norte_frontend::keymap::Resolver,` y `keymap_error: Option<String>,`. En AMBOS constructores: construye el resolver:
```rust
        let (resolver, keymap_error) = match keymap::build_effective() {
            Ok(eff) => (norte_frontend::keymap::Resolver::new(eff), None),
            Err(e) => (
                norte_frontend::keymap::Resolver::new(
                    // fallback al preset puro: no puede fallar (test lo cubre)
                    keymap::build_effective_preset_only(),
                ),
                Some(format!("keymap: {e}")),
            ),
        };
```
   (Añade en `keymap.rs` un `pub fn build_effective_preset_only() -> Effective { Effective::build_for(&orthodox(), &[], COMMANDS, Screen::Browse).expect("preset orthodox válido") }` — INVARIANTE comentada.) Declara `mod keymap;` en main.rs. Pinta `keymap_error` en un banner (reusa el patrón de `errors`).
   Añade el método que ejecuta un comando resuelto:
```rust
    /// Ejecuta un comando del keymap (contexto Browse) sobre el estado.
    /// Reemplaza el `key_to_action` hardcodeado para las acciones nombradas.
    fn run_command(&mut self, cmd: &str, cx: &mut Context<Self>) {
        let f = self.focus;
        match cmd {
            "app.quit" => cx.quit(), // o el mecanismo de cierre que use la GUI
            "pane.switch" => self.focus = 1 - self.focus,
            "cursor.up" => self.panes[f].cursor_up(),
            "cursor.down" => self.panes[f].cursor_down(),
            "cursor.top" => self.panes[f].home(),
            "cursor.bottom" => self.panes[f].end(),
            "cursor.page-up" => self.panes[f].page_up(PAGE),
            "cursor.page-down" => self.panes[f].page_down(PAGE),
            "nav.enter" => self.activate_enter(cx),   // extrae la lógica actual de Action::Enter
            "nav.parent" => { if let Some(p) = self.panes[f].dir().parent() { self.cd(f, p, cx); } }
            "mark.toggle" => self.panes[f].toggle_mark(),
            "pane.copy" => self.open_transfer_modal(TransferKind::Copy),
            "pane.move" => self.open_transfer_modal(TransferKind::Move),
            "pane.delete" => self.open_delete_modal(),
            "task.cancel" => self.cancel_first_task(),  // extrae la lógica actual de Action::CancelTask
            _ => {} // comando desconocido en runtime: no-op (el keymap ya validó)
        }
        // Tras un movimiento de cursor, sigue el scroll (issue #87).
        self.follow_cursor(f);
    }
```
   (Extrae `activate_enter` y `cancel_first_task` de los brazos actuales `Action::Enter`/`Action::CancelTask` de `on_key` — mueve su cuerpo a métodos.)

- [ ] **Step 4: rewire `on_key`** — sustituye el despacho por `input::Action` (para las acciones nombradas) por el resolver. Mantén el orden: (1) modal abierto → `modal::on_key` (fijo); (2) quick-search activo → imprimibles al filtro (fallthrough actual con `quick_char`) + Backspace/Esc del quick; (3) si no → resolver:
```rust
        // (1) modal: teclas fijas (sin cambios).
        if let Some(m) = &mut self.modal { /* ... igual que hoy ... */ return; }

        let f = self.focus;
        let quick_active = self.panes[f].quick_visible().is_some();
        let mods = ks.modifiers;

        // (2) quick-search activo: el tipeo va al filtro (fallthrough), NO al
        // keymap. Solo cuando NO hay ctrl/alt (un ctrl+algo sí es comando).
        if quick_active && !(mods.control || mods.alt || mods.platform) {
            // reusa el manejo actual de Char/Backspace/Esc/Up/Down/Enter del quick.
            // (extrae a `fn quick_key(&mut self, ks, cx) -> bool` que devuelve
            //  true si consumió la tecla; si false, cae al resolver.)
            if self.quick_key(ks, cx) { cx.notify(); return; }
        }

        // (3) keymap: nombre GPUI → Chord → resolver.
        if let Some(chord) = keymap::gpui_chord(
            &ks.key, mods.control, mods.alt, mods.shift, ks.key_char.as_deref(),
        ) {
            match self.resolver.push(chord) {
                norte_frontend::keymap::Resolution::Run(cmd) => self.run_command(&cmd, cx),
                norte_frontend::keymap::Resolution::Pending(_) => {} // secuencia en curso
                norte_frontend::keymap::Resolution::Reset => {
                    // sin binding: si es un imprimible sin ctrl/alt, ABRE quick-search.
                    self.maybe_open_quick(ks); // abre quick si Char imprimible
                }
            }
        }
        cx.notify();
```
   NOTA: la interacción quick↔keymap es la parte delicada. Contrato claro: con quick ABIERTO, el tipeo alimenta el filtro (paso 2); con quick CERRADO, un imprimible sin binding (Resolution::Reset) ABRE el filtro (`maybe_open_quick`). Extrae helpers `quick_key` (maneja la tecla dentro del quick, como hoy: Char→quick_char, Backspace→quick_backspace/parent, Esc→quick_cancel, Up/Down→quick_up/down, Enter→quick_confirm) y `maybe_open_quick` (si `ks.key`/`key_char` es un char imprimible alfanumérico, `quick_start(Filter)` + `quick_char`). El VIEJO `input::key_to_action`/`Action` deja de usarse para navegación; puede quedar solo si algo más lo usa, o se borra. Verifica y borra `input.rs`/`Action` si queda huérfano (o déjalo si `maybe_open_quick` reusa su clasificación de imprimible — DRY: reusa `input::printable`-equivalente).

- [ ] **Step 5: build + tests + clippy** — `cd crates/norte-gui && cargo build -p norte-gui && cargo nextest run --bin norte-gui && cargo clippy --bin norte-gui --all-targets -- -D warnings && cargo fmt` (revierte reordenado incidental de imports). Los tests de `keymap` (gpui_chord, preset válido) + los existentes (modal/task_line/etc) verdes.

- [ ] **Step 6: verificación manual** — daemon + GUI. Confirma: teclas por defecto funcionan (Tab/flechas/Enter/Backspace/Insert/F5/F6/F8/F9) igual que GUI-b. Luego crea `~/.config/norte/keymap.toml` (o `NORTE_CONFIG_DIR`) con un rebind, p. ej.:
```toml
[pane]
prepend_keymap = [{ on = ["j"], run = "cursor.down" }, { on = ["k"], run = "cursor.up" }]
```
   y verifica que `j`/`k` mueven el cursor (y que NO abren quick-search, porque ahora son binding). Un keymap.toml con un comando inexistente → banner de error + la GUI sigue con el preset. Un binding `lua:foo` → se ignora (sin host).

- [ ] **Step 7: Commit** — `feat(gui): keymap configurable (motor compartido + preset + capas) (GUI-c T3)`

---

### Task 4: cierre — reviewers + gate + spec + push

- [ ] **Step 1: reviewers** (controller orquesta):
  - **rust-reviewer** sobre el rango (norte-frontend nuevo + refactor TUI + GUI): la extracción no cambió comportamiento (re-exports, adaptador crossterm 1:1), reglas duras, el fail-safe del keymap inválido (no fail-closed), sin unwrap fuera de invariante.
  - **encoding-auditor** sobre `parse_chord`/`gpui_chord`/`chord_from_crossterm`: un `Char` con bytes/acentos no se degrada; el keymap.toml se parsea sin asumir UTF-8 más allá de lo que TOML garantiza; el nombre de comando `lua:` valida charset ASCII. (Toca teclas, no paths — riesgo bajo, pero confirmar el trato de `Char`.)
  - Aplica hallazgos con TDD donde toquen lógica.
- [ ] **Step 2: gate** — `just ci` EXIT=0 (norte-frontend + norte-tui). `norte-gui` aparte: `cargo build/clippy/nextest -p norte-gui`.
- [ ] **Step 3: cerrar spec** — estado → IMPLEMENTADO + desviaciones (quick↔keymap contrato, `valid_name` duplicado charset = deuda menor, preset GUI propio, `input::Action` borrado/huérfano).
- [ ] **Step 4: deuda** — abre issue si queda: unificar el charset `valid_name` (motor vs lua), indicador de secuencia `Pending` en la GUI, hot-reload, contexto viewer.
- [ ] **Step 5: Commit + push** — `test: cierre GUI-c — reviewers + gate + spec IMPLEMENTADO (GUI-c T4)` + push del rango.

---

## Self-review del plan (hecho)

- **Cobertura spec:** motor compartido con tecla neutra (T1); TUI consume por re-export + adaptador crossterm (T2); GUI con COMMANDS + preset orthodox propio + capas + `gpui_chord` + `run_command` + wire (T3); capas completas (layer_dirs sistema/usuario/proyecto, mark_project); sin Lua (charset validado, sin host, ignora en runtime); carga al arranque (build_effective en `new`); modales fijos + quick fallthrough (paso 1/2 de on_key); fail-safe del keymap inválido (T3 Step 3); reviewers+gate (T4). Fuera de alcance (Lua host, hot-reload, viewer, cheatsheet, i18n) no tocado.
- **Tipos consistentes:** `Chord`/`KeyCode`/`Mods`/`Effective`/`Resolver`/`Resolution`/`Screen`/`KeymapFile`/`parse_chord`/`parse_keymap`/`KeymapError` definidos en T1 (norte-frontend), re-exportados por la TUI (T2), consumidos por la GUI (T3). `chord_from_crossterm` (T2, TUI) y `gpui_chord` (T3, GUI) son los dos adaptadores. `COMMANDS` de la GUI (T3) ≠ `COMMANDS` de la TUI (se queda, T2).
- **Placeholders:** ninguno. El punto delicado quick↔keymap (T3 Step 4) va con contrato explícito + los helpers a extraer (`quick_key`/`maybe_open_quick`); el implementador escribe el flujo real siguiendo el contrato.
- **Riesgo acotado:** T1 crea sin tocar la TUI (just ci verde con duplicación transitoria); T2 la migra (just ci verde, tests del motor ya en frontend cazan regresiones); T3 es norte-gui excluido (gate propio). El punto más delicado (quick↔keymap) tiene contrato escrito y helpers acotados.
