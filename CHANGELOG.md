# Changelog

Todos los cambios notables de norte. Formato basado en
[Keep a Changelog](https://keepachangelog.com/es/1.1.0/); versionado
[SemVer](https://semver.org/lang/es/). La versión del PROTOCOLO (wire) es
independiente de esta y vive en `PROTOCOL_VERSION` (hoy `0.9.0`).

## [Unreleased]

## [0.3.0-alpha.1] — 2026-07-15

Primera versión etiquetada. Cierra el hito **M2** («remotos + archivos»); el
producto es un file manager ortodoxo usable para local, remotos y archivos
comprimidos. **Alpha**: los tests de daemon/socket se validan en CI (no en todo
entorno local); interfaz y config aún pueden cambiar.

### Añadido

**M2 — remotos + archivos**
- Provider **sftp** (russh): capabilities honestas, contención de servidor
  hostil (nombres `../../`, symlinks trampa), nombres como bytes.
- Provider **object storage / S3** (opendal): `CopyObject` server-side,
  paginación por cursor de listados enormes, keys UTF-8 byte-exactas.
- Provider **archive** zip/tar **read-only** como directorios virtuales
  (`zip+…!/ruta`): encoding de nombres ZIP honesto (bit 11/cp437), límites
  anti zip-bomb.
- **Copy engine cross-provider** con **resume** (`.norte-partial` + offset;
  multipart en S3), contrato anti-sobrescritura contra el destino.
- **Papelera lógica remota** `.norte-trash/` para providers sin trash del OS
  (sftp, object), con sidecar de procedencia bytes-safe.
- **Daemon JSON-RPC 2.0** sobre UDS/named pipe (auth por peer credentials,
  jamás root), envelope + framing NDJSON, autoarranque y shutdown por
  inactividad; frontends eligen embebido o daemon.
- **Conexiones + secretos**: `connections.toml` (solo referencias), keyring del
  OS, TOFU de host keys SSH.
- E2E del criterio de salida (`zip+remoto → S3 → local`), fuzz de framing y
  nombres ZIP, benchmarks del copy engine, nightly con testcontainers.

**M1 — TUI usable**
- TUI ratatui de dos paneles, motor de keymaps + presets, config en capas con
  hot-reload, viewer con detección de encoding, papelera con degradación
  explícita a borrado permanente.
- Strings de UI por Fluent (es/en).

**M0 — esqueleto**
- Workspace Cargo, `norte-proto` (tipos del protocolo), `norte-vfs` (trait
  `Provider` + `VPath` en bytes), scheduler de tasks con cancelación limpia,
  copy/move/delete local con progreso, CI en 3 OS con gate de coverage.

### Notas
- Nombres de archivo tratados como **bytes** en todo el stack (nunca se asume
  UTF-8); paths hostiles en el corpus canónico de `norte-testkit`.
- Licencias por crate: `norte-proto`/`norte-vfs*`/`norte-testkit` Apache-2.0 o
  MIT; `norte-core` y frontends AGPL-3.0.

### Pendiente (próximos hitos)
- **M3** agéntico: servidor MCP, policy engine, journal + undo de sesión,
  audit export.
- Escritura dentro de archivos zip; `list`/`restore`/`purge` de la papelera
  lógica; GUI (M5).

[Unreleased]: https://github.com/compilando/norte/compare/v0.3.0-alpha.1...HEAD
[0.3.0-alpha.1]: https://github.com/compilando/norte/releases/tag/v0.3.0-alpha.1
