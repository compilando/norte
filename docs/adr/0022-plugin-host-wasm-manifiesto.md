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

## Addendum P2 (2026-07-16)

**Qué se implementó (M4-P2, T1–T7).** El runtime `norte-plugin-host` ejecuta
componentes WASM reales sobre **wasmtime 46** (Component Model). El sandbox
arranca con un WASI **vacío por defecto** (sin FS, red ni entorno): un plugin
no ve nada que no se le conceda explícitamente. Dos de las cinco interfaces del
plan están **end-to-end**: `previewer::render` y `command::run` (más la puerta
de host `host-log`: `log` + `read-scoped`). El **enforcement de `fs-read` vive
en el HOST** (ADR 0022 D4): el guest siempre puede *llamar* a `read-scoped`,
pero si su `Capabilities` no declaró `fs-read` el host devuelve `Err` sin tocar
recurso alguno; con `fs-read=scoped` el host resuelve el token contra los
recursos que él mismo sembró. Se añaden **guests de ejemplo** (`previewer-demo`,
`command-demo`) fuera del workspace (compilan a `wasm32-wasip2` con lockfile y
perfil propios) y un **helper de build con SKIP**: si el target `wasm32-wasip2`
no está instalado los tests de componente se saltan (no fallan); si está pero el
guest no compila, es fallo real. `just ci` no compila los guests salvo cuando el
test los pide y el target está presente.

**Deuda (queda fuera de P2):**

- **Las 3 interfaces restantes** (`provider`, `columns`, `hook`) → **P2b**.
- **Worlds por-categoría.** Hoy el world `norte-plugin` exige AMBAS interfaces
  (`previewer` + `command`); un guest de una sola categoría implementa la otra
  como "no-soportada". Worlds por-categoría (para no exigir implementar
  interfaces ajenas) → P2b.
- **Límite de memoria/CPU por store.** Hay `TODO M4-P2b` en `runtime.rs`
  (`Store::limiter` + fuel/epoch): un plugin hostil puede hoy consumir memoria o
  colgarse sin tope. → P2b.
- **Integración con el policy engine M3.** El sandbox AÍSLA (media toda
  syscall); el gating fino `ask`/`allow`/`deny` por scope + journal/undo (ADR
  0020) se **superpone después**, cuando el host de plugins se cablee al daemon
  M3 (regla 9 completa). Ver D7.
- **Gestor de extensiones (descubrimiento/instalación/UI)** → **P3**.

## Addendum P3 (2026-07-16)

**Qué se implementó (M4-P3, gestor de extensiones).** El core expone el
CATÁLOGO local y su ESTADO de gobierno por el protocolo `plugin.*` (proto
0.13.0), cumpliendo la regla 7 (los frontends no llevan lógica):

- `PluginRegistry` (en `norte-core`) descubre `config_dir/plugins/<id>/plugin.toml`
  vía `norte_plugin_host::Catalog`, fusiona el estado del usuario y lo sirve como
  `plugin.list` → `PluginListResult { plugins, errors }`. El id reverse-DNS se
  valida por charset al parsear el manifiesto (D3). Los manifiestos ROTOS no
  tumban el catálogo: van a `errors`, y se reportan por **basename**, nunca por
  ruta absoluta (no filtra el `~/.config` del usuario a un agente que llame
  `plugin.list`). Las `name`/`publisher` del modal se enmascaran en la UI (el
  autor es texto no confiable).
- **Aprobar y activar son actos HUMANOS**: `plugin.set_approval` /
  `plugin.set_enabled` SOLO los acepta una conexión no-agente (`Actor::User`);
  un agente recibe `INVALID_REQUEST`. Aprobar = consentir las capabilities
  declaradas (decisión de seguridad, D4); activar = tenerlo encendido.
- **Persistencia ATÓMICA** en `config_dir/plugins-state.toml` (write a temporal
  en el mismo dir + `rename`), con el id-con-puntos entrecomillado bajo `[plugins]`
  (round-trip íntegro). El daemon separa la mutación bajo el lock de la
  persistencia en `spawn_blocking` (regla 2).
- **TUI overlay que solo PINTA**: el gestor de extensiones muestra catálogo,
  categoría, capability badges y el aviso `⚠ sin aprobar`; toda decisión viaja
  por el wire al core.

**Deuda (queda fuera de P3):**

- El gestor **MUESTRA y GOBIERNA** el estado (aprobado/activado) pero aún **NO
  carga ni ejecuta** plugins: el wiring runtime (`norte-plugin-host`) ↔ core bajo
  el policy engine M3 (regla 9 completa) es posterior. Ver D7 y Addendum P2.
- **Sin instalar/desinstalar desde la UI**: la siembra es manual en
  `config_dir/plugins/`. Tampoco hay **registro/índice remoto** (ver D6).
- **Corrupción de `plugins-state.toml`**: el daemon degrada a catálogo vacío y lo
  avisa **solo por log** (`tracing::warn`), no a la UI — un fichero roto no debe
  impedir arrancar, pero el humano no lo ve en pantalla.
- **Sin sincronización daemon ↔ frontend-embebido** sobre el mismo `config_dir`:
  dos escritores concurrentes del `plugins-state.toml` no se coordinan (el
  esquema single-writer del daemon aún no cubre el modo embebido).
