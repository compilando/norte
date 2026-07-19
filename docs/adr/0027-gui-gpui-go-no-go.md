# 0027 — GUI: GPUI go/no-go (M5 spike)

- Estado: accepted
- Fecha: 2026-07-19
- Decisores: oscar, Claude
- Relacionado: spec §18.3 («GUI: GPUI. Asumimos API inestable a cambio de
  rendimiento… spike de validación al inicio de M5, presupuesto 2 semanas»),
  tabla de milestones M5 («decisión GPUI vs Tauri con spike medido; primer
  frontend gráfico contra el mismo daemon»), criterio de salida M5 («GUI y TUI
  sobre la misma sesión simultáneamente»); diseño de referencia
  `docs/superpowers/specs/2026-07-19-m5-spike-gpui-design.md`; ADR 0020
  (norte-theme, la capa de color que cruza a la GPU); crate del spike
  `crates/norte-gui` (EXCLUIDO del workspace).

## Contexto y problema

La spec §18.3 se inclina a **GPUI** (el toolkit de Zed) para la GUI de norte
—rendimiento GPU a cambio de una API inestable—, pero exige validarlo con
datos duros ANTES de invertir en el dual-pane completo. M5 hito 1 es ese
spike: un binario GPUI (`crates/norte-gui`, excluido del workspace para que un
GPUI roto/nightly/pesado JAMÁS contamine el gate verde del core) que habla SOLO
`norte-proto` vía el `RemoteBackend` de `norte-core` (regla 7), read-only
(`fs.list`), y que se somete a cuatro criterios: (1) listado real por el
daemon, (2) GUI+TUI simultáneas contra el mismo socket, (3) norte-theme
aplicado por tipo de archivo, (4) medición de viabilidad. Este ADR recoge esa
medición y decide go/no-go. El spike se completó en las tareas T1–T7 del plan
`docs/superpowers/plans/2026-07-19-m5-spike-gpui.md`.

## Opciones consideradas

### A — GPUI (elegida)

Toolkit de Zed: retained-mode sobre GPU, escrito en Rust, mismo lenguaje que el
core. Consumido por **git pin de Zed** (no existe como `gpui` estable en
crates.io). En el rev pineado el backend GPU es **wgpu 29** (`gpui_wgpu`), no
Blade — dato relevante de portabilidad (wgpu abstrae Vulkan/Metal/DX/GL).

- ✅ Rust puro: cero FFI a un runtime web, cero proceso JS; el frontend enlaza
  `norte-proto`/`norte-theme` directamente y comparte tipos sin serializar a
  una capa web intermedia.
- ✅ El theming de `norte-theme` (ADR 0020) cruza a la GPU **sin retrabajo**:
  `Color::resolve(Truecolor)` → `gpui::Rgba` es un mapeo trivial y determinista
  (T3, con test), reutilizable tal cual en el MVP.
- ➖ API inestable (Zed la reescribe activamente; el split `gpui`/`gpui_platform`
  es reciente) y sin release en crates.io → dependemos de un rev git. Mitigado
  por la regla 7: el frontend solo habla `norte-proto`, así que un breaking de
  GPUI toca el crate de la GUI, jamás el core ni el protocolo.
- ➖ Deps GPU pesadas (wgpu/naga/cosmic-text/usvg) y build en frío largo;
  requiere un display (no corre headless).

### B — Tauri (alternativa, no elegida)

WebView nativo del sistema + backend Rust; UI en HTML/CSS/JS.

- ✅ API estable y en crates.io; binarios pequeños (usa el WebView del OS).
- ❌ Introduce una capa web (JS/DOM/CSS) y un modelo de dos procesos con puente
  serializado: la UI se escribe en otro lenguaje, el theming de `norte-theme`
  habría que reexportarlo a CSS, y cada dato cruza un IPC. Aleja la GUI del
  «mismo Rust, mismos tipos» que hace barato compartir `norte-proto`/tema.
- ❌ Rendimiento sujeto al WebView del sistema (variable entre OS), justo lo que
  §18.3 quiere evitar. Queda como plan B documentado si GPUI se volviera
  insostenible; el aprendizaje del spike GPUI no se tira.

## Medición (criterio 4)

Entorno: Linux (Arch, kernel 7.1.3), Rust **stable 1.96.1** (sin nightly, sin
`rust-toolchain.toml` propio del crate), display KDE Plasma/kwin_wayland
(`DISPLAY=:1` + `WAYLAND_DISPLAY=wayland-0`). GPUI @ rev
`f14fea9bf3c93797d5161f7440ed418655bc6c57` (zed-industries/zed, rama `main`,
2026-07-19), `gpui v0.2.2`.

| Métrica | Valor | Notas |
|---|---|---|
| **Toolchain** | stable **1.96.1**, sin nightly | GPUI compiló sin pedir nightly; MSRV del proyecto (1.94) intacto. El crate NO lleva `rust-toolchain.toml` propio. |
| **Build en frío (debug)** | **136 s** (2m 16s) | `cargo clean` (15.7 GiB, 26 864 ficheros) + `cargo build` desde cero: todo el árbol + GPUI + wgpu. |
| **Build release (desde cero)** | ~376 s (~6.3 min) | `cargo build --release` completo. |
| **Binario release** | **40 MiB** (40 928 408 B) | Arrastra el código GPU (wgpu/naga/cosmic-text) en el cuerpo. |
| **Binario debug** | 803 MiB | Con `debuginfo` completo (referencia, no distribuible). |
| **Deps transitivas** | **757 crates únicos** | `cargo tree` = 2530 líneas (con duplicados dedup `(*)`). |
| **Deps pesadas** | wgpu 29.0.4 (+ `wgpu-hal`/`-core`/`-types`/`-naga-bridge`), naga 29.0.4, `gpui_wgpu`, cosmic-text 0.19.0, tiny-skia 0.11.4, usvg 0.46.0, zed-font-kit 0.14.1-zed | Backend GPU = **wgpu**, NO Blade en este rev. |
| **Arranque en frío (cold-cache)** | ~2.2–2.9 s | Primer run tras el build: init de wgpu + page-cache frío. |
| **Arranque en caliente** | **~0.35 s** a primer frame pintado | Runs 2–3 estabilizados (375/341 ms), medidos hasta el primer render del listado real. `< 1 s` confirmado. |
| **API GPUI** | git pin de Zed, **NO** crates.io | Split `gpui`/`gpui_platform` reciente = API inestable confirmada; Zed la reescribe activamente. |
| **Libs de sistema (Linux)** | libxcb, libxkbcommon, wayland/x11 | Features `wayland`+`x11` de `gpui_platform` (backend `gpui_linux`). **Requiere display**: no corre headless. |
| **Licencias deps nuevas** | GPUI = Apache-2.0 | El crate excluido no pasa por `cargo deny`; si el MVP lo integra, entra en `deny.toml` (revisión del árbol wgpu/naga pendiente para el hito 2). |

## Los cuatro criterios — resultado

- **Criterio 1 — listado real por el daemon: ✅.** T4. `RemoteBackend::connect`
  (socket UDS, `spawn_cmd=None`) + `backend.list(&dir)` → 18 entradas reales
  pintadas en la ventana GPUI. Integración proto→GPU end-to-end, no un
  hello-world. Errores (daemon caído, `NotFound`, config inválida) → texto en la
  ventana, **jamás panic** (contrato verificado).
- **Criterio 2 — GUI+TUI simultáneas: ✅.** T6. Con un solo `norte daemon run`,
  la TUI real (vía pty tmux) y `norte-gui` conectadas a la vez al MISMO socket y
  MISMO dir, viendo el mismo listado, sin interferirse (`lsof` confirma ambas
  `CONNECTED`; screenshot real de la ventana GPUI coloreada). Confirma el
  criterio de salida de M5 en pequeño con un frontend gráfico.
- **Criterio 3 — norte-theme aplicado: ✅.** T5. `Theme::file_style` +
  `theme_map::to_gpui_rgba` (T3, con test) pintan el color por tipo exacto:
  dir `#5fafd7`, symlink `#5fafaf`, `.rs` `#d7875f` — los MISMOS RGB que
  `presets/default.toml`. El theming compartido (ADR 0020) cruza a la GPU sin
  retrabajo.
- **Criterio 4 — medición de viabilidad: ✅.** La tabla de arriba. Todos los
  números recogidos; ninguno prohibitivo (ver Decisión).

## Decisión

**GO.** Los cuatro criterios se cumplen: GPUI compila con la **stable 1.96.1**
(sin nightly, MSRV intacto), abre ventana y renderiza en Linux, el listado real
del daemon cruza a la GPU end-to-end, el theming de `norte-theme` casa al byte,
la GUI y la TUI conviven contra el mismo daemon, y el arranque en caliente
(~0.35 s a primer frame) es holgado. Se confirma §18.3 y se **arranca el plan
del MVP (M5 hito 2)**.

Los costes que resultarían prohibitivos —binario release desmesurado o build en
frío inaceptable— no se materializan: **40 MiB** de release es razonable para un
frontend con motor GPU embebido, y el build en frío (136 s / 2m 16s en debug;
~376 s en release) es un coste de CI/desarrollo aislado en un crate excluido,
no del gate del core. Se registran como **riesgos aceptados**, no como
bloqueantes.

## Consecuencias

- ➕ **Arranca el MVP (M5 hito 2):** dual-pane, navegación (`cd`), mutaciones por
  protocolo (copy/move/delete → journal/policy/undo), keymap, viewer (F3), i18n
  Fluent en la GUI y AccessKit. Todo condicionado a este go, ahora desbloqueado.
- ➕ **Piezas reutilizables ya validadas:** `theme_map::to_gpui_rgba` (con test,
  determinista, sin GPU) sobrevive tal cual al MVP; el patrón async→UI
  (`cx.spawn` + `oneshot` desde un runtime tokio aparte) queda probado.
- ➖ **Coste aceptado — dependencia por git pin de Zed:** sin release en
  crates.io, se fija por SHA (`f14fea9b…`). Actualizar de rev exige revalidar la
  API (el split `gpui`/`gpui_platform` demuestra que cambia). Mitigado por la
  **regla 7**: el impacto de un breaking de GPUI se acota al crate de la GUI —
  nunca toca `norte-proto`, el core ni los demás frontends.
- ➖ **Coste aceptado — build en frío largo + deps GPU pesadas** (757 crates,
  wgpu/naga/cosmic-text/usvg). El crate sigue **EXCLUIDO** del workspace hasta
  que el MVP lo estabilice; el gate del core (`cargo build --workspace`,
  cobertura, `just ci`) no lo compila y permanece verde. Cuando el MVP lo
  integre, su árbol de licencias (wgpu/naga) entra en `deny.toml`.
- ➖ **Coste aceptado — requiere display:** no corre headless (sin
  compositor/GPU). Los tests del MVP que toquen render necesitan un display real
  o quedan fuera del CI headless del core; la lógica testeable sin GPU (mapeos,
  estado) se aísla como en T3.
- ➕ **Tauri queda documentado como plan B** (opción B) con los motivos de su
  descarte: si GPUI se volviera insostenible, el pivote está razonado y el
  aprendizaje del spike no se pierde.
