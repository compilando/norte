# M5 hito 1 — spike GPUI (go/no-go) — diseño

- Fecha: 2026-07-19
- Estado: **COMPLETO** (T1–T7 implementados; spike cerrado 2026-07-19).
- Resultado: **GO** — los 4 criterios cumplidos (listado real por el daemon,
  GUI+TUI simultáneas, norte-theme por tipo al byte, medición sin números
  prohibitivos); GPUI compila con stable 1.96.1 (sin nightly), renderiza en
  Linux y el theming compartido cruza a la GPU sin retrabajo. Arranca el MVP
  (M5 hito 2). Medición y decisión en `docs/adr/0027-gui-gpui-go-no-go.md`.
- Contexto: spec §18.3 («GUI: GPUI. Asumimos API inestable a cambio de
  rendimiento… spike de validación al inicio de M5, presupuesto 2 semanas»),
  tabla de milestones M5 («decisión GPUI vs Tauri con spike medido; primer
  frontend gráfico contra el mismo daemon»), criterio de salida M5 («GUI y
  TUI sobre la misma sesión simultáneamente»). Este es el HITO 1 de M5: el
  spike de decisión, NO el MVP.

## Objetivo

Validar GPUI como framework de la GUI de norte con datos duros para un ADR
go/no-go, en ~2 semanas. Probar que GPUI + el modelo headless-core encajan
ANTES de invertir en el dual-pane completo.

## Decisiones (con el porqué)

1. **Spike de decisión, no MVP.** La spec ya se inclina a GPUI, pero pide
   validación medida. El spike acota el riesgo de GPUI (API inestable,
   toolchain, deps GPU) a 2 semanas y produce un ADR; el MVP (hito 2) queda
   condicionado a su go.
2. **`crates/norte-gui` EXCLUIDO del workspace default.** No en
   `default-members`; `cargo build --workspace`, el gate de cobertura
   (core/vfs/proto) y `just ci` del core NO lo compilan. Se construye aparte
   (`cargo build -p norte-gui` en su dir, con su propio `rust-toolchain.toml`
   si GPUI exige nightly). Motivo: un GPUI roto/nightly/pesado JAMÁS
   contamina el gate verde del core. Regla dura 7 intacta: el frontend solo
   habla `norte-proto`; el core nunca lo referencia.
3. **Los 4 criterios de validación** (todos, decididos con oscar): listado
   real por el daemon, GUI+TUI simultáneas, norte-theme aplicado, medición
   de viabilidad. Cubren exactamente lo que la spec pide validar.
4. **Read-only.** El spike solo hace `fs.list` — cero mutación, cero
   journal, sin riesgo. La navegación/copy/etc. son del MVP.

## Qué construye

`crates/norte-gui` (crate binario nuevo, EXCLUIDO del workspace):

### Criterio 1 — listado real por el daemon
- Arranca, conecta por `RemoteBackend::connect(socket)` (el MISMO unix socket
  que la TUI; requiere `norte daemon run` corriendo).
- `backend.list(&dir)` (o `list_stream`) de un dir pasado por arg/env.
- Pinta las entradas en una ventana GPUI: nombre (display lossy, `display_name`
  del mismo criterio que la TUI — bytes no-UTF8 no rompen) + indicador de
  tipo (dir/file/symlink).
- Integración proto+GPU end-to-end, NO un hello-world.

### Criterio 2 — GUI+TUI simultáneas
- Con `norte daemon run` + la TUI (`norte-tui --daemon`) + `norte-gui` a la
  vez contra el MISMO socket: ambas ven el mismo dir. Valida el modelo
  multi-conexión (ya probado con TUI+agente en M3) con un frontend gráfico.
- Demostración del criterio de salida de M5 en pequeño (un dir, read-only).

### Criterio 3 — norte-theme aplicado
- Los colores por `Role`/`FileKind` de `norte-theme` (truecolor, la capa GPU
  que MT reservó) pintados en la lista: `Color::resolve(ColorDepth::Truecolor)`
  → color GPUI (`gpui::Hsla`/`Rgba`); `FileColors::style_for(name, kind)` para
  el color por tipo de archivo.
- Prueba que el theming compartido cruza a la GPU sin retrabajo.

### Criterio 4 — medición de viabilidad (el entregable que decide el ADR)
Documento con números duros:
- Arranque: cold start y warm start (ms).
- Tamaño del binario release (MiB).
- Peso de deps: `cargo tree -p norte-gui` — nº de crates transitivos nuevos,
  los pesados (blade/wgpu/skia, si aplica).
- Toolchain: ¿compila con la stable pineada (1.96.1)? ¿exige nightly? Si sí,
  qué fecha/features. MSRV del proyecto es 1.94.
- Estabilidad de la API GPUI: versión consumida (¿crates.io? ¿git pin de
  Zed?), señales de breaking en su historial reciente, cadencia.
- Fricción de build en Linux (plataforma de oscar): libs de sistema
  requeridas, tiempo de compilación en frío.
- Licencias de las deps nuevas: revisión aparte (el crate excluido no pasa
  por el `cargo deny` del workspace; nota manual, y si el MVP lo integra,
  entrada en `deny.toml`).

## Datos reales (verificados)

- `Backend`/`RemoteBackend` (norte-core/src/backend.rs): `connect`,
  `list`/`list_stream` reusables tal cual (la TUI ya los consume).
- `norte-theme`: `Color::resolve(ColorDepth::Truecolor)`, `Role`, `FileKind`,
  `FileColors::style_for(name: &[u8], kind)`, `Theme`/`preset_default`.
- El spike los consume sin tocar el core.

## Errores

- Daemon caído / socket ausente → mensaje claro en la ventana (o stderr +
  ventana de error), JAMÁS panic.
- `fs.list` que falla (NotFound, permisos) → se muestra la categoría, no se
  cuelga.

## Tests

Spike de exploración: la MEDICIÓN es el entregable, no una suite. Sin tests
automatizados formales del código del spike. EXCEPCIÓN: cualquier pieza
reutilizable que sobreviva hacia el MVP (p.ej. el mapeo
`norte_theme::Color → gpui::color`) llevará su test cuando se estabilice en
el hito 2.

## Fuera de alcance (es el MVP, hito 2 — condicionado al go)

Dual-pane; cd/navegación; copy/move/delete y cualquier mutación; keymap;
viewer; i18n Fluent en la GUI; AccessKit; empaquetado/distribución; hot-reload
de tema; modo embebido (el spike va SIEMPRE por daemon, es lo que valida el
criterio de salida).

## Entregable y criterio de salida

- El binario del spike (`norte-gui`) que cumple los criterios 1-3.
- **ADR 0027 (GPUI go/no-go)** con la medición del criterio 4 y la decisión:
  - **go** → confirma §18.3; arranca el plan del MVP (hito 2).
  - **no-go** → documenta el pivote a Tauri con los números que lo justifican
    (y el spike GPUI queda como referencia, no se tira el aprendizaje).

Criterio de salida del spike: con `norte daemon run`, la GUI pinta un dir
real con los colores de norte-theme, a la vez que la TUI contra el mismo
socket, y el ADR 0027 registra la medición y la decisión.
