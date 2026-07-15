# Cierre M2 — «remotos + archivos»

Criterio de salida (spec §15): **«copiar de sftp a zip local vía S3 sin
sorpresas»**. Interpretación fijada en el kickoff (decisión 1): archive es
READ-ONLY en M2, así que la cadena LEE un zip alojado en un remoto
(composición `zip+…!` de fase 8f) y lo restaura en local pasando por S3.
Escribir DENTRO de un zip → M3 (issue).

## Estado: COMPLETO

Fases 1–10 entregadas (una fase = un commit convencional con `just ci`
verde). Providers de la cadena: sftp (0013), object/S3 (0016), archive
zip/tar read (0018), local. Daemon JSON-RPC + envelope + framing (0011).
Copy engine cross-provider + resume (0012). Paginación por cursor (0017).
Papelera lógica remota (0019).

## Cómo queda DEMOSTRADO el criterio de salida

La cadena completa está probada por DOS caminos complementarios:

1. **E2E en el gate de PR** (`crates/norte-core/tests/e2e_exit_criterion.rs`,
   fase 10a): la cadena entera `zip+remoto → S3 → local` corre en CI SIN
   Docker — `MemProvider` espeja el sftp (el plan M2 lo prevé como espejo de
   los remotos), `ObjectProvider` sobre `services-fs` hace de S3,
   `LocalProvider` real de destino. Asserts «sin sorpresas» deterministas:
   fidelidad byte-exacta de contenido y nombres hostiles UTF-8 en cada salto,
   colisión = fallo limpio, nombre no-UTF8 muere limpio en la frontera S3.
   Cancelación granular y resume son propiedades del engine independientes del
   provider — probadas en `engine.rs`/`engine_resume.rs`.

2. **Nightly per-provider contra servidores REALES** (fuera del gate,
   `nightly.yml` → `just it-remote`): sftp real (`openssh.rs`, OpenSSH por
   testcontainers), object real (`reals3.rs`, MinIO), ftp real (`realftp.rs`,
   pure-ftpd). Cada provider valida su fidelidad de bytes/semántica contra el
   servidor de producción que espeja.

3. **Fuzz corto en el gate** (fase 10b): framing/envelope JSON-RPC
   (`norte-proto/tests/wire_fuzz.rs`) y nombres/robustez ZIP
   (`norte-vfs-archive/tests/zip_fuzz.rs`).

4. **Benchmarks** (fase 10c, `norte-core/benches/copy_remoto.rs`): throughput
   del copy engine cross-provider con latencia de remoto inyectable. `#27`
   (paginación) CERRADO — ver nota en ADR 0017.

## Deuda trazada (a M3 o issue propia)

- **E2E nightly de 2 contenedores** (la cadena ENTERA contra OpenSSH + MinIO
  reales SIMULTÁNEOS, no solo per-provider): pendiente. No aporta cobertura
  nueva sobre (1)+(2) —la cadena ya está probada en CI y cada remoto contra su
  servidor real— pero sería la demostración «belt-and-suspenders» del criterio
  literal. Diferido porque el entorno de desarrollo actual no tiene
  docker-in-docker para runtime-verificarlo; se escribiría compile-only.
  **Issue por abrir** (hito M3 / infra nightly).
- **Escribir dentro de zip** (archive write): M3, decisión de kickoff 1.
- Papelera lógica remota: `list`/`restore`/`purge` + GC de huérfanos → M3
  (ADR 0019; el sidecar `meta/` ya queda sembrado).
- Deuda menor viva en sus issues: merge incremental del fill del TUI,
  tuning de `max_blocking_threads`/TTL de listados, single-flight de
  conexiones (#47), streaming del rename de prefijos S3 (#49).

## Nota de entorno

Sin toolchain Rust local en la máquina de desarrollo: `just ci` se corrió en
`docker run rust:1.96.1` (toolchain pineado). Dos clases de test NO se pueden
runtime-verificar ahí, ambas por correr **como root dentro del contenedor** —
verificado idéntico en el commit BASE previo a este trabajo, luego ajeno a M2:

1. **Daemon/socket** (`daemon.rs`, `backend_remote.rs`, `connect_real.rs`):
   el daemon aplica la guarda «un daemon por usuario, jamás root» (spec §17.6),
   así que `Daemon::bind` falla bajo uid 0 → caen todas las pruebas de socket.
2. **Permisos POSIX** (`known_hosts::fichero_ilegible_es_error_no_tofu`): root
   ignora `chmod 000`, así que la lectura «ilegible» no falla.

Ambas pasan en el runner de CI (no-root). Los tests de contenedor (nightly)
tampoco se runtime-verifican aquí (sin docker-in-docker); los valida el runner
nightly de GitHub. Todo lo AÑADIDO en M2-fase-9/10 (vfs/sftp/object/archive/
proto + los E2E/fuzz/bench nuevos de core) SÍ corre y pasa en el docker
no-privilegiado de estas pruebas.
