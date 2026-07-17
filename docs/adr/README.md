# ADRs — norte

Decisiones de arquitectura en formato MADR. Se crean con el comando `/adr`.
La spec evoluciona por ADR, no por edición silenciosa (spec §18).

| Nº | Título | Estado |
|----|--------|--------|
| [0001](0001-vpath-representacion-wire.md) | Representación de VPath y su wire format | accepted |
| [0002](0002-runtime-async-blocking-io.md) | Runtime async y modelo de I/O bloqueante | accepted |
| [0003](0003-workspace-lints-licencias.md) | Estructura del workspace, lints y licencias | accepted |
| [0004](0004-convenciones-wire-protocolo.md) | Convenciones de wire del protocolo v0 | accepted |
| [0005](0005-provider-ancho-politicas-engine.md) | Ensanchado del contrato Provider y políticas del copy engine | accepted |
| [0006](0006-keymap-resolucion.md) | Semántica de resolución del keymap engine | accepted |
| [0007](0007-config-capas-hot-reload.md) | Config en capas: precedencia y hot-reload | accepted |
| [0008](0008-norte-encoding-frontera.md) | norte-encoding: frontera de detección/decodificación | accepted |
| [0009](0009-trash.md) | Papelera: crate trash, capability y degradación explícita | accepted |
| [0010](0010-frontera-core-plugin-config.md) | Frontera core/plugin/config para extensiones | accepted |
| [0011](0011-envelope-jsonrpc-daemon.md) | Envelope JSON-RPC 2.0, framing, transporte y daemon | accepted |
| [0012](0012-resume-transferencias.md) | Resume de transferencias: `.norte-partial`, reanudación y GC | accepted |
| [0013](0013-provider-sftp.md) | Provider SFTP: russh, contención del servidor hostil y testing | accepted |
| [0014](0014-provider-ftp.md) | Provider FTP: suppaftp, MLSD, testing in-process y cleartext | accepted |
| [0015](0015-conexiones-y-secretos.md) | Conexiones y secretos: connections.toml, keyring, TOFU, ed25519 | accepted |
| [0016](0016-provider-object-storage.md) | Provider de object storage: opendal, modelo de keys y S3 primero | accepted |
| [0017](0017-paginacion-cursor-fs-list.md) | Paginación por cursor de `fs.list`: stream retenido por conexión | accepted |
| [0018](0018-provider-archive.md) | Provider archive: zip/tar read-only como directorios virtuales | accepted |
| [0019](0019-papelera-logica-remota.md) | Papelera lógica `.norte-trash/` en providers remotos | accepted |
| [0020](0020-theming-crate-norte-theme.md) | Theming: crate `norte-theme`, roles semánticos y degradación de color | accepted |
| [0021](0021-distribucion-cargo-dist.md) | Distribución: binarios prebuilt e instalador `curl \| sh` con cargo-dist | accepted |
| [0022](0022-plugin-host-wasm-manifiesto.md) | Plugin host WASM: runtime, manifiesto, capabilities y los tres niveles | accepted |
| [0023](0023-journal-sqlx-sqlite.md) | Journal: sqlx sobre SQLite (WAL), hash-chain propia | accepted |
| [0024](0024-norte-mcp-puente-stdio.md) | norte-mcp: puente MCP stdio → daemon, sin SDK | accepted |
| [0025](0025-journal-anclaje-hmac-audit-export.md) | Journal: anclaje HMAC del head + audit export (M3-5) | accepted |
