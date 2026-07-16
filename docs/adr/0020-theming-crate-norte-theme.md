# 0020 — Theming: crate `norte-theme`, roles semánticos y degradación de color

- Estado: accepted
- Fecha: 2026-07-15
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §15 (roadmap — este ADR REORDENA los hitos), §16.2
  (licencias por crate), ADR 0007 (config en capas + hot-reload), ADR 0010
  (frontera core/plugin/config). Milestone de theming (MT), fases T1–T6.

## Contexto y problema

La TUI de M1 es MONOCROMA: solo usa tres modificadores de ratatui
(`BOLD`/`REVERSED`/`DIM`) hardcodeados en `ui.rs`, sin un solo color. La
dirección pide **theming potente** ANTES de M3 (agéntico) y M4 (plugins),
reordenando el roadmap. Preguntas:

1. **Dónde vive** el modelo de tema, dado que la GUI de M5 (GPUI) debe reusar
   los MISMOS temas que la TUI.
2. **Qué modela** (roles semánticos vs estilos sueltos; colores por tipo de
   archivo; presets) y con qué **profundidad de color** (truecolor vs 256/16).
3. Cómo **degrada** en terminales pobres sin verse mal.
4. Cómo encaja la petición de **efectos de GPU** sin contaminar la TUI.

## Decisión

### D1 — Crate `norte-theme` (presentación, Apache-2.0/MIT), sin backend

El modelo vive en un crate NUEVO `norte-theme`, licencia permisiva como las
otras libs de presentación (`norte-encoding`, §16.2). NO depende de `ratatui`
ni de ningún backend de render: expone tipos propios. La TUI mapea
`norte_theme::Color` → `ratatui::Color`; la GUI de M5 hará su propio mapeo. Un
cambio de framework de frontend jamás toca el modelo de tema (misma tesis que
la regla 7: la lógica no vive en el frontend).

Regla 7 (frontends sin lógica de negocio): el theming es PRESENTACIÓN, no
lógica de negocio del core — por eso vive en una lib de presentación
compartida, no en `norte-core` ni en el protocolo. El core y el wire NO saben
de temas.

### D2 — Modelo: `Color` propio + `Style` por `Role` semántico

- **`Color`**: RGB de 24 bits (autoría en `#rrggbb`). Es el formato canónico;
  la degradación es una PROYECCIÓN, no otro tipo de autoría.
- **`ColorDepth`**: `Truecolor | Ansi256 | Ansi16`. `Color::resolve(depth)`
  proyecta a un `ResolvedColor` que el frontend traduce a su color nativo:
  truecolor = RGB tal cual; 256 = índice más cercano del cubo xterm-256;
  16 = ANSI base más cercano (distancia en espacio RGB). Algoritmo en el
  crate, sin dependencias.
- **`Role`**: enum de roles SEMÁNTICOS (selección, borde-foco, borde-sin-foco,
  directorio, symlink, ejecutable, error, aviso, barra de estado, badge de
  nombre hostil…), NO estilos por-widget. Un tema define un `Style`
  (`fg`/`bg` + `bold`/`dim`/`italic`/`underline`/`reverse`) por rol; la TUI
  pide el rol, no el color. Añadir un rol es no-breaking (default heredado).
- **Colores por tipo de archivo** (T4): mapa por `kind` (dir/symlink/exec/
  fifo/socket) y por EXTENSIÓN (bytes, regla 1). Resolución: extensión > kind
  > rol `regular`.

### D3 — Presets embebidos + config

Presets cuidados embebidos como TOML (catppuccin, gruvbox, nord + un default),
sin dependencia externa (son datos). `[ui].theme` (ADR 0007) acepta un NOMBRE
de preset o una RUTA a un `.toml` propio. El hot-reload de config ya recarga y
re-resuelve el tema. Un tema del usuario que no cubre todos los roles HEREDA
del default (jamás un rol sin estilo).

### D4 — Efectos de GPU: capa `[theme.effects]` OPACA, reservada a M5

La petición de «efectos de GPU» se modela como una sección `[theme.effects]`
que el crate parsea a un holder OPACO (validado laxo, tolerante a claves
futuras) y que **la TUI IGNORA sin coste** (un terminal no tiene GPU). La GUI
de M5 (GPUI) la interpretará (gradientes, glow, animaciones). Así el mismo
fichero de tema sirve a ambos frontends y la TUI degrada el efecto a su color
plano. NO se implementa ningún efecto en MT — solo se reserva el espacio en el
modelo para no romper temas cuando M5 los añada.

### D5 — Reorden del roadmap (spec §15)

Orden nuevo tras M2: **MT (theming) → M4 (plugins) → M3 (agéntico) →
M5 (GUI)**. Justificación: theming y plugins son valor para el USUARIO humano
(el producto ya es usable desde M1); lo agéntico (M3) es automatización que
puede esperar. El reorden no cambia NINGÚN criterio de salida de los hitos,
solo su secuencia. Se anota en §15.

## Consecuencias

Positivas: la TUI gana color de verdad por primera vez; el modelo es
GUI-ready (M5 no reimplementa nada); truecolor con degradación honesta cubre
desde kitty hasta un `xterm` de 16 colores; los presets dan buen aspecto
out-of-the-box; el `[effects]` reservado evita un cambio incompatible cuando
llegue la GPU.

Negativas / deuda: un crate más en el workspace; el algoritmo de degradación
a 16 colores es aproximado (aceptable — es el peor terminal); syntax
highlighting del viewer sigue siendo territorio de plugin (`previewer`,
ADR 0010) — el tema define sus colores pero el resaltado en sí es M4; los
efectos de GPU son solo un hueco hasta M5. La detección de profundidad de
color (`COLORTERM`/`TERM`) es heurística (no hay API portable fiable) —
override manual en config como red.
