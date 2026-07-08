# NORTE — Especificación fundacional (v0.2)

> Codename provisional: **norte** (homenaje a Norton Commander; palabra española; corto, minúscula, CLI-friendly). Sustituible.

**Elevator pitch.** Un file manager ortodoxo de nueva generación con arquitectura *headless-core*: un daemon en Rust que expone un protocolo estable y un VFS universal, sobre el que se montan frontends intercambiables (TUI, GUI, CLI) y sobre el que los agentes de IA operan de forma gobernada (MCP, aprobaciones, journal, auditoría). "El Zed de los file managers".

---

## 1. Principios de diseño (no negociables)

1. **Core headless primero.** Ningún frontend tiene lógica de negocio. Si una operación no se puede hacer vía protocolo, no existe.
2. **Async en la base.** Toda I/O es asíncrona y cancelable. La UI nunca se bloquea. Toda operación de larga duración es una *Task* con progreso, prioridad y cancelación.
3. **Los bytes son la verdad.** Los nombres de archivo NO son UTF-8. La representación interna es bytes/OsString; UTF-8 es solo una vista para display. Ningún path se corrompe jamás por un roundtrip.
4. **Toda mutación es reversible o explícitamente irreversible.** Journal transaccional, papelera propia multiplataforma, undo. Las operaciones irreversibles (shred, rm en remoto sin trash) se marcan y requieren confirmación reforzada.
5. **La IA es un ciudadano, no un dueño.** Los agentes operan a través del mismo protocolo que los humanos, sujetos a un policy engine (allow/ask/deny), con audit trail completo. Nunca acceso directo al filesystem por debajo del core.
6. **Ultra-configurable, con defaults sanos.** Config en capas, presets de keybindings (orthodox/vim/CUA), todo remapeable, hot-reload.
7. **Testeable por construcción.** El VFS es un trait; existe un provider en memoria determinista para tests. Cobertura y property-based testing como gate de CI, no como aspiración.
8. **Compat es una feature.** Windows, macOS y Linux son first-class desde el commit 1 (CI matrix en los tres). Los edge cases de cada OS (paths largos de Windows, NFD de macOS, permisos POSIX) tienen tests dedicados.

**Anti-objetivos (v1):** sync entre dispositivos, base de datos distribuida (la tumba de Spacedrive), cloud storage propio, mobile.

---

## 2. Arquitectura general

```
┌────────────┐  ┌────────────┐  ┌────────────┐  ┌──────────────┐
│ norte-tui  │  │ norte-gui  │  │ norte-cli  │  │ Agentes IA   │
│ (ratatui)  │  │ (fase 2)   │  │ (scripting)│  │ (via MCP)    │
└─────┬──────┘  └─────┬──────┘  └─────┬──────┘  └──────┬───────┘
      │   Protocolo norte (JSON-RPC 2.0)  │      MCP (stdio/HTTP)
      └───────────────┴───────────────────┴─────────────┘
                        │
                ┌───────▼────────┐
                │   norte-core    │  daemon: sesiones, tasks,
                │                 │  policy engine, journal,
                │  ┌───────────┐  │  config, plugin host
                │  │ Scheduler │  │
                │  └───────────┘  │
                └───────┬────────┘
        ┌───────────────┼────────────────┐
  ┌─────▼─────┐   ┌─────▼─────┐   ┌──────▼──────┐
  │ norte-vfs │   │norte-index│   │  norte-ai   │
  │ providers │   │ (SQLite,  │   │ (providers  │
  │ local/sftp│   │ búsqueda, │   │ LLM/embed)  │
  │ s3/archive│   │ embeddings│   │             │
  └───────────┘   └───────────┘   └─────────────┘
```

- **Modo embebido y modo daemon.** El core puede correr in-process (el TUI lo linka como lib para arranque instantáneo, caso Yazi) o como daemon compartido (varios frontends contra la misma sesión, caso agentes + GUI simultáneos). Misma API en ambos: el protocolo se abstrae sobre un transporte `InProcess | UnixSocket | NamedPipe | Tcp(loopback)`.
- **Sesiones.** Un cliente abre una sesión con capacidades negociadas (versión de protocolo, features). El estado de panes/tabs vive en el core → un agente puede "ver" los mismos panes que el humano.

---

## 3. Separación en proyectos (cargo workspace)

Monorepo con workspace Cargo. Cada crate con responsabilidad única, API pública mínima y tests propios.

| Crate | Responsabilidad | Deps clave |
|---|---|---|
| `norte-proto` | Tipos del protocolo (serde), versionado, sin lógica | serde |
| `norte-vfs` | Trait `Provider`, tipos VFS (`VPath`, `Entry`, `Capabilities`) | tokio, bytes |
| `norte-vfs-local` | Provider filesystem local (por OS) | tokio, cfg(windows/unix) |
| `norte-vfs-sftp` | SFTP/SSH | russh |
| `norte-vfs-object` | S3/GCS/Azure (una crate, backends feature-gated) | opendal |
| `norte-vfs-archive` | ZIP/TAR/7z/RAR(read) como directorios virtuales | zip, tar, sevenz-rust |
| `norte-index` | Metadatos, búsqueda, tags, embeddings (SQLite) | rusqlite, tantivy? |
| `norte-plugin-host` | Runtime WASM (wasmtime), WIT, permisos | wasmtime |
| `norte-ai` | Trait `AiProvider` + implementaciones | reqwest, eventsource |
| `norte-mcp` | Servidor MCP (core como tool provider) y cliente MCP | rmcp |
| `norte-core` | Daemon: sesiones, scheduler, policy, journal, config | todo lo anterior |
| `norte-tui` | Frontend terminal | ratatui, crossterm |
| `norte-cli` | Cliente headless (`norte cp`, `norte ls --json`) | clap |
| `norte-testkit` | Provider en memoria, fixtures, estrategias proptest | proptest |
| `norte-gui` | Fase 2 (GPUI o Tauri; decisión diferida) | — |

Reglas de dependencia: los frontends solo dependen de `norte-proto` (+ core en modo embebido). Los providers VFS no se conocen entre sí. `norte-ai` no conoce el VFS (recibe contenido, no rutas). Enforcement con `cargo-deny` + lint de dependencias en CI.

---

## 4. Async en la base

- **Runtime:** tokio multi-thread. FS local vía `spawn_blocking` con pool dimensionado (o `tokio-uring` en Linux tras benchmark, detrás de feature flag).
- **Modelo de Task.** Toda operación no instantánea (>10 ms estimados) se materializa como `Task`:

```rust
struct Task {
  id: TaskId,
  kind: TaskKind,          // Copy, Move, Delete, Search, Hash, Index, AiJob…
  state: Queued|Running|Paused|Done|Failed|Cancelled,
  progress: { bytes_done, bytes_total, items_done, items_total, current_item },
  priority: Low|Normal|High|UserInteractive,
  cancel: CancellationToken,
  parent: Option<TaskId>,  // árboles de tareas (copy dir → subtareas)
}
```

- **Scheduler:** colas por prioridad, límites de concurrencia por provider (p.ej. SFTP max 4 streams; disco local max N según media detectada), pausa/reanudación, reordenación por el usuario. Conflictos (dos escrituras al mismo destino) se serializan por lock de ruta.
- **Cancelación cooperativa:** todo provider debe chequear el token en su inner loop; test obligatorio por provider: "cancelar copia de archivo grande deja el destino limpio (sin parciales) o marcado `.norte-partial`".
- **Backpressure:** streams de lectura/escritura con buffers acotados; los eventos de progreso al frontend se coalescen (máx 30 Hz por task).
- **Eventos:** el core emite notificaciones (protocolo) para cambios de FS (watchers: `notify` crate, polling en remotos), progreso de tasks, cambios de config. Los frontends son puramente reactivos.

---

## 5. VFS: el contrato central

```rust
#[async_trait]
trait Provider: Send + Sync {
  fn scheme(&self) -> &str;                    // "file", "sftp", "s3", "zip"…
  fn capabilities(&self) -> Capabilities;      // ver abajo
  async fn stat(&self, p: &VPath) -> Result<Entry>;
  async fn list(&self, p: &VPath, opts: ListOpts) -> Result<EntryStream>;
  async fn read(&self, p: &VPath, range: Option<Range>) -> Result<ByteStream>;
  async fn write(&self, p: &VPath, opts: WriteOpts) -> Result<ByteSink>;
  async fn mkdir(&self, p: &VPath) -> Result<()>;
  async fn remove(&self, p: &VPath, opts: RemoveOpts) -> Result<()>;
  async fn rename(&self, from: &VPath, to: &VPath) -> Result<()>;      // mismo provider
  async fn copy_native(&self, from: &VPath, to: &VPath) -> Option<Result<()>>; // server-side copy si existe (S3, SFTP ext, reflink/clonefile local)
  async fn watch(&self, p: &VPath) -> Result<EventStream>;             // opcional por capability
}
```

- **`VPath` = URI + bytes.** `scheme://authority/<segmentos en bytes>`. Los segmentos se almacenan como bytes crudos; nunca se fuerza UTF-8. Display siempre lossy y marcado (`�` + tooltip con hex).
- **`Capabilities`** (bitflags): `WATCH, RENAME_ATOMIC, SERVER_COPY, TRASH, SYMLINKS, HARDLINKS, PERMISSIONS_POSIX, XATTR, ADS(NTFS), CASE_SENSITIVE, CASE_PRESERVING, MAX_PATH(n), RANDOM_WRITE, APPEND…` Las operaciones compuestas del core (copy entre providers, move cross-provider = copy+verify+delete) consultan capabilities para elegir estrategia y para pintar la UI (p.ej. ocultar "permisos" en S3).
- **Copy engine (en core, no en providers):** streaming con verificación opcional (hash), preservación de metadatos según capabilities (mtime, permisos, xattr, ADS), política de colisiones (ask/overwrite/skip/rename-auto/newer), reintentos con backoff en remotos, resume de transferencias interrumpidas (`.norte-partial` + offset).
- **Archivos como directorios:** `zip://<vpath-del-zip>!/ruta/interna`. Lectura v1; escritura (add/delete en ZIP) v1.1. Anidamiento soportado (zip dentro de tar) con límite de profundidad configurable.
- **Papelera universal:** trash nativo donde exista (freedesktop, Recycle Bin, macOS); en providers sin trash, papelera lógica propia (`.norte-trash/` opcional por conexión) o degradación explícita a borrado permanente con aviso.

---

## 6. Codificación de archivos y juegos de caracteres

Dos problemas distintos, tratados por separado. Esta sección es crítica: aquí es donde los FM mediocres corrompen datos.

### 6.1 Nombres de archivo

| OS/Contexto | Realidad | Política norte |
|---|---|---|
| Linux/Unix | bytes arbitrarios (salvo `/` y NUL), sin encoding garantizado | Almacenar bytes tal cual. Decodificar para display con UTF-8 → lossy si falla, badge "nombre no-UTF8" + acción "reparar nombre" (transcodificar desde encoding elegido/detectado) |
| Windows | UTF-16 con posibles *unpaired surrogates* | `OsString`/WTF-8 internamente; roundtrip garantizado; display lossy |
| macOS | UTF-8 normalizado NFD por HFS+/APFS | Conservar la forma original en bytes; **comparaciones y búsqueda siempre normalizando a NFC** |
| Windows paths largos | límite 260 salvo prefijo `\\?\` | Prefijo `\\?\` automático y transparente; tests con paths >260 |
| Case | NTFS/APFS case-insensitive-preserving, ext4 sensitive | Detección por capability y por sondeo; las colisiones de copia se evalúan según el FS *destino* |
| ZIP entries | cp437 histórico vs flag UTF-8 (bit 11) vs encodings locales (CP866, Shift-JIS, GBK…) | Respetar flag UTF-8; si no, detección (chardetng) con override manual por-archivo ("reinterpretar nombres como…") — la killer feature de TC que casi nadie clona bien |

Normalización Unicode: opción global `unicode_compare = nfc|nfd|none` (default nfc) para ordenación, búsqueda y detección de duplicados; nunca se renombra en disco sin acción explícita del usuario.

### 6.2 Contenido (viewer/editor/preview)

- **Detección:** BOM primero; después `chardetng` sobre muestra inicial (64 KiB) + heurísticas (binario si NUL density > umbral). Resultado expuesto en la UI y siempre corregible a mano (menú "recargar como… UTF-8 / Latin-1 / Windows-1252 / Shift-JIS / …", lista completa de `encoding_rs`, que cubre todo el universo WHATWG).
- **Transcodificación:** vía `encoding_rs` en streaming (archivos grandes sin cargar en RAM). Operación "convertir encoding" como Task, con detección de pérdida (caracteres no mapeables → abortar o sustituir, a elección).
- **EOL:** detección LF/CRLF/CR/mixto; visible en status bar; conversión como operación explícita.
- **Preview de texto:** siempre a través del detector; jamás asumir UTF-8. Hexview integrado como fallback universal.
- **Tests:** corpus de fixtures con archivos reales en UTF-8/16LE/16BE (con y sin BOM), Latin-1, Windows-1252, Shift-JIS, GB18030, KOI8-R, más nombres de archivo hostiles (bytes inválidos, NFD, surrogates, emoji, RTL, espacios finales, puntos finales en Windows). `norte-testkit` los provee a todos los crates.

---

## 7. Sistema de plugins

Dos niveles, deliberadamente:

### 7.1 Plugins WASM (extensión seria, sandboxed)
- **Runtime:** wasmtime + Component Model (WIT). Los plugins se compilan desde cualquier lenguaje con toolchain WASM (Rust, Go, AssemblyScript, Python via componentize-py).
- **Interfaces WIT expuestas (world `norte:plugin`):**
  - `previewer` — genera preview (texto estructurado, imagen, tabla) para mimetypes declarados.
  - `provider` — ¡providers VFS de terceros como plugins! (p.ej. WebDAV, Google Drive, un ERP interno). Misma interfaz del §5 vía WIT.
  - `command` — comandos invocables desde palette/keybinding, con acceso a la API de sesión (panes, selección, tasks).
  - `columns` — columnas custom en el listado (estilo TC content plugins: EXIF, duración de vídeo, git status).
  - `hook` — before/after de operaciones (p.ej. escaneo antivirus pre-copy, DLP).
- **Permisos por manifiesto** (`plugin.toml`): declaración de capabilities (`fs-read:scoped`, `fs-write:scoped`, `net:hosts=[…]`, `ai:chat`, `exec:none`). El usuario aprueba en la instalación; el host WASM las hace cumplir (WASI preview2 + capabilities propias). Sin permiso declarado no hay syscall: sandbox real, no promesa.
- **Distribución:** registro git-based (índice tipo crates.io minimal) + instalación desde archivo/URL. Firmado con sigstore (fase 1.1).

### 7.2 Scripting Lua (glue ligero, config viva)
- `mlua` embebido para: keybindings programáticos, automatizaciones de usuario, linemode/statusbar custom. API síncrona de alto nivel sobre el protocolo (como Yazi, que ha validado el modelo).
- El scripting Lua corre con los permisos del usuario (no sandboxed); es config, no software de terceros. La distinción se documenta con claridad.

### 7.3 Integración externa clásica
- Openers/tools externos declarativos por mimetype+OS (como Yazi/TC): `openers.toml`. Variables `%f %F %d` etc. Esto no es un plugin, es config.

---

## 8. Keybindings

- **Modelo:** mapa `(contexto, secuencia) → acción`. Secuencias multi-tecla (chords) estilo vim/emacs: `g g`, `space c e`. Contextos jerárquicos: `global > pane > dialog > preview > editor > task-manager`; resolución del más específico al más general, determinista.
- **Acciones = comandos del protocolo.** Todo keybinding invoca un comando nombrado (`pane.copy`, `sel.invert`, `ai.rename-batch`) con args opcionales. Los comandos de plugins se mapean igual (`plugin:git.stage`). Consecuencia: paridad total palette ↔ teclado ↔ scripting ↔ agente.
- **Presets de fábrica:** `orthodox` (F3 view, F5 copy, F6 move, Tab cambia pane — sagrado), `vim` (hjkl, modos visual/normal), `cua` (Ctrl+C/V, flechas). Se elige en el onboarding; se mezclan por capas.
- **Config:** `keymap.toml` con `prepend_keymap`/`append_keymap` (modelo Yazi, probado) sobre el preset. Hot-reload. Detección de conflictos con warning (comando `norte doctor keymap`).
- **Discoverability:** which-key overlay tras 500 ms en secuencia incompleta; palette (Ctrl+P) con fuzzy search de todos los comandos mostrando su binding actual; cheatsheet exportable.
- **Internacionalización de teclado:** capturar por keycode físico con fallback a layout lógico (config `keyboard.capture = logical|physical`) — los teclados ES/DE/FR agradecen no heredar los dolores de vim con `[`.

---

## 9. Providers de IA

- **Trait único:**

```rust
#[async_trait]
trait AiProvider {
  fn id(&self) -> &str;                       // "anthropic", "openai", "vertex", "ollama", "openai-compat"
  fn models(&self) -> Vec<ModelInfo>;
  async fn chat(&self, req: ChatRequest) -> Result<ChatStream>;   // streaming SSE
  async fn embed(&self, req: EmbedRequest) -> Result<Vec<Embedding>>;  // opcional
  fn capabilities(&self) -> AiCaps;           // tools, vision, json_mode, embed
}
```

- **Implementaciones v1:** Anthropic, OpenAI, Google (Gemini API + Vertex con ADC — tu caso GCP corporativo), Ollama/llama.cpp local, y genérico OpenAI-compatible (cubre vLLM, LM Studio, Mistral, etc.).
- **Credenciales:** keyring del OS (Keychain/Credential Manager/Secret Service); nunca en config plano; soporte de env vars y de ADC/instance metadata para entornos corporativos.
- **Routing por función:** config asigna modelo a cada caso de uso (`ai.rename.model`, `ai.summarize.model`, `ai.embed.model`) — barato/local para lo masivo, potente para lo puntual.
- **Privacidad por diseño:** IA 100 % opt-in; indicador visible de "qué se envió" (payload inspector); reglas de exclusión (`ai.deny_paths`, glob) que el core hace cumplir antes de que ningún byte salga; modo `local-only` que deshabilita providers remotos globalmente.
- **Funciones IA v1 (todas como comandos normales, mapeables):** rename batch por descripción natural (con preview diff obligatorio), resumen/explicación de archivo en preview, clasificación/organización sugerida (plan → aprobación → tasks), búsqueda semántica sobre `norte-index` (embeddings locales por defecto: fastembed/ONNX; remotos opcionales).

---

## 10. Integración agéntica (el diferencial)

- **norte-mcp (server).** El core expone un servidor MCP (stdio y streamable HTTP) con tools: `list_dir, stat, read_file, write_file, copy, move, delete, mkdir, search, archive_extract, task_status, request_scope…` Cualquier agente (Claude Code, Codex CLI, agentes custom) gestiona archivos *a través de norte*, no contra el FS desnudo.
- **Scopes.** Un agente se conecta y solicita un scope: conjunto de rutas (globs) + operaciones permitidas + TTL. El humano lo concede desde el frontend (o por policy pre-aprobada). Fuera de scope, el core deniega — no es prompt engineering, es enforcement.
- **Policy engine.** Reglas declarativas (TOML/JSON) evaluadas por operación: `allow | ask | deny`, con condiciones (ruta, tamaño, extensión, provider, agente, hora). Ejemplos: `write dentro de ~/proyectos/x → allow`, `delete recursivo → ask siempre`, `cualquier op en sftp://prod → deny para agentes`. Modo `ask` empuja una aprobación interactiva al frontend con diff/preview de la operación.
- **Journal transaccional + undo.** Toda mutación (humana o agéntica) se registra en un journal (SQLite, WAL): op, origen (usuario/agente/plugin), antes/después, hash. Deshacer por operación o por *sesión de agente completa* ("revertir todo lo que hizo el agente X desde las 10:31"). Los borrados agénticos van SIEMPRE a trash/staging, nunca directos.
- **Audit trail exportable.** Hash-chain sobre el journal (integridad verificable), export CSV/JSONL. Este es el ángulo enterprise/compliance: visibilidad y gobernanza de agentes sobre filesystem — nadie lo ofrece hoy con enforcement real.
- **norte como cliente MCP (dirección inversa).** El chat/asistente embebido del frontend puede consumir MCP servers externos (p.ej. un MCP de Jira) — fase 1.1, la prioridad es ser servidor.
- **Modo "copilot de sesión".** El asistente ve el estado de sesión (panes, selección, tasks) vía el mismo protocolo y propone comandos que se ejecutan tras aprobación — dogfooding de la tesis "el agente es un cliente más".

---

## 11. Protocolo norte

- **Base:** JSON-RPC 2.0 sobre transporte pluggable (in-process channel, UDS, named pipe, TCP loopback opcional). MessagePack como encoding alternativo negociable (perf en listados enormes).
- **Handshake:** `initialize(client_info, protocol_version, requested_caps) → server_caps`. Versionado semántico del protocolo; el core soporta N y N-1.
- **Familias de métodos:** `session.*` (panes, tabs, selección, cwd), `fs.*` (mapea al VFS), `task.*` (list, cancel, pause, reprioritize), `config.*`, `plugin.*`, `ai.*`, `policy.*` (aprobaciones pendientes), `index.*` (search, tags).
- **Notificaciones (server→client):** `fs.changed`, `task.progress` (coalescido), `policy.approval_required`, `config.reloaded`, `session.updated`.
- **Listados grandes:** paginación por cursor + streaming incremental (el TUI pinta las primeras 100 entradas en <16 ms aunque el dir tenga 500k).
- **Especificación como artefacto:** el protocolo se define en un IDL propio mínimo (o directamente los tipos serde de `norte-proto` + JSON Schema generado) publicado y versionado — terceros pueden escribir frontends sin leer el código del core.

---

## 12. Testing (full, en serio)

- **Unit tests** en cada crate, obligatorios para toda lógica de decisión (estrategias de copy, resolución de keymaps, policy engine, detección de encoding). Objetivo: ≥85 % líneas en crates de lógica (`core`, `vfs`, `proto`), medido con `cargo-llvm-cov`, gate en CI.
- **Property-based (proptest):** roundtrips de `VPath` (bytes arbitrarios → serialize → parse → idénticos), normalización Unicode, resolución de keybindings (ninguna secuencia ambigua), planificador de colisiones de copia. `norte-testkit` publica las estrategias (`arb_hostile_filename()`, `arb_vpath()`).
- **Provider en memoria (`MemProvider`):** FS simulado determinista con inyección de fallos (latencia, EIO en byte N, desconexión) para testear el copy engine, cancelación y resume sin tocar disco.
- **Integration tests:** contra FS real en tmpdir (los tres OS, CI matrix GitHub Actions), y contra servicios reales en contenedor vía testcontainers: `sftp` (openssh), `s3` (MinIO). Casos obligatorios: cancelación limpia, resume, colisiones en FS case-insensitive, paths >260 en Windows, nombres NFD en macOS.
- **Golden tests del protocolo:** fixtures request/response versionadas; cualquier cambio de wire format rompe un test y exige bump de versión.
- **Fuzzing (`cargo-fuzz`):** parsers de entrada no confiable — nombres de entradas ZIP, detección de encoding, config TOML, mensajes JSON-RPC.
- **Tests de plugins:** harness que carga un plugin WASM de referencia y verifica el enforcement de permisos (un plugin sin `net` que intenta abrir socket → trap, test rojo si no).
- **Snapshot tests del TUI (`insta`):** render de pantallas clave a texto, diffs revisables.
- **Benchmarks (`criterion`) + regresión:** listar 100k entradas, copy 10 GiB local, cold start. Presupuestos: cold start TUI <50 ms, listado 100k <200 ms hasta primer render.

---

## 13. Configuración

- **Capas (menor a mayor precedencia):** defaults compilados → `/etc/norte` (sistema) → `~/.config/norte` (usuario) → `.norte/` (por-directorio/proyecto, opt-in) → flags CLI.
- **Formato TOML**, dividido: `norte.toml` (general), `keymap.toml`, `theme.toml`, `openers.toml`, `ai.toml`, `policy.toml`, `connections.toml` (remotos; secretos en keyring, aquí solo referencias).
- **Hot-reload** con watcher + notificación a clientes; validación con JSON Schema publicado (autocompletado en editores gratis).
- **`norte doctor`:** diagnóstico de config, conflictos de keymap, permisos de plugins, conectividad de providers.

---

## 14. Seguridad

- Secretos solo en keyring; memoria con `zeroize` donde aplique.
- Plugins WASM: sandbox por capabilities (§7); sin `exec` jamás desde plugin (solo openers declarativos de usuario).
- Agentes: enforcement por scope+policy en el core (§10); rate limits por sesión de agente.
- Supply chain: `cargo-deny` (licencias+advisories), `cargo-audit` en CI, lockfile estricto, releases firmadas, SBOM (CycloneDX) por release.
- Threat model documentado en `SECURITY.md` desde v0.1 (incluye: plugin malicioso, agente prompt-injected, servidor SFTP hostil con nombres trampa `../../`, archivo ZIP bomb — límites de descompresión).

---

## 15. Roadmap por hitos

| Hito | Contenido | Criterio de salida |
|---|---|---|
| **M0 — esqueleto** | workspace, `norte-proto`, `norte-vfs` (trait+Mem+Local), scheduler mínimo, CI 3 OS con coverage gate | copy/move/delete local con progreso y cancelación, testeado en 3 OS |
| **M1 — TUI usable** | ratatui dual-pane, keymap engine + presets, config en capas, viewer con detección de encoding, trash | "yo lo uso a diario en vez de Yazi/mc" |
| **M2 — remotos+archivos** | sftp, archive (zip/tar read), copy engine cross-provider con resume, object storage | copiar de sftp a zip local vía S3 sin sorpresas |
| **M3 — agéntico** | norte-mcp server, scopes, policy engine, journal+undo, audit export | Claude Code gestiona un directorio real bajo policy `ask`, con undo de sesión completa |
| **M4 — plugins+IA** | plugin host WASM (previewer+command), Lua scripting, norte-ai (Anthropic/OpenAI/Ollama), rename batch IA, búsqueda semántica | tercero publica un plugin sin tocar el core |
| **M5 — GUI** | decisión GPUI vs Tauri con spike medido; primer frontend gráfico contra el mismo daemon | GUI y TUI sobre la misma sesión simultáneamente |

---

## 16. Decisiones resueltas (v0.2)

1. **Nombre:** pendiente de elección final; candidatos evaluados en Anexo A. El codename de trabajo sigue siendo `norte` hasta decisión.
2. **Licencia (modelo Zed):** `norte-proto`, `norte-vfs*`, `norte-testkit` y el SDK de plugins → **Apache-2.0/MIT dual** (maximiza ecosistema: cualquiera puede escribir frontends, providers y plugins sin fricción legal). `norte-core` y frontends oficiales → **AGPL-3.0** con **CLA** que reserva a la entidad titular la posibilidad de licenciamiento comercial (open-core: features enterprise futuras — SSO, policy centralizada, audit remoto — como módulos propietarios sobre el core AGPL). Archivo `LICENSE-*` por crate desde el commit 1; el CLA vía CLA-assistant.
3. **GUI: GPUI.** Asumimos API inestable a cambio de rendimiento y coherencia con la tesis "Zed de los file managers". Mitigación: el frontend solo habla `norte-proto`, así que un cambio de framework nunca toca el core; spike de validación al inicio de M5 igualmente (presupuesto: 2 semanas).
4. **Índice: SQLite + FTS5 en v1.** Un solo motor de almacenamiento (journal, index, tags, embeddings vía sqlite-vec) simplifica ops, backup y tests. `tantivy` queda como upgrade path documentado en ADR si FTS5 se queda corto en corpus >1M archivos.
5. **RAR: solo lectura, para siempre, y vía delegación.** Nada de linkar unrar (licencia vírica no-libre): extracción delegada a binario externo (`unrar`/`7z`) si está presente, detectado en runtime, con degradación limpia ("instala X para soporte RAR"). Cero contaminación de licencia en el árbol.
6. **Telemetría: ninguna, ni opt-in.** Declarado en README y web. Los diagnósticos son locales (`norte doctor --report` genera un archivo que el usuario adjunta manualmente si quiere).

---

## 17. Gaps detectados en la revisión (incorporados al alcance)

Funcionalidad que el spec v0.1 no cubría y que un commander serio no puede omitir:

**17.1 Búsqueda (comando, no solo índice).** Dos modos: (a) *live search* sobre VFS — nombre por glob/regex/fuzzy y contenido tipo grep, streaming de resultados como Task cancelable, **consciente de encodings** (busca "año" en archivos Latin-1 y UTF-8 por igual, transcodificando la aguja, no el pajar); (b) búsqueda indexada (FTS5 + semántica) sobre `norte-index`. Resultados como "pane virtual" operable (seleccionar y copiar/borrar desde resultados, estilo TC).

**17.2 Comparación y sincronización de directorios.** Feature sagrada del género: diff de dos panes (por nombre/tamaño/mtime/hash), vista de diferencias, sync unidireccional/bidireccional con preview del plan como lista de operaciones aprobables (reutiliza el mismo mecanismo de "plan → aprobación → tasks" del agéntico). Comparación de archivos delegable a herramienta externa o viewer diff propio (v1.1).

**17.3 Multi-rename (no-IA).** Herramienta clásica de rename masivo: patrones con contadores `[N]`, slices `[N3-6]`, regex con grupos, cambio de mayúsculas, limpieza de caracteres; preview con detección de colisiones ANTES de ejecutar; deshacer como una sola transacción del journal. (El rename IA del §9 genera entradas para este mismo motor: un solo ejecutor, dos generadores.)

**17.4 Volúmenes y puntos de montaje.** Enumeración de unidades (Windows: letras + UNC; macOS: /Volumes; Linux: mounts + GVfs/udisks2), espacio libre en status bar, detección de medios extraíbles, eyección segura. Cambio de unidad como comando de primer nivel (Alt+F1/F2 en preset orthodox).

**17.5 Selección como objeto de primera clase.** Modelo de selección persistente por pane (sobrevive a re-sorts y refreshes por identidad de entrada, no por índice), selección por patrón (`+`/`-` de TC), inversión, guardado/restauración de selecciones, y filtros de vista (mostrar solo `*.rs`) distintos de la selección.

**17.6 Ciclo de vida del daemon.** Autoarranque bajo demanda (el primer cliente lo lanza), shutdown por inactividad configurable, upgrade sin drama (el daemon viejo rechaza clientes nuevos con versión mayor y se despide cuando termina sus tasks; `norte daemon restart --graceful`), **autenticación de socket** por peer credentials (SO_PEERCRED/named pipe SID) — solo el mismo usuario; TCP loopback requiere token. Un daemon por usuario, nunca root.

**17.7 Taxonomía de errores.** Enum de error del protocolo estable y documentado (`NotFound, PermissionDenied, Conflict{kind}, ProviderUnavailable{retryable}, Cancelled, PolicyDenied{rule}, EncodingLoss…`) con mapeo por-OS/provider en el borde. Los frontends renderizan errores por categoría, no parseando strings. Política de panics: un panic en un task no tumba el daemon (task supervisado → estado `Failed{panic}` + issue-ready report local).

**17.8 Observabilidad local.** `tracing` estructurado en todo el core con spans por task/sesión; log rotativo local (`~/.local/state/norte/`); `norte task inspect <id>` vuelca la traza de una operación. Flamegraphs de dev con `tracing-flame`. Nada sale de la máquina (coherente con §16.6).

**17.9 Symlinks, hardlinks y casos raros del FS.** Semántica explícita: copiar symlink = preguntar (follow/preserve/skip) con default configurable; detección de ciclos en recorridos (visited set por (dev,inode)/file_id); sparse files preservados donde el API lo permita; junctions/reparse points de Windows tratados como symlinks con badge propio; archivos abiertos/bloqueados en Windows → estrategia retry + informe, nunca cuelgue.

**17.10 Integración shell.** Wrapper `cd-on-quit` para bash/zsh/fish/PowerShell (modelo `y()` de Yazi), `norte --choose-files` como file-picker invocable por otras apps (protocolo portal en Linux: xdg-desktop-portal, fase 1.1), apertura de terminal en el cwd del pane activo.

**17.11 Git-awareness (columna, no cliente).** Estado git por entrada como columna/badge (modificado/untracked/ignored) vía plugin oficial `columns` — dogfooding del plugin system; norte no es un cliente git.

**17.12 i18n del propio producto.** Strings del TUI/GUI vía Fluent (`fluent-rs`), es/en desde v1 (ventaja: mercado hispanohablante desatendido en tooling de este nivel). Los docs en inglés primero (alcance global), README bilingüe.

**17.13 Accesibilidad.** TUI: no depender solo de color (badges textuales), temas de alto contraste, anchos compatibles con screen readers de terminal. GUI (M5): AccessKit desde el diseño, no retrofit.

**17.14 Empaquetado y releases.** Cross-compile reproducible (cargo-dist), canales stable/nightly, binarios firmados (notarización macOS, Authenticode Windows — presupuestar certificados), paquetes: Homebrew, winget, Scoop, AUR, Nix flake, .deb/.rpm. Auto-update: solo notificación en v1 (nunca auto-instalar; coherencia con postura de confianza).

---

## 18. Mejores prácticas de ingeniería (contrato del proyecto)

**Proceso y gobierno**
- **ADRs obligatorios** (`docs/adr/NNNN-*.md`, formato MADR) para toda decisión de arquitectura; el spec evoluciona por ADR, no por edición silenciosa. Los ADR 0001–0006 nacen del §16.
- **Trunk-based development**: ramas cortas (<3 días), PR pequeñas (<400 líneas diff netas objetivo), squash-merge, `main` siempre verde y releasable.
- **Conventional Commits** + `release-plz` (changelog y versionado automáticos por crate).
- **Definition of Done** de una PR: código + tests (unit y, si toca borde, integration) + docs (rustdoc de API pública, mdBook si es user-facing) + entrada de changelog + sin warnings. Sin excepciones "luego lo testeo".
- **CODEOWNERS** por crate; `norte-proto` y `policy` requieren revisión doble (superficie de compatibilidad y seguridad).

**Rust**
- Toolchain fijada (`rust-toolchain.toml`), MSRV declarada y testeada en CI (política: stable − 2).
- `#![forbid(unsafe_code)]` en todos los crates salvo `norte-vfs-local` (syscalls por OS), donde cada `unsafe` lleva comentario `// SAFETY:` y test.
- Clippy en modo `pedantic` + `warnings = deny` en CI; excepciones solo con `#[allow]` justificado en línea.
- Errores: `thiserror` en libs (tipos concretos), `anyhow` solo en binarios; nunca `unwrap()`/`expect()` fuera de tests salvo invariantes comentadas.
- API pública auditada con `cargo-semver-checks` en CI (romper semver rompe el build).
- `cargo-deny` (licencias, advisories, duplicados), presupuesto de dependencias: añadir una dep nueva al core requiere justificación en la PR (tamaño, mantenimiento, alternativas).
- Docs: `#![warn(missing_docs)]` en crates de API (`proto`, `vfs`, SDK plugins); ejemplos compilables (doctests) en todo trait público.

**Calidad continua**
- CI matrix: {ubuntu, macos, windows} × {stable, MSRV}; jobs: fmt, clippy, test, coverage-gate (≥85 % crates de lógica), semver-checks, deny, docs build, bench-smoke.
- Nightly job: fuzzing corto (10 min por target), tests de integración con testcontainers (sftp/minio), benchmarks completos con detección de regresión (>10 % → issue automática).
- Fixtures hostiles versionadas en `norte-testkit` como corpus canónico; añadir un bug de encoding/paths al corpus es parte del fix (test-first para regresiones).

**Documentación**
- mdBook de usuario (instalación, keymaps, config, plugins) + mdBook de contribuidor (arquitectura, protocolo, cómo escribir un provider/plugin) desde M1.
- El protocolo publica su JSON Schema generado en cada release (`norte-proto/schema/`).
- `ARCHITECTURE.md` estilo matklad en la raíz: el mapa mental del repo en una página.

---

## Anexo A — Candidatos a nombre

Criterios: pronunciable en ES/EN, ≤6 letras para el binario, sin colisión fuerte en crates.io/brew/GitHub, dominio razonable, evoca el género (navegación/orden) sin ser genérico.

| Nombre | Racional | Riesgos |
|---|---|---|
| **norte** | Homenaje directo a Norton Commander; "rumbo, referencia"; español universal | Común como palabra; SEO regular; `norte.dev` posiblemente libre |
| **rumbo** | "Heading/course": navegación pura; sonoro en EN | Marcas de viajes existentes; verificar crates.io |
| **estiba** | Colocación ordenada de la carga (estibar): materialista, exacto al dominio "ordenar archivos" | Menos obvio para anglófonos ("es-TEE-ba") |
| **veta** | Veta/filón: seguir la veta de los datos; corto, fuerte | Abstracto; colisiones menores |
| **derrota** | En náutica clásica, "derrota" = ruta trazada; guiño culto | En español coloquial significa "defeat": arriesgado, aunque memorable |
| **faro** | Guía, señal | Colisión dura: Grafana Faro. Descartable |

Recomendación: **norte** (primera opción) o **estiba** (si quieres algo más propio y registrable). Decisión antes de M0 para fijar crates, org de GitHub y dominio.
