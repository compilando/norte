# Los filtros de búsqueda en la ventana (y el formulario del puente)

Punto 3 de la batida de funcionalidades (`docs/batida-de-funcionalidades-2026-09-20.md`).

**El hueco.** La TUI pide la búsqueda con siete campos y cuatro interruptores
(protocolo 0.81.0). La ventana manda **un glob de nombre y nada más**:
`controller/search.rs::lanzar_busqueda` construye `FsSearchParams` con
`name_glob` y `max_hits`, y su diálogo tiene un solo `input`.

**El wire NO se toca.** `FsSearchParams` ya lleva clases, tamaños, fecha,
exclusiones, palabra entera, recursividad y codificación desde 0.81.0. Lo que
falta es la forma de PREGUNTARLO en la ventana.

**Lo que falta de verdad son dos cosas, y conviene no confundirlas:**

1. Un **formulario en el puente**, que hoy no existe: `DialogView` lleva
   `input: Option<String>` y un flag de secreto, y ningún diálogo de la
   ventana tiene más de un campo. Se hace **genérico**, no a medida de la
   búsqueda: cualquier diálogo futuro con formulario lo quiere, y ésa es la
   razón que da la batida para hacerlo bien.
2. Un **modelo compartido** del formulario de búsqueda. Hoy vive entero en
   `norte-tui` (modelo, parsers, validación y mapeo a `FsSearchParams`).
   Copiarlo a la ventana sería la divergencia que ya costó una vez: ADR 0077
   y `norte-frontend/src/search_status.rs`, que existe precisamente porque
   los dos frontends decidían el desenlace de una búsqueda por su cuenta y
   con precedencias distintas.

## Restricciones

- Regla 7: el mapeo formulario → `FsSearchParams` es lógica, y no puede vivir
  en un frontend. Sube a `norte-frontend`.
- El reloj se INYECTA (`ahora_ms`): «cambiado hace 7 días» se cuenta desde que
  se pulsa Enter, y un mapeo que lee el reloj por su cuenta no se puede probar.
- Las claves i18n **ya existen** (`search-name`, `search-content`,
  `search-exclude`, `search-min-size`, `search-max-size`, `search-days`,
  `search-encoding`, `search-regex`, `search-case`, `search-whole-word`,
  `search-recursive`, `search-kinds*`, `search-empty`, `search-bad-field`).
  No se inventan otras: la ventana pinta las mismas.
- Un campo ilegible se SEÑALA antes de lanzar (`campo_ilegible`). Una búsqueda
  que ignora en silencio un `1 gigabyte` mal escrito devuelve el árbol entero
  y se lee igual que un resultado.

## T1 — `norte-frontend::search`: el formulario, compartido

Mueve, sin cambiar comportamiento, desde `norte-tui/src/app/pane.rs` y
`norte-tui/src/jobs/search.rs`:

```rust
pub struct SearchForm { /* name, content, exclude, min_size, max_size, days,
                           encoding: String; field: SearchField;
                           regex, case, whole_word, recursive: bool;
                           kinds: SearchKinds */ }
pub enum SearchField { Name, Content, Exclude, MinSize, MaxSize, Days, Encoding }
impl SearchField { pub const ORDEN: [SearchField; 7]; pub fn clave(self) -> &'static str; }
pub enum SearchKinds { Todo, Ficheros, Carpetas }   // siguiente(), clave(), wire()

pub fn parse_size(s: &str) -> Option<u64>;   // 1024, 500k, 2.5G, 1T
pub fn parse_days(s: &str) -> Option<u32>;   // entero, tope 36 500

impl SearchForm {
    pub fn texto(&self, f: SearchField) -> &str;
    pub fn set_texto(&mut self, f: SearchField, s: String); // NUEVO: la ventana
                    // manda el texto ENTERO de un campo, no pulsación a pulsación
    pub fn push_char(&mut self, c: char);   // los que ya usa la TUI
    pub fn backspace(&mut self);
    pub fn toggle_field(&mut self);
    pub fn toggle_regex(&mut self); /* case, whole_word, recursive, cycle_kinds */
    pub fn has_criteria(&self) -> bool;
    pub fn exclude_names(&self) -> Vec<String>;
    pub fn campo_ilegible(&self) -> Option<SearchField>;
}

/// El mapeo, con el reloj DICHO y no leído.
pub fn params(form: &SearchForm, root: VPath, ahora_ms: i64, max_hits: u32)
    -> norte_proto::methods::FsSearchParams;
```

Los tests de `parse_size`/`parse_days` viajan con ellos. Nuevos:

- `params` con `days = "7"` y un `ahora_ms` fijo da el `mtime_after` exacto
  (hoy no se puede probar: lee `SystemTime::now`).
- `params` respeta el eje del toggle `regex` (nombre y contenido a la vez).
- `campo_ilegible` señala tamaño, días y codificación, en ese orden.

`norte-tui` re-exporta los nombres viejos (`SearchDialog = SearchForm`) para
que T5 sea mecánico y este paso no toque el pintado.

## T2 — Puente 91: un diálogo puede llevar un FORMULARIO

`DialogView` gana un campo, y los diálogos que no lo usan lo mandan vacío:

```rust
pub struct DialogFieldView {
    pub id: String,              // estable: config, clic y test
    pub label_key: String,       // Fluent; el renderer no traduce nada más
    pub value: String,           // ya enmascarado y acotado
    pub hostile: bool,
    pub kind: DialogFieldKind,
}
pub enum DialogFieldKind {
    Text,
    Toggle { on: bool },
    Cycle { value_key: String }, // la etiqueta del valor actual, ya elegida en Rust
}
```

Y la acción de vuelta:

```rust
UiAction::DialogField { id: ModalId, field: String, value: DialogFieldValue }
// DialogFieldValue = Text(String) | Toggled | Cycled
```

`DialogInput` se queda tal cual: es el camino del diálogo de UN campo y de la
contraseña, y no se toca (#327 dice por qué el secreto no viaja por aquí).

Goldens: `NORTE_BLESS=1` reescribe `crates/norte-ui-host/tests/golden/*.json`
(`variants`, `changes`, `updates`, `actions` llevan diálogo). Leer el diff: debe
enseñar el campo nuevo y la acción nueva, y nada más. `BRIDGE_VERSION` a **91**
en `bridge.rs` (con su bullet de historia) y en `ui/src/types.ts`.

## T3 — La ventana: el diálogo de búsqueda pasa a formulario

- `Tecleado::Formulario(Box<SearchForm>)`, al lado de `Texto` y `Secreto`.
- `pedir_busqueda` monta los siete campos y los cuatro interruptores desde
  `SearchField::ORDEN` (la misma lista que pinta la TUI: dos listas escritas a
  mano se separan en cuanto entra un campo).
- `UiAction::DialogField` → `escribir_campo_de_dialogo`: valida el tope, mete
  el texto crudo en el formulario y proyecta el enmascarado, igual que
  `escribir_en_dialogo` hace hoy con el campo único.
- Al confirmar, `Pendiente::Buscar { root }` se lleva el formulario: sin
  criterio → `search-empty`; campo ilegible → `search-bad-field` señalando el
  campo; si no, `lanzar_busqueda` con `search::params(...)` COMPLETO.

## T4 — El renderer

`render/dialogs.ts` pinta `fields` antes de las opciones: texto con su
`<input>`, interruptor con su casilla, ciclo con su botón. Cada uno manda
`dialog_field`. `types.ts` transcribe los tipos nuevos y sube `BRIDGE_VERSION`.
Tests: `contract.test.ts` (el corpus golden) y `render.test.ts` (un diálogo con
formulario se pinta y teclear en el tercer campo manda el id del tercer campo,
no el del primero).

## T5 — La TUI consume el modelo compartido

Borrar el duplicado de `app/pane.rs` y `jobs/search.rs`, dejar el pintado y las
teclas donde están. Es mecánico si T1 dejó los re-exports.

## T6 — Cierre

- **ADR 0143**: un diálogo puede llevar un formulario (por qué genérico y no a
  medida de la búsqueda; por qué el modelo vive en `norte-frontend`).
- Ayuda: `finding.md` en los dos idiomas — hoy describe los filtros como teclas
  de función del terminal, y a partir de aquí la ventana los tiene también.
- Goldens del CLI si la ayuda cambia, `just ci-fast` UNA vez al cerrar.

## Lo que NO entra

- Varias raíces de búsqueda, seguir enlaces, buscar dentro de comprimidos y la
  pestaña de resultados persistente: siguen en la batida, y ninguna es un
  problema de formulario.
- `max_hits` en el formulario: el diálogo de la TUI tampoco lo expone.
