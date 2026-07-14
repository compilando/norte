# Cierre de M2 — remotos + archivos

- Fecha: 2026-07-14
- Estado: **M2 COMPLETO** (criterio de salida verificado por E2E)

## Criterio de salida

Spec §15 / ADR 0010: *"copiar de sftp a zip local vía S3 sin sorpresas"*. Con
archive READ-ONLY (ADR 0018) el zip no puede ser destino, así que se
reinterpretó (fase 10, aprobado) como: **leer DESDE un zip local y mover su
contenido por S3 y de vuelta, byte-exacto, con nombres hostiles preservados y
cancelación limpia.**

Verificado por el E2E `crates/norte-core/tests/e2e_exit_criterion.rs` (fase 10b):
zip → object-fs (S3) → local (Mem), round-trip S3↔S3, nombre hostil UTF-8
(`año 名前 😀.txt`) preservado byte a byte, cancelación de un binario de 256 KiB
sin objeto a medias. Remoto = object-fs in-process; sftp queda cubierto por su
suite contractual + el nightly OpenSSH real; el multipart real de S3 (ETags) va
al nightly (ADR 0016).

## Qué entró en M2

- **Providers remotos**: sftp (ADR 0013), ftp + FTPS (0014), object storage /
  S3 (0016). Conexiones + secretos + TOFU (0015).
- **Copy engine cross-provider** con resume (0012) y paginación por cursor de
  `fs.list` (0017).
- **Archive read-only**: zip/tar como directorios virtuales, scheme compuesto
  (0018).
- **Papelera lógica remota** `.norte-trash/` (0019, fase 9): opt-in por
  conexión, sftp (rename) + object (copy-all→delete-all), metadatos de
  restauración con guard confused-deputy.
- **Daemon** JSON-RPC sobre UDS (0011) con framing NDJSON, auth por peer-cred.

## Salud del hito (fase 10)

- **10a** honestidad de capabilities: cross-check `SERVER_COPY⟺copy_native` en
  el macro de contrato (todos los providers) + gating del engine (no degrada
  `Trash` sin cap, veta copy-hacia-READ_ONLY).
- **10b** E2E del criterio (arriba).
- **10c** fuzz del framing NDJSON (proptest): independencia de troceado,
  resync, oversize sin OOM.
- **10d** calidad: cobertura 85.9% (≥85% gate en core/vfs/proto), presupuestos
  verdes (cold start 0.78 ms <50 ms, primer render 100k 0.69 ms <200 ms),
  rustdoc `-D warnings`, sin `TODO` sin issue. `just ci` verde local (CI GitHub
  desactivado por billing).

## Deuda que sale de M2 (tracked, no bloqueante)

Sesión de triage 2026-07-14. **Cerrados**: #21 (bidi display), #43 (cap PASS
log), #40 (parcial: parser LIST + cota). **Diferidos con razón**:

- **Tier-3 / features**: #49 (rename S3 streaming), #47 (ciclo de vida
  sesiones), #54 (merge incremental TUI), #55/#56 (tar.gz / anidamiento),
  #59 (parser CD zip propio), #61 (single-flight índice), #57 (reinterpretar
  encoding), #52 (d_type lazy — exige coordinar el walk).
- **Acoplados a M3**: #32 (desambiguación → journal), #35 (verify=Hash →
  superficie trait). #11 (journal), #34 (hardening daemon).
- **Wire/UX pendientes (Tier-2, no hechos)**: #58 (`Error::Corrupt`), #44
  (degradación TLS por protocolo), #45 (modal TOFU TUI), #60 (fixtures tar
  longname/pax).
- **Bloqueados por entorno**: #42/#50 (CI/Docker, billing OFF), #25/#33
  (Windows), #37/#48 (lossy upstream russh/opendal), #26/#51.
- **Fase 9**: `trash::execute` anti-duplicación, fault-injection rename-fail,
  `corpus::utf8_hostile_names`, `poisoned_trash_info` → M3 restore.

## Siguiente

**M3 — agéntico**: norte-mcp server, scopes, policy engine, journal + undo,
audit export. Aquí aterrizan el restore de la papelera (lee `.norte-info`) y
buena parte de la deuda acoplada a M3.
