# Los ajustes, por secciones — plan de implementación

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** repartir los 33 ajustes en siete secciones y darles índice, cabecera
pegajosa, buscador con operadores, punto de modificado y restablecer, en la
terminal y en la ventana.

**Architecture:** todo lo que las dos superficies tienen que contar igual vive
en `norte-frontend::settings` (catálogo, secciones, filtro, «modificado»); cada
frontend solo pinta. La terminal scrollea a mano y la ventana con el navegador,
así que la cabecera pegajosa se resuelve dos veces por fuerza — pero el reparto
en secciones, el filtro y el «esto no es de fábrica» se calculan una sola vez.

**Tech Stack:** Rust (ratatui en la terminal), TypeScript + DOM en la webview,
Fluent para todo texto, `toml_edit` para escribir `norte.toml`, nextest y
vitest para probar.

**Spec:** `docs/superpowers/specs/2026-09-20-ajustes-por-secciones-design.md`

## Global Constraints

- **Toda cadena de usuario pasa por Fluent**, en `es` y `en`. Una clave que
  exista en un locale y no en el otro rompe el test de cobertura que ya hay
  (`fluent_keys_existen_en_ambos_locales_para_cada_entrada`).
- **Nada de lógica de negocio en los frontends** (regla 7 de CLAUDE.md). Si la
  terminal y la ventana necesitan la misma cuenta, la cuenta va en
  `norte-frontend`.
- **Sin `unwrap()`/`expect()` fuera de tests** salvo con un comentario que
  enuncie el invariante.
- **Errores tipados**: `thiserror` en librerías, `anyhow` solo en binarios.
- **Ningún comando nuevo del catálogo.** Las teclas de esta pantalla son
  locales del overlay; un comando nuevo obliga a bindear en los **siete**
  presets. Si en algún momento parece que hace falta uno, PARA y pregunta.
- **Esto no cruza el socket**: no se toca `norte-proto` ni se sube
  `PROTOCOL_VERSION`. Sí se sube `BRIDGE_VERSION` (80 → 81) en la tarea 9, y
  hay que subirlo **en los dos sitios**: `crates/norte-ui-host/src/bridge.rs:441`
  y `crates/norte-gui-tauri/ui/src/types.ts:12`.
- **El gate se paga por PLAN, no por tarea**: `just t <crate>` todo lo que haga
  falta; `just ci-fast` UNA vez cada ~3 tareas; `just ci` UNA vez al cerrar.
  Nunca uses el gate como depurador.
- **Nada de esperas.** Ni `sleep`, ni `timeout N tail -f /dev/null`, ni esperar
  a un «monitor»: no existe y nada te va a avisar.
- Rama: `feat/ajustes-por-secciones`.

---

### Task 1: Siete secciones en el modelo

**Files:**
- Modify: `crates/norte-frontend/src/settings.rs:27-33` (el `enum Section`),
  `:111-355` (la `section:` de cada una de las 33 entradas)
- Modify: `crates/norte-i18n/i18n/es.ftl:809`, `crates/norte-i18n/i18n/en.ftl:800`
- Test: `crates/norte-frontend/src/settings.rs` (módulo `tests` del final)

**Interfaces:**
- Produces: `Section::{Appearance, Panes, OpenWith, Input, Behavior, Plugins, Paths}`,
  `Section::label_key(self) -> &'static str`, `Section::ORDER: &[Section]`,
  `Section::stable_key(self) -> &'static str` (el nombre en inglés, sin
  traducir: lo usa `@section:` en la tarea 4).

- [ ] **Step 1: Escribe el test que falla**

En el módulo `tests` de `crates/norte-frontend/src/settings.rs`:

```rust
/// Ninguna sección se queda vacía y ninguna entrada se queda sin sitio.
///
/// Las 33 entradas nacieron todas en `General`, que es por qué la pantalla
/// se leía como una lista plana con un rótulo encima.
#[test]
fn cada_seccion_del_orden_tiene_al_menos_una_entrada() {
    for s in Section::ORDER {
        if matches!(s, Section::Plugins | Section::Paths) {
            continue; // No salen del catálogo.
        }
        assert!(
            catalog().iter().any(|d| d.section == *s),
            "la sección {s:?} no tiene ninguna entrada del catálogo"
        );
    }
}

/// Cada sección se dice en los dos idiomas. Media pantalla traducida es
/// peor que ninguna.
#[test]
fn cada_seccion_tiene_su_clave_en_ambos_locales() {
    for s in Section::ORDER {
        for lang in [norte_i18n::Lang::Es, norte_i18n::Lang::En] {
            let txt = norte_i18n::t_in(lang, s.label_key());
            assert!(
                !txt.is_empty() && !txt.contains(s.label_key()),
                "{:?} sin traducir en {lang:?}: {txt}",
                s
            );
        }
    }
}

/// La clave estable NO se traduce: es la que acepta `@section:appearance`
/// escriba quien escriba y en el idioma que sea.
#[test]
fn la_clave_estable_de_una_seccion_no_cambia_con_el_idioma() {
    assert_eq!(Section::Appearance.stable_key(), "appearance");
    assert_eq!(Section::OpenWith.stable_key(), "open-with");
}
```

`Section` necesita `PartialEq` (ya lo tiene) y `Debug` (ya lo tiene).
Comprueba cómo se nombran las variantes de `norte_i18n::Lang` antes de
escribir el segundo test: `grep -n "pub enum Lang" crates/norte-i18n/src/*.rs`.

- [ ] **Step 2: Compruébalo en rojo**

Run: `just t norte-frontend`
Expected: no compila — `Section::ORDER`, `label_key` y `stable_key` no existen.

- [ ] **Step 3: Escribe el enum y el reparto**

```rust
/// Bajo qué grupo de la pantalla de ajustes se pinta una entrada.
///
/// El orden de [`Self::ORDER`] es el de la pantalla, y es deliberado: lo que
/// se toca el primer día arriba, lo que es diagnóstico abajo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Tema, fuentes y lo que se ve.
    Appearance,
    /// Qué enseña un panel y qué cromo lo rodea.
    Panes,
    /// Con qué programa se abre un fichero.
    OpenWith,
    /// Teclado y ratón.
    Input,
    /// Lo que norte hace sin que se lo pidan.
    Behavior,
    /// Construida desde el manifiesto de un plugin aprobado — no hay
    /// entradas de esta clase en [`catalog`].
    Plugins,
    /// Las ubicaciones (configuración, estado, logs, socket). Tampoco sale
    /// del catálogo: la proyecta quien hospeda. Es una sección del MODELO
    /// para que el índice la liste como una más y para que la terminal la
    /// gane sin copiar la proyección de la ventana.
    Paths,
}

impl Section {
    /// Las secciones en el orden en el que se pintan.
    pub const ORDER: &'static [Section] = &[
        Section::Appearance,
        Section::Panes,
        Section::OpenWith,
        Section::Input,
        Section::Behavior,
        Section::Plugins,
        Section::Paths,
    ];

    /// La clave Fluent de su rótulo.
    #[must_use]
    pub fn label_key(self) -> &'static str { /* settings-section-<stable_key> */ }

    /// Su nombre ESTABLE, sin traducir: lo acepta `@section:` en cualquier
    /// idioma, y un fichero de traducción a medias no puede volverlo
    /// inencontrable.
    #[must_use]
    pub fn stable_key(self) -> &'static str { /* "appearance", "panes", … */ }
}
```

El reparto de las 33 entradas es el de la tabla D1 de la spec. Léela, no lo
inventes. `Section::General` desaparece; la clave `settings-section-general`
se queda en los dos `.ftl` sin usar, con un comentario `# obsoleta: se borra
en el ciclo siguiente`.

Las claves nuevas, en `es.ftl` junto a las que ya hay:

```
settings-section-appearance = Apariencia
settings-section-panes = Paneles y listado
settings-section-open-with = Abrir con
settings-section-input = Teclado y ratón
settings-section-behavior = Comportamiento
```

y en `en.ftl`: `Appearance`, `Panes and listing`, `Open with`,
`Keyboard and mouse`, `Behaviour`.

- [ ] **Step 4: Verde**

Run: `just t norte-frontend`
Expected: PASS. Si otro test se pone rojo por nombrar `Section::General`,
arréglalo aquí: no hay ningún `General` que conservar.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/settings.rs crates/norte-i18n/i18n/es.ftl crates/norte-i18n/i18n/en.ftl
git commit -F - <<'EOF'
feat(frontend): los ajustes se reparten en siete secciones

Las 33 entradas vivían en `Section::General` y la única otra variante
(`Plugins`) no aparece en el catálogo: la pantalla se leía como una lista
plana con un rótulo encima.
EOF
```

---

### Task 2: Cada fila sabe su sección y si la has tocado

**Files:**
- Modify: `crates/norte-frontend/src/settings.rs:508-532` (`struct Row`),
  `:617-640` (`build_rows_in`), `:412` (`current_value`, solo se lee)
- Test: mismo módulo `tests`

**Interfaces:**
- Consumes: `Section::ORDER` (tarea 1).
- Produces: campos públicos `Row.section: Section` y `Row.modified: bool`;
  `pub fn default_value(def: &SettingDef) -> String`.

- [ ] **Step 1: Escribe el test que falla**

```rust
/// El punto de «esto lo has tocado tú» se calcula contra el valor DE
/// FÁBRICA, con la misma función que pinta el valor: una tabla de defectos
/// escrita a mano se desincroniza del esquema en cuanto alguien cambia uno.
#[test]
fn sobre_la_config_por_defecto_no_hay_nada_modificado() {
    let cfg = FrontendConfig::default();
    for r in build_rows(&cfg, &[]) {
        assert!(!r.modified, "«{}» no debería salir modificada", r.name);
    }
}

#[test]
fn cambiar_un_campo_enciende_el_punto_de_esa_fila_y_de_ninguna_otra() {
    let mut cfg = FrontendConfig::default();
    cfg.ui_theme = "nord".to_owned(); // comprueba el nombre real del campo
    let filas = build_rows(&cfg, &[]);
    let tocadas: Vec<_> = filas.iter().filter(|r| r.modified).map(|r| r.id()).collect();
    assert_eq!(tocadas, vec![Some("ui.theme")]);
}

/// Una fila que no sale del catálogo nunca está modificada: no hay valor de
/// fábrica con el que compararla.
#[test]
fn una_fila_de_plugins_no_esta_modificada() {
    let resumen = PluginConfigSummary {
        plugin_id: "org.a".into(),
        name: "A".into(),
        key_count: 2,
    };
    let filas = build_rows(&FrontendConfig::default(), &[resumen]);
    let fila = filas.last().expect("hay fila de plugin");
    assert_eq!(fila.section, Section::Plugins);
    assert!(!fila.modified);
}
```

Antes de escribirlo, mira el nombre exacto del campo del tema en
`FrontendConfig` (`grep -n "ui_theme\|pub theme" crates/norte-frontend/src/config.rs`)
y la forma exacta de `PluginConfigSummary` (`settings.rs:493`).

- [ ] **Step 2: Rojo**

Run: `just t norte-frontend`
Expected: no compila (`Row` no tiene `section` ni `modified`).

- [ ] **Step 3: Impleméntalo**

```rust
/// El valor DE FÁBRICA de una entrada, como texto de pantalla.
///
/// Es [`current_value`] sobre una config por defecto: dos lecturas de la
/// misma función en vez de una segunda tabla que se desincronice.
#[must_use]
pub fn default_value(def: &SettingDef) -> String {
    current_value(def, &FrontendConfig::default())
}
```

En `build_rows_in`, cada fila del catálogo nace con
`section: def.section` y `modified: value != default_value(def)`; las de
`plugin_summary_rows`, con `section: Section::Plugins` y `modified: false`.

Si `FrontendConfig::default()` resulta caro, constrúyelo UNA vez fuera del
`map` — no una vez por fila.

- [ ] **Step 4: Verde**

Run: `just t norte-frontend`

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/settings.rs
git commit -m "feat(frontend): cada fila de ajustes dice su sección y si está tocada"
```

---

### Task 3: El índice es una proyección

**Files:**
- Modify: `crates/norte-frontend/src/settings.rs` (`impl SettingsState`, junto a
  `visible()`/`cursor()`)
- Test: mismo módulo

**Interfaces:**
- Consumes: `Row.section` (tarea 2), `Section::ORDER`/`label_key` (tarea 1).
- Produces:

```rust
pub struct SectionView {
    pub section: Section,
    pub title: String,
    pub visible: usize,
    pub first_row: Option<usize>,
}

impl SettingsState {
    pub fn sections(&self) -> Vec<SectionView>;
    pub fn jump_to(&mut self, section: Section);
}
```

`first_row` es una posición dentro de `visible()`, la misma unidad que
`cursor()` — no un índice real dentro de `rows()`. Escríbelo en el rustdoc:
mezclar las dos unidades es el bug que este proyecto ya pagó una vez.

- [ ] **Step 1: Test que falla**

```rust
#[test]
fn el_indice_lista_todas_las_secciones_aunque_el_filtro_vacie_alguna() {
    let mut s = SettingsState::new(rows());
    for c in "tema".chars() {
        s.push_char(c);
    }
    let idx = s.sections();
    assert_eq!(idx.len(), Section::ORDER.len(), "el índice no encoge al filtrar");
    let apariencia = idx.iter().find(|v| v.section == Section::Appearance).expect("apariencia");
    assert!(apariencia.visible > 0);
    let abrir = idx.iter().find(|v| v.section == Section::OpenWith).expect("abrir con");
    assert_eq!(abrir.visible, 0);
    assert_eq!(abrir.first_row, None);
}

#[test]
fn saltar_a_una_seccion_pone_el_cursor_en_su_primera_fila_visible() {
    let mut s = SettingsState::new(rows());
    s.jump_to(Section::Input);
    let fila = &s.rows()[s.visible()[s.cursor()]];
    assert_eq!(fila.section, Section::Input);
}

#[test]
fn saltar_a_una_seccion_vacia_no_mueve_nada() {
    let mut s = SettingsState::new(rows());
    for c in "tema".chars() {
        s.push_char(c);
    }
    let antes = s.cursor();
    s.jump_to(Section::OpenWith);
    assert_eq!(s.cursor(), antes, "una sección sin filas visibles no mueve el cursor");
}
```

`rows()` es el helper que ya usan los tests de ese módulo; mira cómo lo
construyen antes de copiarlo.

- [ ] **Step 2: Rojo** — `just t norte-frontend`, no compila.

- [ ] **Step 3: Implementa** `sections()` recorriendo `Section::ORDER` y
contando sobre `visible()`, y `jump_to` como `set_cursor(first_row)` cuando
hay `Some`. `jump_to` respeta el guard de edición que ya tiene `set_cursor`
(un click no mueve el cursor mientras se edita; un salto tampoco).

- [ ] **Step 4: Verde** — `just t norte-frontend`

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/settings.rs
git commit -m "feat(frontend): el índice de secciones de los ajustes"
```

---

### Task 4: El buscador con operadores y cuenta

**Files:**
- Modify: `crates/norte-frontend/src/settings.rs` (`recompute`, `fold_rows`, y
  un `parse_query` nuevo)
- Test: mismo módulo

**Interfaces:**
- Produces: `SettingsState::total()` y `SettingsState::shown()` (las dos
  cuentas de «7 de 33»); el filtro entiende `@modified` y `@section:<x>`.

- [ ] **Step 1: Test que falla**

```rust
#[test]
fn el_operador_modified_deja_solo_lo_tocado() {
    let mut cfg = FrontendConfig::default();
    cfg.ui_theme = "nord".to_owned();
    let mut s = SettingsState::new(build_rows(&cfg, &[]));
    for c in "@modified".chars() {
        s.push_char(c);
    }
    assert_eq!(s.shown(), 1);
    assert_eq!(s.rows()[s.visible()[0]].id(), Some("ui.theme"));
}

/// En los DOS idiomas: un fichero de traducción no puede ser la diferencia
/// entre encontrar algo y no encontrarlo.
#[test]
fn el_operador_section_acepta_el_nombre_traducido_y_el_estable() {
    for q in ["@section:appearance", "@section:apariencia"] {
        let mut s = SettingsState::new(rows());
        for c in q.chars() {
            s.push_char(c);
        }
        assert!(s.shown() > 0, "«{q}» no encontró nada");
        assert!(s
            .visible()
            .iter()
            .all(|&i| s.rows()[i].section == Section::Appearance));
    }
}

#[test]
fn los_operadores_se_combinan_con_el_texto() {
    let mut cfg = FrontendConfig::default();
    cfg.ui_theme = "nord".to_owned();
    cfg.ui_font_size = 18.0; // comprueba el nombre y el tipo reales
    let mut s = SettingsState::new(build_rows(&cfg, &[]));
    for c in "@modified tema".chars() {
        s.push_char(c);
    }
    assert_eq!(s.shown(), 1, "modificadas hay dos; con «tema», una");
}

/// Una arroba que no abre operador conocido es TEXTO. Nadie tiene que
/// escapar nada para buscar una arroba.
#[test]
fn una_arroba_suelta_es_texto_normal() {
    let mut s = SettingsState::new(rows());
    for c in "@nada".chars() {
        s.push_char(c);
    }
    assert_eq!(s.shown(), 0);
    assert_eq!(s.total(), s.rows().len(), "el total no lo toca el filtro");
}
```

- [ ] **Step 2: Rojo** — `just t norte-frontend`

- [ ] **Step 3: Implementa**

Un `parse_query(&[char]) -> (Vec<Token>, String)` puro: separa los tokens
`@modified` / `@section:<x>` del resto del texto libre. El texto libre sigue
pasando por `crate::nav::fold` contra el mismo `haystack` de siempre; los
tokens filtran aparte. `@section:<x>` compara `fold(x)` contra
`fold(stable_key)` y contra `fold(t_in(lang, label_key))` **de los dos
idiomas**, no solo del activo.

`total()` es `rows().len()`; `shown()` es `visible().len()`.

- [ ] **Step 4: Verde** — `just t norte-frontend`

- [ ] **Step 5: `just ci-fast` (primera y única de este tercio)**

Run: `just ci-fast` en primer plano. Si sale rojo, reproduce el fallo con
`just t <crate>`, arréglalo ahí, y NO vuelvas a correr el gate hasta el
siguiente punto marcado.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-frontend/src/settings.rs
git commit -m "feat(frontend): @modified y @section: en el buscador de ajustes"
```

---

### Task 5: `norte-config` sabe quitar una clave

**Files:**
- Modify: `crates/norte-config/src/load.rs` (junto a `persist_set:92` y
  `persist_keymap_unbind:1078`)
- Test: el módulo de tests de `load.rs` (mira cómo montan un `dir` temporal
  los tests de `persist_set`)

**Interfaces:**
- Produces:

```rust
/// Lo que una escritura de configuración hizo: dónde, y si cambió algo.
pub struct ConfigWrite {
    pub path: PathBuf,
    pub changed: bool,
}

/// Quita `key` de `[section]` en el `norte.toml` de `dir`.
///
/// # Errors
/// [`std::io::Error`] si el TOML existente no parsea o falla el I/O. Un
/// fichero que no está, una sección que no está o una clave que no está son
/// un no-op documentado, no un error: no hay nada que quitar.
pub fn persist_unset(dir: &Path, section: &str, key: &str) -> std::io::Result<ConfigWrite>;
```

- [ ] **Step 1: Test que falla**

```rust
#[test]
fn quitar_una_clave_la_borra_y_deja_las_vecinas() {
    let tmp = tempfile::tempdir().expect("tmp");
    persist_set(tmp.path(), "ui", "theme", toml_edit::Value::from("nord")).expect("set");
    persist_set(tmp.path(), "ui", "font_size", toml_edit::Value::from(18)).expect("set");
    let out = persist_unset(tmp.path(), "ui", "theme").expect("unset");
    assert!(out.changed);
    let txt = std::fs::read_to_string(&out.path).expect("leer");
    assert!(!txt.contains("theme"));
    assert!(txt.contains("font_size"));
}

#[test]
fn quitar_lo_que_no_esta_no_escribe_nada() {
    let tmp = tempfile::tempdir().expect("tmp");
    let out = persist_unset(tmp.path(), "ui", "theme").expect("unset sin fichero");
    assert!(!out.changed, "un fichero que no está es un no-op");
    assert!(!out.path.exists(), "y no lo crea");
}

/// Poner y quitar deja el fichero como estaba: si esto falla, restablecer
/// ensucia el `norte.toml` un poco en cada vuelta.
#[test]
fn poner_y_quitar_es_la_identidad() {
    let tmp = tempfile::tempdir().expect("tmp");
    persist_set(tmp.path(), "ui", "font_size", toml_edit::Value::from(18)).expect("set");
    let antes = std::fs::read_to_string(tmp.path().join("norte.toml")).expect("leer");
    persist_set(tmp.path(), "ui", "theme", toml_edit::Value::from("nord")).expect("set");
    persist_unset(tmp.path(), "ui", "theme").expect("unset");
    let despues = std::fs::read_to_string(tmp.path().join("norte.toml")).expect("leer");
    assert_eq!(antes, despues);
}
```

Comprueba el nombre real del fichero (`NORTE_TOML`) y cómo se llama el
`tempdir` que usan los tests vecinos antes de copiar esto.

- [ ] **Step 2: Rojo** — `just t norte-config`

- [ ] **Step 3: Implementa** siguiendo `persist_keymap_unbind` al pie de la
letra: **sin `create_dir_all`** (quitar no crea nada), `lock_config_file`
antes de leer, `NotFound` → no-op, `write_config_file` solo si `changed`.
Si al quitar la clave la tabla `[section]` se queda vacía, **déjala**: una
sección vacía es inofensiva y borrarla cambia el fichero más de lo que el
usuario pidió.

- [ ] **Step 4: Verde** — `just t norte-config`

- [ ] **Step 5: Commit**

```bash
git add crates/norte-config/src/load.rs
git commit -m "feat(config): persist_unset quita una clave de norte.toml"
```

---

### Task 6: Restablecer, y decir la verdad sobre las capas

**Files:**
- Modify: `crates/norte-frontend/src/settings.rs` (`impl SettingsState`)
- Modify: `crates/norte-i18n/i18n/es.ftl`, `crates/norte-i18n/i18n/en.ftl`
- Test: módulo `tests` de `settings.rs`

**Interfaces:**
- Consumes: `Row.modified` (tarea 2).
- Produces:

```rust
/// Una clave que hay que QUITAR de la capa de escritura, producida por
/// [`SettingsState::reset`]. Como [`PendingWrite`], es pura: quien la
/// recibe llama a `norte_config::persist_unset` fuera del hilo de pintado.
#[derive(Debug, Clone)]
pub struct PendingReset {
    pub section: &'static str,
    pub key: String,
    pub name: String,
}

impl SettingsState {
    /// Restablecer la fila del cursor, o `None` si no hay nada que quitar.
    pub fn reset(&mut self) -> Option<PendingReset>;
}
```

Claves Fluent nuevas (los dos locales):

```
settings-reset-done = «{$name}» vuelve al valor de fábrica
settings-still-set-elsewhere = «{$name}» sigue fijado por otra capa (perfil o proyecto): no vuelve al valor de fábrica
```

En inglés: `"{$name}" is back to its factory value` y
`"{$name}" is still set by another layer (profile or project): it does not go back to the factory value`.

- [ ] **Step 1: Test que falla**

```rust
#[test]
fn restablecer_una_fila_tocada_pide_quitar_su_clave() {
    let mut cfg = FrontendConfig::default();
    cfg.ui_theme = "nord".to_owned();
    let mut s = SettingsState::new(build_rows(&cfg, &[]));
    s.set_cursor(0); // ui.theme es la primera del catálogo
    let r = s.reset().expect("hay algo que quitar");
    assert_eq!((r.section, r.key.as_str()), ("ui", "theme"));
}

#[test]
fn restablecer_lo_que_ya_es_de_fabrica_no_escribe_nada() {
    let mut s = SettingsState::new(build_rows(&FrontendConfig::default(), &[]));
    s.set_cursor(0);
    assert!(s.reset().is_none());
}

#[test]
fn una_fila_de_plugins_no_se_restablece() {
    let resumen = PluginConfigSummary {
        plugin_id: "org.a".into(),
        name: "A".into(),
        key_count: 2,
    };
    let mut s = SettingsState::new(build_rows(&FrontendConfig::default(), &[resumen]));
    let ultima = s.visible().len() - 1;
    s.set_cursor(ultima);
    assert!(s.reset().is_none());
}
```

- [ ] **Step 2: Rojo** — `just t norte-frontend`

- [ ] **Step 3: Implementa** `reset` usando `wire_key(id)` (`settings.rs:394`)
para obtener `(section, key)`, devolviendo `None` cuando
`!row.modified || row.is_plugins_note()` o mientras se está editando.

**La parte incómoda, y es de diseño, no de código:** quitar la clave de TU
capa no siempre devuelve el valor de fábrica — si el sistema, el perfil o el
proyecto la fijan, el valor cambia y sigue sin ser el defecto. No se
construye procedencia de capas para explicarlo. Quien llama a `persist_unset`
reconstruye las filas (ya lo hace tras cada escritura) y **mira el punto**: si
la fila sigue `modified`, anuncia `settings-still-set-elsewhere`; si no,
`settings-reset-done`. El punto encendido es verdad sin maquinaria nueva.

- [ ] **Step 4: Verde** — `just t norte-frontend`

- [ ] **Step 5: Commit**

```bash
git add crates/norte-frontend/src/settings.rs crates/norte-i18n/i18n/es.ftl crates/norte-i18n/i18n/en.ftl
git commit -m "feat(frontend): restablecer un ajuste, y decirlo cuando otra capa lo fija"
```

---

### Task 7: La terminal clava la cabecera y cuenta

**Files:**
- Modify: `crates/norte-tui/src/ui/overlays.rs:1256-1388` (`SettingsLine`,
  `settings_line_plan`, `settings_list_rows`, `draw_settings`)
- Modify: `crates/norte-tui/src/ui/geometry.rs:119-140`
- Test: `crates/norte-tui/tests/snapshots_ui.rs`

**Interfaces:**
- Consumes: `SettingsState::sections()`, `shown()`, `total()`, `Row.section`,
  `Row.modified`.
- Produces: `settings_line_plan` emite una cabecera por **cada** sección con
  filas visibles (hoy solo conoce dos), y `draw_settings` pinta la de la
  sección del cursor FUERA del `Paragraph` que scrollea.

- [ ] **Step 1: Test que falla**

```rust
/// La cabecera de la sección del cursor está clavada arriba: se ve estés
/// donde estés dentro de ella, y cambia al cruzar a la siguiente.
#[test]
fn la_cabecera_clavada_sigue_a_la_seccion_del_cursor() {
    let mut app = app_base();
    let settings =
        norte_tui::app::Settings::new(norte_tui::settings::build_rows(&cfg_vacia(), &[]));
    app.settings = Some(settings);
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    ui::before_frame(&mut app, area);
    let arriba = render_80x24(&app);
    assert!(arriba.contains("Apariencia"), "la primera sección:\n{arriba}");

    // Hasta la última fila: la cabecera clavada tiene que ser OTRA.
    let ultima = app.settings.as_ref().expect("ajustes").visible().len() - 1;
    app.settings.as_mut().expect("ajustes").set_cursor(ultima);
    ui::before_frame(&mut app, area);
    let abajo = render_80x24(&app);
    assert!(
        !abajo.contains("Apariencia"),
        "al final de la lista, la cabecera clavada ya no es la primera:\n{abajo}"
    );
}

/// La cuenta del filtro. Sin ella, «no hay nada» y «lo tapé con una letra»
/// se ven igual.
#[test]
fn el_pie_dice_cuantos_ajustes_se_ven_de_cuantos() {
    let mut app = app_base();
    app.settings = Some(norte_tui::app::Settings::new(
        norte_tui::settings::build_rows(&cfg_vacia(), &[]),
    ));
    ui::before_frame(&mut app, ratatui::layout::Rect::new(0, 0, 80, 24));
    let pantalla = render_80x24(&app);
    assert!(pantalla.contains("33"), "el total se dice siempre:\n{pantalla}");
}
```

Ajusta el `33` a lo que devuelva `total()` de verdad (el catálogo puede haber
crecido); léelo del estado en el test en vez de escribirlo a mano si prefieres.

- [ ] **Step 2: Rojo** — `just t norte-tui`

- [ ] **Step 3: Implementa**

- `settings_line_plan` recorre `Section::ORDER` y emite
  `SettingsLine::Header(Section)` — la variante deja de llevar `&'static str`
  y lleva la sección, que es quien sabe su clave Fluent.
- `draw_settings` reserva la primera línea del área de lista para la cabecera
  de la sección del cursor y pinta el resto con el `Paragraph` que scrollea.
  Las cabeceras siguen estando dentro del plan (una sección que empieza a
  media pantalla necesita la suya), pero la de arriba está SIEMPRE.
- El punto de modificado va delante del nombre: `•` si la terminal lo
  admite, `*` si no. Mira cómo decide eso el resto de la TUI antes de
  inventar un método (`grep -rn "unicode\|ascii_only" crates/norte-tui/src | head`).
- El pie pasa a `/{query}  {shown} de {total}  {hint}`, con la cuenta por
  Fluent (`settings-count = {$shown} de {$total}`), no concatenada a mano.
- `geometry.rs` resta una línea más a `settings_list_rows` por la cabecera
  clavada. El ancla de la tarea del bug se queda: sigue siendo la regla
  correcta para las cabeceras que sí scrollean.

- [ ] **Step 4: Verde** — `just t norte-tui`. Los snapshots que ya existen van
a cambiar: revísalos uno a uno (`cargo insta review` si lo tienes, o
`INSTA_UPDATE=always` después de MIRARLOS) y no aceptes ninguno cuyo cambio no
sepas explicar.

- [ ] **Step 5: Commit**

```bash
git add crates/norte-tui crates/norte-i18n
git commit -m "feat(tui): cabecera de sección clavada y cuenta del filtro en ajustes"
```

---

### Task 8: El índice de la terminal, y las teclas

**Files:**
- Modify: `crates/norte-tui/src/ui/overlays.rs` (`draw_settings`)
- Modify: `crates/norte-tui/src/screens/settings.rs:59` (`on_settings_key`)
- Modify: `crates/norte-i18n/i18n/{es,en}.ftl` (la línea de pie)
- Test: `crates/norte-tui/tests/snapshots_ui.rs`

**Interfaces:**
- Consumes: `sections()`, `jump_to()`, `reset()`.
- Produces: el overlay con dos columnas y las teclas `tab`, `[`, `]`,
  `ctrl+r`.

- [ ] **Step 1: Test que falla**

```rust
/// Con sitio, el índice está; sin sitio, se va. La misma degradación que
/// hacen las columnas de un panel.
#[test]
fn el_indice_de_secciones_desaparece_en_una_terminal_estrecha() {
    let mut app = app_base();
    app.settings = Some(norte_tui::app::Settings::new(
        norte_tui::settings::build_rows(&cfg_vacia(), &[]),
    ));
    let ancha = ratatui::layout::Rect::new(0, 0, 100, 24);
    ui::before_frame(&mut app, ancha);
    let pantalla = render(&app, 100, 24);
    assert!(pantalla.contains("Abrir con"), "el índice lista las secciones:\n{pantalla}");

    let estrecha = ratatui::layout::Rect::new(0, 0, 50, 24);
    ui::before_frame(&mut app, estrecha);
    let pantalla = render(&app, 50, 24);
    assert!(
        !pantalla.contains("Abrir con"),
        "a 50 columnas no cabe el índice y manda la lista:\n{pantalla}"
    );
}

/// `]` lleva a la primera fila de la sección siguiente.
#[test]
fn corchete_salta_de_seccion() {
    let mut app = app_base();
    app.settings = Some(norte_tui::app::Settings::new(
        norte_tui::settings::build_rows(&cfg_vacia(), &[]),
    ));
    // Usa el mismo camino que el bucle: la función de teclas, no el estado.
    pulsa(&mut app, ']');
    let s = app.settings.as_ref().expect("ajustes");
    assert_eq!(s.rows()[s.visible()[s.cursor()]].section, Section::Panes);
}
```

`render(&app, w, h)` y `pulsa(...)` son helpers: mira cómo se llaman de
verdad en `snapshots_ui.rs` (hay un `render_80x24`; puede que tengas que
generalizarlo) y en los tests de teclas del overlay.

- [ ] **Step 2: Rojo** — `just t norte-tui`

- [ ] **Step 3: Implementa**

- El modal ensancha a `clamp(30, 100)`. Con ancho interior ≥ 60 se parte en
  índice de 20 celdas | lista; por debajo, solo lista.
- El índice pinta `título  (n)` por sección, apagada la que tiene `visible ==
  0`, y marcada la del cursor.
- `tab` mueve el foco entre índice y lista. OJO: en la pantalla `dialog`,
  `tab` ya significa `dialog.pane` — aquí es una tecla LOCAL del overlay y
  no pasa por el catálogo, que es justo por lo que no hay que tocar los siete
  presets. Compruébalo: el overlay intercepta antes que el despachador.
- `[` y `]` → `jump_to` de la sección anterior/siguiente **con filas
  visibles**; saltarse las vacías es lo que hace que la tecla sirva con un
  filtro puesto.
- `ctrl+r` → `reset()`; si devuelve `Some`, persiste con `persist_unset` por
  el mismo camino por el que `persist_setting` (`screens/settings.rs:152`)
  llama hoy a `persist_set` —fuera del hilo de pintado, regla 2— y anuncia
  según el punto (tarea 6).
- Las teclas nuevas van en la línea de pie (`settings-hint`), en los dos
  locales.

- [ ] **Step 4: Verde** — `just t norte-tui`

- [ ] **Step 5: `just ci-fast` (segunda y única de este tercio)**

- [ ] **Step 6: Commit**

```bash
git add crates/norte-tui crates/norte-i18n
git commit -m "feat(tui): índice de secciones, saltos y restablecer en ajustes"
```

---

### Task 9: El host proyecta secciones, filtra y restablece (puente 81)

**Files:**
- Modify: `crates/norte-ui-host/src/settings.rs:118-275` (`struct Ajustes`,
  `vista`, `mover`, `senalar`; el `debug_assert` de `:181` se cae con esto)
- Modify: `crates/norte-ui-host/src/action.rs:462-473`
- Modify: `crates/norte-ui-host/src/controller/settings.rs:88-140`
- Modify: `crates/norte-ui-host/src/bridge.rs:441` (`BRIDGE_VERSION` 80 → 81)
- Modify: `crates/norte-ui-host/src/dto.rs` (`SettingsView`, `SettingsSectionView`)
- Test: `crates/norte-ui-host/tests/controller/ajustes_escritura.rs`

**Interfaces:**
- Consumes: todo lo de las tareas 2-6.
- Produces: acciones `SettingsQuery { text: String }`,
  `SettingsJumpSection { section: String }` (la `stable_key`),
  `SettingsReset { row: u32 }`; `SettingsView` gana `index: Vec<SectionIndexView>`,
  `shown: u64`, `total: u64`, y cada `SettingRowView` gana `modified: bool`
  y `section: String`.

```rust
/// Una sección tal y como la pinta el índice de la ventana.
pub struct SectionIndexView {
    /// La clave estable (`appearance`, `open-with`…): la manda de vuelta
    /// `settings_jump_section`.
    pub key: String,
    /// Rótulo ya traducido al idioma del HOST.
    pub title: String,
    /// Cuántas filas visibles tiene con el filtro puesto.
    pub visible: u64,
}
```

- [ ] **Step 1: Test que falla**

```rust
/// Con filtro, un cursor plano miente: el `debug_assert` de `settings.rs`
/// decía justo que lo plano solo valía porque esta ventana no filtraba.
#[test]
fn con_filtro_el_cursor_apunta_a_la_fila_que_se_ve() {
    let mut c = controlador_con_ajustes_abiertos();
    c.accion(Action::SettingsQuery { text: "tema".into() });
    let v = c.vista().settings.expect("ajustes abiertos");
    let fila = fila_del_cursor(&v);
    assert!(fila.name.to_lowercase().contains("tema"));
    assert!(v.shown < v.total);
}

/// Restablecer algo que otra capa fija deja el punto ENCENDIDO y lo dice.
#[test]
fn restablecer_con_otra_capa_por_debajo_lo_anuncia() {
    // Monta dos capas: una de sistema con `theme = "nord"` y la de
    // usuario con `theme = "tokyonight"`.
    let mut c = controlador_con_capas(&[("sistema", "nord"), ("usuario", "tokyonight")]);
    c.accion(Action::SettingsReset { row: fila_de("ui.theme", &c) });
    let v = c.vista().settings.expect("ajustes");
    assert!(fila_de_id(&v, "ui.theme").modified, "el punto sigue encendido");
    assert!(c.ultimo_aviso().contains(&t("settings-still-set-elsewhere")));
}
```

Los helpers (`controlador_con_ajustes_abiertos`, `ultimo_aviso`) tienen
equivalentes en `ajustes_escritura.rs`: léelo entero antes de escribir el
test, y reutiliza sus nombres en vez de inventar otros.

- [ ] **Step 2: Rojo** — `just t norte-ui-host`

- [ ] **Step 3: Implementa**

El cursor de `Ajustes` pasa a contar filas **visibles** sobre el estado
compartido (`SettingsState`), no un índice plano sobre `filas ++ rutas`. Las
rutas se proyectan como la sección `Paths`, que ahora existe en el modelo.
Sube `BRIDGE_VERSION` a 81 en `bridge.rs` **y** en `types.ts`.

- [ ] **Step 4: Verde** — `just t norte-ui-host`

- [ ] **Step 5: Revisión antes de commitear**

Despacha `rust-reviewer` sobre el rango de commits de esta tarea. Dile qué
elegiste y qué no te convence: el cursor sobre visibles, la subida del puente
y si `SettingsQuery` puede llegar con la lista ya cambiada debajo. Un revisor
al que solo se le dice «revisa esto» devuelve una lista de la compra.
**El revisor no compila nada.**

- [ ] **Step 6: Commit** (con los BLOCKER y MAJOR ya aplicados)

```bash
git add crates/norte-ui-host crates/norte-gui-tauri/ui/src/types.ts
git commit -m "feat(ui-host): secciones, filtro y restablecer en ajustes (puente 81)"
```

---

### Task 10: La ventana, en dos paneles

**Files:**
- Modify: `crates/norte-gui-tauri/ui/src/render/settings.ts:24-129`
- Modify: `crates/norte-gui-tauri/ui/src/types.ts:12,977-984`
- Modify: `crates/norte-gui-tauri/ui/src/style.css:2107-2204`
- Test: `crates/norte-gui-tauri/ui/tests/render.test.ts`

**Interfaces:**
- Consumes: el `SettingsView` de la tarea 9.

- [ ] **Step 1: Test que falla**

```ts
it("pinta el índice y manda al host la sección elegida", () => {
  const { screen, enviados } = montar();
  screen.paint(conAjustes());
  const indice = [...document.querySelectorAll(".settings-index-item")];
  expect(indice).toHaveLength(7);
  (indice[2] as HTMLElement).click();
  expect(enviados.at(-1)).toEqual({
    action: "settings_jump_section",
    section: "open-with",
  });
});

it("el buscador manda el texto, no una tecla", () => {
  const { screen, enviados } = montar();
  screen.paint(conAjustes());
  const caja = document.querySelector(".settings-search") as HTMLInputElement;
  caja.value = "tema";
  caja.dispatchEvent(new Event("input", { bubbles: true }));
  expect(enviados.at(-1)).toEqual({ action: "settings_query", text: "tema" });
});

it("conserva el sitio del scroll al repintar", () => {
  const { screen } = montar();
  screen.paint(conAjustes());
  const lista = document.querySelector(".settings-rows") as HTMLElement;
  lista.scrollTop = 120;
  screen.paint(conAjustes());
  const despues = document.querySelector(".settings-rows") as HTMLElement;
  expect(despues.scrollTop).toBe(120);
});
```

`montar()` y `enviados` ya existen en `render.test.ts`: mira su forma real
antes de copiar (el nombre del espía puede no ser `enviados`). Y actualiza
el `conAjustes()` que ya hay para que traiga `index`, `shown`, `total` y
`modified`.

- [ ] **Step 2: Rojo**

Run: `cd crates/norte-gui-tauri/ui && npx vitest run tests/render.test.ts -t "ajustes"`

- [ ] **Step 3: Implementa**

- `<nav class="settings-index">` + `<main>`, `display: grid` a dos columnas;
  por debajo de ~700px de ancho, una sola (el índice arriba, en horizontal).
- `.settings-group { position: sticky; top: 0 }` con fondo opaco del tema —
  una cabecera translúcida sobre filas que pasan por debajo es ilegible.
- El punto de modificado es un `<span class="settings-dot">` con
  `aria-label`, no solo color: el color no es información.
- Un botón «restablecer» por fila modificada, que manda `settings_reset`.
- Conserva `scrollTop` alrededor del `replaceChildren`.
- Sin `style` inline: la CSP lo bloquea (usa CSSOM, como hace `paintTheme`).

- [ ] **Step 4: Verde** — `npx vitest run tests/render.test.ts`

- [ ] **Step 5: Míralo de verdad**

`just link-gui` y abre F11. Un renderer verde en jsdom no dice nada sobre si
la cabecera pegajosa tapa la primera fila. Comprueba: scroll largo, filtro que
vacía una sección, y la ventana estrecha.

- [ ] **Step 6: Commit**

```bash
git add crates/norte-gui-tauri
git commit -m "feat(gui): los ajustes en dos paneles, con índice y buscador"
```

---

### Task 11: ADR, ayuda, changelog y memoria

**Files:**
- Create: `docs/adr/0129-ajustes-por-secciones.md` (usa `/adr`, que numera
  solo — comprueba el número libre, puede no ser 0129)
- Modify: `crates/norte-help/topics/es/settings.md` y su gemelo en `en/`
- Modify: `CHANGELOG.md` (`## [Unreleased]` → `### Added`)
- Modify: la memoria del proyecto

- [ ] **Step 1: El ADR**

Decisión: los ajustes se organizan en siete secciones con índice, y
restablecer quita la clave de la capa de escritura sin construir procedencia
de capas — el punto de «modificado» dice la verdad porque se calcula contra
el valor de fábrica. Consecuencia que hay que escribir: **una capa inferior
puede dejar el punto encendido después de restablecer**, y eso es correcto.

- [ ] **Step 2: El tema de ayuda, en los DOS locales**

Las teclas nuevas (`tab`, `[`, `]`, `ctrl+r`) no están en ningún preset
porque son locales del overlay: si no se escriben aquí, no están escritas en
ninguna parte.

- [ ] **Step 3: Changelog**

Una entrada en `### Added` que diga qué ve el usuario, no qué se refactorizó.

- [ ] **Step 4: `just ci` (la única de la rama)**

En PRIMER PLANO y recipe a recipe (`lint`, `test`, `docs`, `cov`), nunca por
una tubería a `tail`: si la matan, cinco minutos de cómputo no reportan nada.

- [ ] **Step 5: Commit y cierre**

```bash
git add docs CHANGELOG.md crates/norte-help
git commit -m "docs(settings): ADR, ayuda y changelog de los ajustes por secciones"
```

---

## Notas de ejecución

- **Las tareas 7-8 (terminal) y 9-10 (ventana) son independientes**: crates
  disjuntos. Si se reparten entre dos agentes, cada uno en su **worktree**
  (`scripts/wt.sh`), nunca dos en el mismo árbol.
- **Las tareas 1-6 son puras y rápidas**: hazlas seguidas, sin gate entre
  medias más allá de `just t norte-frontend`, que tarda segundos.
- Si tocas un item documentado: `cargo test -p <crate> --doc`. Si escribes un
  enlace de rustdoc: `cargo doc -p <crate> --no-deps`. Ni `just t` ni `just c`
  los miran, y el gate sí.
