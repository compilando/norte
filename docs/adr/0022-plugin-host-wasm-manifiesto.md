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

## Addendum P4 (2026-07-16)

**Qué se implementó (M4-P4, ejecución de plugins).** El core ya EJECUTA plugins
de categoría `command` aprobados+activados, cerrando la deuda runtime que P3
dejaba abierta:

- `PluginRegistry::run_command(&rt, id, command, arg) -> Result<String, PluginRunError>`
  (en `norte-core`) valida el consentimiento **fail-closed** (un plugin
  desconocido / sin aprobar / desactivado / sin `plugin.wasm` JAMÁS se ejecuta —
  `Unknown`/`NotApproved`/`Disabled`/`NoBinary`) y solo entonces instancia el
  componente con las capabilities DEL MANIFIESTO y el **sandbox heredado de
  M4-P2** (deadline de época, sin ambient authority, `fs-read` con enforcement
  host-side). El binario es `<dir>/plugin.wasm` por convención (D6).
- Expuesto por wire como `plugin.run_command` (proto 0.14.0) y por CLI
  `norte plugin run <id> <command> [arg]`. El daemon **resuelve bajo el lock**
  (`resolve_runnable`, barato) y **ejecuta en `spawn_blocking`** fuera del lock
  (compilar+instanciar es pesado y síncrono, regla 2). Los errores de runtime se
  **redactan** al cliente (taxonomía gruesa, nunca la ruta absoluta ni el detalle
  interno del trap).
- **Cierre E2E con un componente WASM real** (`crates/norte-core/tests/plugins_run_e2e.rs`):
  la cadena descubrir → (denegar sin aprobar, con el `.wasm` presente) → aprobar
  → activar → ejecutar, compilando `examples-wasm/command-demo` a
  `wasm32-wasip2` y comprobando salida (`echo`→arg, `shout`→MAYÚSCULAS) y el
  `Err` del guest → `Runtime`. SKIP si el target no está instalado.

**Deuda (queda fuera de P4):**

- **Invocación desde el TUI** (palette de comandos de plugin): necesitaría
  exponer la LISTA de comandos de cada plugin por wire — saltado en P4. Hoy la
  ejecución es por CLI / wire directo.
- **Wiring de las otras cuatro interfaces** del world: `previewer` (viewer F3),
  `provider` como scheme VFS, `columns` y `hook` — ninguna cableada aún; solo
  `command` ejecuta.
- **Integración fina con el policy engine M3**: las puertas FS del plugin
  (`fs-read`/`fs-write` scoped) aún NO pasan por un gate por-op del policy engine
  ni se anota en el journal lo que el plugin hace (regla 9 completa pendiente).
- **Sin caché de instancias**: cada `run_command` compila+instancia el
  componente de cero (sin `InstancePre` ni pool). Aceptable para el cierre;
  optimización posterior.
- **Convención `plugin.wasm` fija**: el binario es siempre `<dir>/plugin.wasm`;
  el manifiesto aún no puede nombrar otro artefacto (D6).
- **Issue #69**: la aprobación debería ligarse a un DIGEST de las capabilities
  (re-aprobar si cambian) + dedup de ids duplicados en el catálogo.

## Addendum P5 (2026-07-16)

**Qué se implementó (M4-P5, previewer de plugins en el viewer).** El core cablea
la interfaz `previewer` del world al **viewer F3**, cerrando el segundo consumidor
runtime del host (tras `command` en P4):

- `PluginRegistry::resolve_previewer(mime) -> Option<(id, name, wasm, caps)>`
  elige, **fail-closed**, el primer previewer APROBADO+ACTIVADO cuyo glob de
  mimetypes case `mime` (un previewer no consentido jamás se elige). Barato y
  bajo el lock; el caller lee bytes y ejecuta fuera.
- Expuesto por wire como `plugin.preview` (proto 0.15.0). La detección de
  mimetype es **por extensión** del nombre (`guess_mimetype`, pub(crate)). El
  core **lee los bytes del archivo acotados a 1 MiB** y los pasa al guest (regla
  9: el plugin no toca el FS a pelo; recibe lo que el host le entrega). El output
  del guest se **enmascara en el viewer** con un indicador «via <plugin>».
- **Acople `plugin.preview` ↔ `fs.read`**: `plugin.preview` solo se sirve si
  `fs.read` está abierto — anclado en el handler (leer un archivo para
  previsualizarlo es una lectura; no se abre una puerta nueva por la de atrás).
- Los errores de runtime se **redactan** al cliente (taxonomía gruesa, sin ruta
  ni detalle interno del trap).
- **Cierre E2E con un componente WASM real**
  (`crates/norte-core/tests/plugins_preview_e2e.rs`): descubrir → (denegar sin
  aprobar, con el `.wasm` presente y el mimetype casando) → aprobar → activar →
  resolver `text/plain` → EJECUTAR `examples-wasm/previewer-demo` y comprobar que
  el render lleva la cabecera `[text/plain]` + las 3 primeras líneas (la 4.ª no).
  `application/json` no casa `text/*` → `None`. SKIP si el target no está.

**Deuda (queda fuera de P5):**

- **Mimetype por SNIFFING de contenido**, no solo por extensión: un archivo sin
  extensión (o con extensión mentirosa) no resuelve el previewer correcto.
- **Preferencia/orden si varios previewers casan** el mismo mimetype: hoy gana el
  primero del catálogo; falta política de prioridad y desempate.
- **Previewer en el PANE** (columna de vista rápida), no solo en el viewer F3.
- **Streaming del preview** para archivos grandes: hoy el core lee un bloque
  acotado (1 MiB) y lo pasa entero; sin streaming ni paginación del contenido.
- **Sin cap del output del guest** (**issue #68**): el guest puede devolver un
  string arbitrariamente grande; falta un tope en el render.
- **Las interfaces `provider`, `columns`, `hook` siguen sin wiring**: solo
  `command` (P4) y `previewer` (P5) tienen consumidor runtime.
- **Issue #29** (previewer plugin en el pane con syntax-highlight) queda
  **PARCIALMENTE cubierto**: hay previewer de plugin en el viewer F3, pero no en
  el pane ni con resaltado de sintaxis. El issue se deja ABIERTO anotando el
  avance de P5.
