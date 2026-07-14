# Fase 10 — Validación y criterio de salida de M2

- Fecha: 2026-07-14
- Estado: diseño aprobado (pendiente plan + implementación)
- Relacionado: spec §15 (criterio de salida M2), ADR 0010 (frontera; "sftp→S3→zip
  sin sorpresas"), 0011 (fuzzing de framing → fase 10), 0012 (resume), 0016
  (multipart S3 "fase 10 no debe darlo por hecho"), 0018 (archive read-only).

## Contexto

M2 (remotos + archivos) está funcionalmente completo: sftp, ftp, object/S3,
archive read-only, copy engine cross-provider con resume, papelera lógica
(fase 9). Fase 10 NO añade features: **valida** que el conjunto cumple el
criterio de salida "sin sorpresas" y hace el pase de calidad para declarar M2.

El criterio original de la spec ("copiar de sftp a zip local vía S3") asumía
ESCRIBIR en zip; archive es READ-ONLY (ADR 0018), así que se reinterpreta:
**leer DESDE un zip local y copiar su contenido a S3 y sftp, ida y vuelta entre
remotos, con resume y cancelación limpia.** El zip es ORIGEN, nunca destino.

## Decisiones tomadas (brainstorming)

1. **Criterio = leer desde zip → S3/sftp** + round-trip remoto↔remoto + resume +
   cancelación. Zip como origen (read); escritura en archive = deuda #55+ / fuera
   de M2.
2. **Alcance = los 4 ejes**: E2E del criterio, fuzz de framing (ADR 0011),
   honestidad de capabilities cross-provider, pase de calidad + declaración.
3. **Harness in-process** (services-fs/s3s-fs + servidor sftp in-process,
   solo-Linux) para que el E2E sea gate repetible en `just ci` (CI GitHub OFF por
   billing). El nightly (MinIO/OpenSSH reales) lo extiende — el multipart real de
   S3 (ETags) solo se cierra ahí (ADR 0016:184).
4. **Fuzz = proptest**, no `cargo-fuzz`: sin toolchain nightly ni target aparte,
   corre en `just ci`, coherente con el proptest ya usado.

## Descomposición

### 10a — Honestidad de capabilities cross-provider

Test transversal de la matriz declarado↔real sobre los providers instanciables
in-process (Mem, Local, sftp-in-process, object-fs, archive-zip/tar):

- **Coherencia por provider:** `SERVER_COPY` ⟹ `copy_native` = `Some`; ausencia
  de `RENAME_ATOMIC` ⟹ no se asume atomicidad; `READ_ONLY` (archive) ⟹ toda
  mutación = `Unsupported`; `TRASH` ⟺ `trash()` funciona; `APPEND` ⟺
  `open_resumable` reanuda.
- **Gating del engine:** copy degrada a streaming sin `SERVER_COPY`;
  `DeleteMode::Trash` sin `TRASH` = `Unsupported` (el engine NUNCA degrada solo,
  ADR 0009); archive read-only rechaza copy-hacia-dentro limpio.

Reusa `readonly_provider_contract!` (8c) + contrato general; añade el test de
matriz en `norte-core` (engine) y `norte-vfs` (providers). Sin infra nueva.

### 10b — E2E del criterio de salida (central)

Test de integración en `norte-core` (nivel Engine, no daemon). Remoto =
**object-fs (S3)** — el eje del criterio "vía S3"; barato (solo `opendal`
dev-dep, sin servidor). Providers sobre el mismo Engine: object-fs, Local sobre
tempdir, y un zip real sembrado con `ZipSmith` (8c). **sftp NO entra en este
E2E de core** (evita replicar su arnés russh de ~350 líneas): su semántica
cross-provider está cubierta por su suite contractual + el nightly OpenSSH real.

Secuencia:
1. **Leer desde zip:** árbol + nombres hostiles (corpus UTF-8-representable:
   emoji/unicode/espacios — S3 y sftp son UTF-8-only) + un binario grande.
2. **zip → S3:** byte-exacto + nombres preservados.
3. **zip → sftp:** ídem.
4. **Round-trip remoto↔remoto:** S3→sftp y sftp→S3, byte-exacto.
5. **Resume:** copia grande cortada a media (cancel token) → reanudar →
   byte-exacto sin recopiar (ADR 0012). El multipart real de S3 (ETags) va al
   nightly; el in-process s3s-fs no lo ejercita → documentado.
6. **Cancelación limpia:** destino limpio o `.norte-partial` marcado, jamás a
   medias.

Aserciones transversales: byte-exactitud (hash), preservación de nombres
(bytes), progreso monótono, sin panics. Pasos como `#[tokio::test]` separados o
secciones de uno.

Límite honesto documentado: zip como origen (read), nunca destino; multipart
real → nightly.

### 10c — Fuzz de framing NDJSON

Test proptest sobre el decoder de framing (`wire::framing`, NDJSON 16MiB O(1)):

- **Entradas adversarias:** bytes aleatorios, líneas sin `\n`, líneas > 16MiB,
  `\n` embebidos, UTF-8 roto, JSON truncado/anidado profundo, ráfagas de líneas
  vacías.
- **Invariantes:** nunca panic; nunca OOM (respeta 16MiB → error, no acumula);
  cada frame = `Ok(bytes)` o error tipado limpio; el decoder resincroniza tras un
  error (siguiente `\n`).

Estrategia en el corpus de `norte-testkit`. Sin `cargo-fuzz`.

### 10d — Pase de calidad + declaración M2

Verificación (código nuevo solo si algo falta):
- `cargo llvm-cov` ≥ 85% en core/vfs/proto.
- `just bench` en presupuesto: listado 100k <200ms primer render, copy, cold
  start TUI <50ms.
- `rustdoc -D warnings`, `#![warn(missing_docs)]` limpio, sin `TODO` sin issue.
- Nota de cierre M2 (spec/ADR): qué entró, deuda viva (Tier-3/M3/bloqueados de la
  sesión de triage 2026-07-14), estado del criterio de salida.

## Testing

Fase 10 ES tests; los fallos son aserciones. Test-first en cualquier bug que el
E2E destape (reproducir rojo, luego arreglar). Los tests hostiles de encoding
reusan/añaden al corpus de `norte-testkit` (regla CLAUDE.md).

## No-objetivos (YAGNI)

- Escritura en archive (deuda #55+).
- Multipart real de S3 en el gate de PR (va al nightly).
- Daemon/frontend E2E (10b es a nivel Engine; el daemon ya tiene su E2E de fases
  previas).
- Cerrar la deuda Tier-3/M3/bloqueada (queda como issues vivos).

## Decomposición de PRs

- **10a** capabilities honesty (test matriz).
- **10b** E2E criterio de salida.
- **10c** fuzz de framing.
- **10d** pase de calidad + nota de cierre M2.
