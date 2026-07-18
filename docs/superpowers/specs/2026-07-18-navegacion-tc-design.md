# Navegación TC (quick search, historial, hotlist) — diseño

- Fecha: 2026-07-18
- Estado: aprobado (oscar); pendiente de plan
- Contexto: petición «moverme por los archivos como Total Commander».
  Proyecto hermano: `2026-07-18-live-search-design.md` (Alt+F7), que se
  implementa DESPUÉS de este. Frontend puro — cero cambio de protocolo
  (regla 7: es estado de UI + cd's que ya existen).

## Objetivo

Tres capacidades de navegación del TC en el TUI: quick search sobre el
listado del pane, historial de directorios por pane, y hotlist persistida.

## Decisiones

1. **Quick search con `/` explícito** (no tipear-directo: colisionaría con
   los bindings de letra del preset vim). Dos modos:
   - **Filtro incremental (default)**: el listado se REDUCE en vivo a los
     nombres que casan (substring case-insensitive, comparación en NFC —
     trampa macOS documentada en CLAUDE.md); Esc restaura el listado
     completo, Enter entra en el dir / se queda sobre el fichero
     seleccionado y cierra el filtro, ↑↓ navegan lo filtrado.
   - **Salto (`[ui] quick_search = "jump"` en norte.toml)**: mismo input,
     el listado NO cambia; el cursor salta al primer match, Tab al
     siguiente (con wrap), Esc cierra.
   - Filtra sobre lo YA drenado del listado paginado (ADR 0017): si el fill
     sigue en marcha, el contador muestra «parcial» (reutiliza la señal de
     `pane-loading`); al llegar lotes nuevos, el filtro se re-aplica.
   - Match sobre el `display_name` lossy (UX de tipeo, no identidad — los
     bytes reales siguen intactos en las entradas; operar usa el VPath).
   - El input del quick search vive como estado del pane (no modal): las
     teclas no-imprimibles no capturadas (F5, Tab en modo filtro…) actúan
     sobre la SELECCIÓN filtrada — feed-to-listbox pequeño gratis.

2. **Historial por pane, sesión** (no persistido): cada cd EXITOSO empuja
   el cwd anterior a un deque por pane (tope 30, dedup consecutivo).
   `Alt+↓` abre popup (mismo estilo que theme picker): ↑↓/Enter = cd,
   Esc cierra. Paths mostrados con `path_display` (lossy marcado).

3. **Hotlist persistida en el `norte.toml` del USUARIO** (capa usuario,
   jamás proyecto — un repo ajeno no inyecta favoritos), editada con
   `toml_edit` preservando comentarios (mismo patrón que `persist_ui_theme`):
   ```toml
   [[hotlist]]
   name = "proyectos"
   path = "file:///home/oscar/work"   # forma wire; remotos válidos
   ```
   `Ctrl+D` popup: Enter = cd al path (remoto incluido — pasa por el mismo
   camino de cd/connect que la navegación normal), `a` = añadir cwd actual
   (pide nombre, input de una línea), `d` = borrar la entrada seleccionada,
   Esc cierra. Paths inválidos en config: entrada mostrada con badge de
   error, no revienta la carga (misma filosofía que keymap roto = error
   claro, pero la hotlist es data, no config estructural: se degrada por
   entrada).

4. **Comandos nuevos en `COMMANDS`** (§8: paridad keymap/palette/scripting):
   `pane.quick-search`, `pane.history`, `pane.hotlist`, `hotlist.add` (los
   tres primeros con bindings default `/`, `alt+down`, `ctrl+d` en los tres
   presets; `hotlist.add` solo dentro del popup). Cada uno con su
   `help-cmd-*` en en/es (el test de paridad de ayuda OBLIGA).

## Componentes

- `norte-tui/src/nav.rs` (nuevo, lib): `QuickSearch` (estado: query bytes
  del input + modo + índices filtrados/match actual — lógica pura testeable
  sobre `Vec<Entry>`), `History` (deque + push/dedup), `Hotlist`
  (load/add/remove/persist vía config).
- `app.rs`: estados nuevos en `Pane` (quick search) y `App` (popups
  history/hotlist — enum `Overlay` o campos como theme_picker; seguir el
  patrón existente del theme picker).
- `config.rs`: `[[hotlist]]` en `NorteToml` (+ `[ui] quick_search`) +
  `persist_hotlist_*` con toml_edit.
- `ui.rs`: render del input de quick search (línea del pane), popups.
- `main.rs`: teclas → comandos; el filtro intercepta imprimibles mientras
  está activo.

## Errores

- Cd de historial/hotlist a un dir que ya no existe: mismo camino de error
  del cd normal (barra por categoría, #20). La entrada de historial se
  RETIRA si el cd falla con NotFound; la de hotlist NO (es config del
  usuario, se avisa y queda).
- Claves Fluent nuevas para popups y estados (`quicksearch-*`, `history-*`,
  `hotlist-*`) en en/es.

## Tests

1. `QuickSearch` puro: filtro substring NFC-insensitive sobre entries con
   nombres hostiles (corpus: NFD vs NFC casan; bytes no-UTF8 no rompen —
   se filtra sobre el lossy pero la entrada sobrevive intacta); modo salto
   con wrap; re-aplicar al llegar lote nuevo.
2. `History`: push/dedup/tope; retirada en NotFound.
3. `Hotlist`: round-trip por `norte.toml` con toml_edit (comentarios
   preservados), paths remotos wire, path inválido degrada por entrada.
4. Snapshots UI: pane con filtro activo, popup historial, popup hotlist.
5. Interacción: filtro activo + F5 opera sobre la entrada seleccionada
   FILTRADA (feed-to-listbox pequeño).

## Fuera de alcance

Persistir historial entre sesiones; hotlist jerárquica/submenús (TC lo
tiene; v2 si se echa de menos); quick search sobre resultados de live
search (llegará gratis al ser un listing normal); breadcrumb/drive bar.

## Criterio de salida

`/pro` filtra el listado a lo que casa y Enter entra; `Alt+↓` vuelve al
dir anterior; `Ctrl+D` + `a` guarda el cwd con nombre y sobrevive a
reiniciar; todo funciona igual sobre un dir sftp remoto.
