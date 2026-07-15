# 0022 — Plugin host WASM: runtime, manifiesto, capabilities y los tres niveles

- Estado: accepted
- Fecha: 2026-07-15
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §7 (sistema de plugins), §16.5 (RAR por delegación); ADR
  0010 (frontera core/plugin/config), 0005 (contrato Provider), 0020
  (reorden del roadmap: M4 antes que M3). Hito M4 (plugins), fases P1–P4.
  Kickoff confirmado con oscar (2026-07-15).

## Contexto y problema

M4 abre la extensibilidad de terceros. La spec §7 ya fija el grueso (runtime,
interfaces, permisos); este ADR lo cierra como decisiones de implementación y
añade la dirección de producto de oscar: los plugins deben verse **muy
ordenados** (referencia: la vista de Extensiones de VSCode), al contrario de
Krusader —donde plugins y acciones de usuario quedan borrosos—, con nuestras
categorías y capabilities SIEMPRE visibles.

Preguntas a cerrar: runtime y bindings; formato del manifiesto; modelo de
capabilities y su enforcement; cómo se distinguen plugins de scripts y de
comandos nativos; distribución; y la gobernanza dado que M4 va ANTES del policy
engine (M3).

## Decisión

### D1 — Runtime: wasmtime + Component Model (WIT)

Como manda la spec §7.1: **wasmtime** + Component Model, world `norte:plugin`,
bindings con `wit-bindgen` (host) — los plugins se compilan desde cualquier
lenguaje con toolchain WASM. Alternativas (Lua-only, WASM sin components,
extism) descartadas: el Component Model da interfaces tipadas y aislamiento
real. `wasmtime` es dep GRANDE → entra en M4-P2 (no en el scaffold P1), bajo
`cargo deny`.

### D2 — Cinco interfaces WIT (world `norte:plugin`)

`previewer`, `provider`, `command`, `columns`, `hook` (spec §7.1). El trait
`Provider` de `norte-vfs` se proyecta a la WIT `provider`. Cada plugin declara
su categoría PRIMARIA (para ordenar) y sus contribuciones concretas por
interfaz.

### D3 — Manifiesto `plugin.toml`: identidad + contribuciones + capabilities

Estilo `contributes` de VSCode, pero con NUESTRAS categorías:

```toml
[plugin]
id = "org.norte.syntax-preview"   # reverse-DNS único
name = "Syntax Preview"
publisher = "norte"
version = "0.1.0"
category = "previewer"            # interfaz primaria → ordena la lista

[contributions]
previewer = [{ mimetypes = ["text/*", "application/json"] }]

[capabilities]
fs-read = "scoped"                # none | scoped
fs-write = "none"
# net = { hosts = ["dav.example.com"] }
# ai  = "chat"
exec = "none"                     # SIEMPRE none (spec §7.1); un valor != none se RECHAZA
```

El host parsea el manifiesto y RECHAZA `exec` distinto de `none` (invariante
dura, no negociable). Un permiso no declarado = sin syscall.

### D4 — Capabilities y su enforcement (sandbox real)

`fs-read`/`fs-write` = `none|scoped` (scoped = solo lo que el host abre y pasa,
jamás el FS a pelo — regla dura 9); `net = { hosts=[…] }`; `ai = chat`;
`exec = none`. El host las hace cumplir con **WASI preview 2** (allow-list de
capacidades del componente) + comprobaciones propias en cada llamada al host.
El usuario APRUEBA las capabilities al instalar; hasta entonces el plugin queda
`⚠ sin aprobar` y no se carga.

### D5 — Tres niveles NÍTIDOS (el anti-Krusader)

Todo lo «invocable» pertenece a UN nivel, jamás mezclados:

| Nivel | Qué | Sandbox | Origen |
|-------|-----|---------|--------|
| **Plugins** | WASM de terceros | Sí (WASI + capabilities) | instalados |
| **Scripts** | Lua del usuario | No (permisos del usuario; es config) | config propia |
| **Built-in** | comandos nativos (`pane.copy`, `app.theme`…) | — | el binario |

El Lua corre con los permisos del USUARIO (no sandbox): es config, no software
de terceros, y se documenta con claridad (spec §7.1). El **gestor de
extensiones** (vista TUI, M4-P3) pinta el catálogo agrupado por nivel y dentro
por categoría, con **capability badges** siempre a la vista y el aviso `⚠ sin
aprobar`: el usuario ve QUÉ puede hacer cada cosa sin abrir nada.

### D6 — Distribución: `.wasm` locales primero

El host descubre plugins en `~/.config/norte/plugins/<id>/` (`plugin.toml` +
`.wasm`). Un registro/índice remoto queda para después (no bloquea M4).

### D7 — Gobernanza: sandbox de capabilities en M4; policy engine en M3

El roadmap pone M4 antes que M3 (policy engine + journal/undo, ADR 0020). La
gobernanza de M4 es el **sandbox por capabilities** (manifiesto + WASI + checks
del host): suficiente para aislar terceros. La integración con el POLICY ENGINE
(reglas ask/allow/deny por scope, journal, undo de sesión) se SUPERPONE en M3 —
la regla 9 («todo pasa por core → policy engine») se cumple parcialmente ahora
(el host media toda syscall) y del todo en M3. Se documenta la costura.

## Consecuencias

Positivas: extensibilidad seria y aislada; un modelo de plugins ordenado y
legible (categorías + capabilities visibles) que evita el blur de Krusader;
interfaces tipadas por el Component Model; el trait `Provider` ya existente se
reusa como WIT. El scaffold P1 (manifiesto/capabilities/catálogo) es Rust puro,
testeable HOY, sin arrastrar wasmtime.

Negativas / deuda: `wasmtime` es dep grande (entra en P2, `cargo deny`
vigilante); compilar plugins exige toolchain `wasm32-wasip2` (los ejemplos lo
documentan); la gobernanza queda a medias hasta M3 (sandbox sí, policy engine
no); el gestor de extensiones y el catálogo añaden superficie de UI. El SDK
permisivo (Apache/MIT) para AUTORES de plugins (bindings de guest) es un crate
aparte, futuro; `norte-plugin-host` es subsistema del core (AGPL).
