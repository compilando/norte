# Barra de estado por elementos (fase B del diseño VS Code)

Spec: conversación 2026-09-21 (análisis VS Code ↔ norte). Fase A (barra de
actividad, sin barra de teclas en la ventana) = ADR 0131, rama
`feat/barra-de-actividad`. Esta es la fase B: ADR 0132.

## Qué se decide

La barra de estado tiene DOS mitades y solo una es configurable.

- **Izquierda = la cadena de siempre**, sin tocar: espera > arrastre >
  mensaje > búsqueda viva > hook Lua > ruta con AVISOS (listado incompleto,
  reinterpretación de nombres, marcas podadas, sesión suelta). Son avisos:
  no se configuran ni se descartan por ancho antes que un elemento.
- **Derecha = elementos**, informativos, configurables y con clic:
  `[ui.status] items = ["position", "marks", "hidden", "sort", "encoding",
  "tasks", "layout", "notices"]` (orden = orden en pantalla, de izquierda a
  derecha). Lista vacía = mitad derecha vacía. Id desconocido = error de
  carga con el fichero, como `panel_bar_style`.

## El modelo (norte-frontend, `chrome/statusbar.rs`)

```rust
pub struct StatusItem {
    pub id: &'static str,        // estable: config, clic y test
    pub text: String,            // ya traducido y saneado
    pub tooltip: Option<String>,
    pub command: Option<&'static str>, // del catálogo; None = no se pulsa
    pub priority: u8,            // mayor = cede más tarde
}
pub struct StatusInput<'a> { /* hechos del pane con foco + globales */ }
pub fn items(input: &StatusInput<'_>, ids: &[&str], lang: Lang) -> Vec<StatusItem>;
/// Qué elementos caben en `width` celdas: descarta por prioridad (menor
/// primero), conserva el ORDEN configurado. Devuelve índices.
pub fn fit(items: &[StatusItem], width: usize, sep: usize) -> Vec<usize>;
```

Elementos v1 y su comando:

| id | texto | comando |
| --- | --- | --- |
| `position` | `3/120` (vacío con filtro activo) | — |
| `marks` | `2 marcadas · 4 MiB` (vacío sin marcas) | — |
| `hidden` | `ocultos: 5` / `ocultos visibles` | `pane.toggle-hidden` |
| `sort` | `Nombre ↑` | `pane.sort-menu` |
| `encoding` | `UTF-8` / `CP437` | `pane.names-encoding` |
| `tasks` | `⟳ 2` (vacío sin tareas) | `layout.processes` |
| `layout` | `orthodox` | `layout.pick` |
| `notices` | `!3` (vacío sin avisos) | `layout.log` |

Un elemento con texto vacío no ocupa nada (ni separador).

## Pasos

1. `norte-config`: `[ui.status] items`, validado; default = la tabla.
   Test de carga + id inválido. Schema (`NORTE_UPDATE_SCHEMA=1`).
2. `norte-frontend::statusbar`: `items` y `fit`, con tests de: orden
   conservado, descarte por prioridad, vacío no cuenta, ancho exacto.
   i18n `status-item-*` en los dos idiomas.
3. TUI: `compose_line` reserva la derecha para `fit(...)`; la ruta cede
   antes que un elemento de prioridad alta y DESPUÉS que los avisos. Zonas
   de clic por elemento (sustituye `NoticeZone`). Snapshots.
4. Ventana (puente 85): `StatusView.items: Vec<StatusItemView {id, text,
   tooltip, clickable}>`; acción `status_item_activate { id }` (por ID,
   como `settings_set`: la lista puede moverse entre pintado y clic). El
   renderer pinta a la derecha con `margin-left: auto`; descarte por
   ancho con el mismo `fit` en el host sobre el ancho declarado.
5. Ajustes: fila `ui.status-items` (tipo `Args`) — editable en los dos.
6. ADR 0132, changelog, ayuda `appearance` (es/en), memoria.

## Fuera de v1 (anotado en el ADR)

- Elementos aportados por plugins (cruza WIT → `security-reviewer`).
- Clic derecho en la ventana para ocultar un elemento (necesita escribir
  la lista: sale gratis cuando exista el paso 5 desde el renderer).
- `git.branch` (depende del panel gitlog; su fuente no es barata por frame).
