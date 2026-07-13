# 0016 — Provider de object storage: opendal, modelo de keys y S3 primero

- Estado: accepted
- Fecha: 2026-07-13
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §3 (tabla de crates: `norte-vfs-object` = "S3/GCS/Azure,
  una crate, backends feature-gated, opendal"); ADR 0005 (contrato Provider),
  0012 (resume — compromiso multipart para S3), 0013/0014 (providers remotos,
  mismo patrón), 0015 (conexiones y secretos). Plan M2 fase 7. ADR 0017
  (paginación de `fs.list`, misma fase).

## Contexto y problema

Object storage es el tercer proveedor remoto y el primero NO-jerárquico: S3 no
tiene directorios, ni rename, ni append; tiene keys UTF-8 planas, prefijos con
delimitador, multipart uploads y `CopyObject` server-side. Preguntas:

1. **Qué librería** y cómo se aísla del trait `Provider` (regla: la dep no
   cruza la frontera pública).
2. **Modelo de directorios** sobre un espacio de keys plano.
3. **Nombres**: keys S3 = UTF-8 (máx. 1024 bytes) vs regla dura 1 (bytes).
4. **create-new** (`write` exige `Conflict` si el destino existe) sin
   filesystem que dé `O_EXCL`.
5. **Resume** (ADR 0012 prometió multipart en fase 7).
6. **`copy_native`**: S3 fue el motivo por el que existe el hook — primera
   implementación real del repo.
7. **Testing** sin Docker en el gate de PR.

## Opciones consideradas

### A. Librería

- **A1 — `aws-sdk-s3`**: oficial, pero S3-only (la spec exige camino a
  GCS/Azure en la misma crate), y el stack smithy es igual de grande.
- **A2 — firma propia (`reqsign`/`rusty-s3` + reqwest)**: control total, pero
  mantener firma sigv4 + XML + paginación a mano es deuda permanente.
- **A3 — `opendal` 0.58 (`default-features = false`, `features =
  ["services-s3"]`)**: Apache-2.0, MSRV 1.91 (< 1.94 del workspace),
  multi-backend feature-gated ("GCS/Azure después sin tocar código", spec §3),
  lister perezoso con ContinuationToken nativo, `if_not_exists` en write y
  copy (conditional writes). Dep GRANDE (reqwest/reqsign/quick-xml
  transitivas): feature-gate mínimo y `cargo deny` vigilante (riesgo asumido
  en plan-m2). ELEGIDA.

### B. Frontera con el trait

- **B1 — el provider construye su cliente**: acopla a config/credenciales
  (violaría reglas 7/10 y el patrón de ADR 0015).
- **B2 — inyección de `Operator`**: `ObjectProvider::new(op, scheme,
  authority)` recibe un `opendal::Operator` YA configurado (bucket, region,
  endpoint, credenciales) que construye `norte-connect` en fase 7d. El
  provider JAMÁS ve `secret_access_key`; el único tipo de opendal en la API
  pública es el del constructor (re-exportado por norte-connect, patrón
  `FtpStream`). ELEGIDA.

### C. Modelo de directorios

- **C1 — todo es prefijo implícito**: sin markers; `mkdir` sería no-op y un
  dir vacío no existiría — rompe el contrato (`mkdir`+`stat`, dir vacío
  listable).
- **C2 — marker objects (`key/`) + prefix-probe**: `mkdir` crea el marker
  (`create_dir` de opendal, `/` final obligatorio) tras validar padre
  (`NotFound`) y no-existencia (`Conflict`); `stat` resuelve en orden
  file → marker → prefijo-con-hijos (list limit 1) → `NotFound`; raíz = `Dir`
  siempre. Precedencia file > dir documentada (S3 permite `x` y `x/`
  coexistiendo; imposible de crear desde el propio provider porque
  write/mkdir se chequean mutuamente). ELEGIDA.

## Decisión

- **A3 + B2 + C2.** Crate `norte-vfs-object` (MIT/Apache; providers no se
  conocen entre sí). `#![forbid(unsafe_code)]`. Deps: `opendal`
  (services-s3), `tokio`, `bytes`, `futures`, `async-trait`, `norte-vfs`,
  `norte-proto`.
- **Nombres — UTF-8-only (D2 de 0013/0014)**: la key se compone SIEMPRE desde
  `Segment`s validados del `VPath` (jamás un path ecoado); segmento no-UTF8 →
  `InvalidPath`; key total > **1024 bytes** (límite S3) → `InvalidPath`
  upfront (→ `max_path: Some(1024)` en capabilities). En `list`, un nombre del
  backend vacío o con `/`, `.`, `..` o U+FFFD corta el listado con
  `InvalidPath`. SIN filtro CRLF: S3 viaja por HTTP firmado (sigv4 cubre el
  path), no hay protocolo de líneas que inyectar — divergencia justificada
  respecto a FTP (ADR 0014).
- **Whitespace en extremos → `InvalidPath`** (hallazgo del spike de 7b): el
  `normalize_path` de opendal-core hace `path.trim()` — un nombre con
  whitespace Unicode inicial/final se RENOMBRARÍA en silencio ("file " →
  "file") en TODOS los backends, corrupción de bytes (regla 1) que S3 sí
  permitiría. Rechazo fail-loud uniforme por segmento hasta que upstream
  preserve los bytes (deuda con issue, misma familia que #37).
- **Sink / create-new**: el multipart upload (o el PutObject bufferizado de
  opendal para objetos pequeños) ES el staging invisible natural — nada existe
  en la key final hasta `close()`. `write(p)` = validar padre + stat-check de
  destino (file y dir) → `Conflict` AL ABRIR (lo exige el contrato) + `writer_
  with(key).if_not_exists(true)`; `commit` = `close()` (el `If-None-Match: *`
  viaja en el PutObject/Complete; `ConditionNotMatch` → `Conflict`) — race-free
  en servidores honestos, MEJOR que el TOCTOU de ftp; `abort` =
  `writer.abort()` (AbortMultipartUpload). El stat-check upfront se mantiene
  SIEMPRE como cinturón: contra un servidor S3-compatible que IGNORE
  If-None-Match la garantía degrada al nivel ftp (check racy), nunca a
  sobrescritura sin chequeo. No hay `.norte-partial` remoto en S3.
- **Resume — DIFERIDO con issue y hito** (decisión de oscar, 2026-07-13).
  Verificado en docs.rs: el `Writer` de opendal 0.58 NO expone
  `upload_id`/`ListParts` ni reanudar un multipart existente — el compromiso
  literal de ADR 0012 ("`open_resumable` consultará ListParts…") no es
  implementable vía opendal hoy. `open_resumable` hereda el default del trait
  (`(write(p), 0)`) y `keep` hereda `keep = abort`: cancelar deja el destino
  LIMPIO y reanudar recopia desde cero — correcto y seguro (coherente con B2
  de ADR 0012: un provider sin reanudación no deja parcial; el contrato se
  auto-degrada con `already == 0`). Tres caminos para la issue, por orden de
  preferencia: (i) contribuir la superficie de multipart-resume a opendal,
  (ii) llamadas S3 crudas acotadas a ListParts/UploadPart/Complete con
  `reqsign` (ya en el árbol), (iii) `aws-sdk-s3` acotado a esa pieza. Este ADR
  RE-PROGRAMA el compromiso de 0012, no lo borra.
- **`copy_native` (fase 7c)**: `CopyObject` server-side; destino existente
  (file o dir) → `Conflict` (el `ensure_absent` no es solo cinturón: es el
  ÚNICO guard contra un destino DIRECTORIO, que el `If-None-Match` del copy no
  ve); `copy_with(...).if_not_exists(true)` si la capability del `Operator` lo
  confirma (`copy_with_if_not_exists`), si no check racy documentado. OJO:
  `Operator::copy`/`rename` de opendal SOBRESCRIBEN por defecto — nunca
  llamarlos sin el check. `SERVER_COPY` se declara solo si el backend anuncia
  `copy` (un `Operator` sin copia haría fallar en duro un fichero, sin
  fallback a streaming). opendal hace UploadPartCopy automático para >5 GiB
  (sin tope oculto). **Cancelación**: el engine llama `copy_native` en un solo
  `.await` — un multipart copy largo no tiene punto de corte (regla 3): issue
  #51, se ataca en el wiring de 7d/7e.
- **Resto de semántica**: `list` = stat previo (`NotFound` honesto) + stream
  PEREZOSO sobre el lister de opendal (pagina con ContinuationToken por
  debajo; engancha con ADR 0017 sin tocar el trait), filtrando la self-entry
  que opendal devuelve. `read` con rango vía `read_with(...).range(...)`;
  416/RangeNotSatisfied → stream vacío (semántica pread del trait). `remove` =
  stat previo (el delete de opendal es idempotente y mentiría con `NotFound`);
  dir con hijos → `Conflict`. `rename` = check de destino racy documentado +
  file: copy+delete; dir: walk del prefijo copy-all-then-delete-all (un fallo
  a mitad deja duplicados, JAMÁS pérdida) — no atómico y O(n), documentado en
  rustdoc.
- **Capabilities honestas**: `CASE_SENSITIVE | CASE_PRESERVING` (keys = bytes
  UTF-8 exactos) + `SERVER_COPY` (desde 7c) + `max_path: Some(1024)`. NO:
  `APPEND` (S3 no tiene append — sin él el engine ni intenta resume por
  offset), `RANDOM_WRITE`, `SYMLINKS`, `RENAME_ATOMIC`, `TRASH` (papelera
  lógica `.norte-trash/` = fase 9). `node_id` → default `Ok(None)`.
- **Config/credenciales (fase 7d, patrón ADR 0015)**: `access_key_id` = 
  referencia en `connections.toml` (NO es secreto); `secret_access_key` por
  `SecretResolver` (UN secreto — encaja sin tocarlo); `deny_unknown_fields`
  caza un `secret_access_key` inline (test). `auth = "access-key"` explícito
  desactiva la cadena ambiente (`disable_config_load` +
  `disable_ec2_metadata`); `auth = "agent"` en s3 = cadena ambiente de opendal
  (`AWS_*`/perfil/IMDS — el caso CI). Endpoint https por defecto; http =
  opt-in visible en config. Sin TOFU: S3 va por TLS/WebPKI, nunca emite
  `HostKeyUnknown`.
- **Testing — harness DUAL (el spike de 7b activó el plan B)**. El spike
  midió s3s-fs FIEL en multipart, invisibilidad pre-commit, abort,
  `if_not_exists` (simple y multipart), rangos y delimiter — e INFIEL justo
  donde el contrato aprieta: markers de dir vacío invisibles al LIST,
  nombres cerca del NAME_MAX de su fs → 500, `HeadObject` de un
  path-directorio → 500 (S3 real: 404) y `copy_with if_not_exists` ignorado.
  Resultado:
  1. `tests/contract.rs`: `provider_contract!` + corpus hostil sobre un
     `Operator` **`services-fs`** con `atomic_write_dir` (misma lógica del
     provider, sin HTTP; fixtures de >246 bytes filtradas — NAME_MAX del
     harness, no del provider), solo-Linux.
  2. `tests/s3.rs`: suite S3-ESPECÍFICA contra **s3s-fs** in-process
     (127.0.0.1:0, patrón libunftp): multipart real, ventana de carrera del
     conditional write, rangos, nombres — solo donde es fiel.
  3. `tests/hostile.rs`: contención provider-level + servidor S3 MENTIROSO
     por HTTP crudo (keys con `/` inyectado / U+FFFD cortan el listado).
  4. Nightly (feature `it-s3`, fuera del gate): MinIO real por testcontainers
     (spec §12) — roundtrip, hostiles + keys largas, dirs/markers,
     `copy_native` y conditional-write real.

## Consecuencias

Positivas:

- Tercer provider remoto sobre el MISMO contrato/patrón (inyección +
  in-process + nightly): coste marginal contenido.
- Camino GCS/Azure = activar features de opendal, sin tocar el provider.
- Primera implementación real de `copy_native` — valida el hook del engine
  (el contrato `contract_copy_native_iff_capability` ya la esperaba).
- Conditional writes (`If-None-Match`) dan create-new race-free donde el
  servidor es honesto — mejor garantía que ftp.

Negativas / deuda asumida:

- **Resume multipart diferido** (issue con hito): reanudar en S3 = recopia
  hasta entonces. La fase 10 NO debe darlo por hecho.
- **opendal es una dep grande**: transitivas reqwest/reqsign/quick-xml/hyper
  bajo vigilancia de `cargo deny`; pila cripto ÚNICA (aws-lc-rs — si `cargo
  tree` muestra `ring`, se ajustan features antes del commit).
- **Nombres UTF-8-only**: misma asimetría que sftp/ftp (aquí es límite del
  protocolo S3, no de la librería — no aplica el fix de bytes crudos #37).
- **rename/remove de prefijos no atómicos y O(n)**; cancelación a mitad deja
  duplicados (nunca pérdida).
- **Colisión de clave de caché**: `s3://bucket` sin endpoint en la authority →
  el mismo nombre de bucket en DOS endpoints distintos comparte clave de caché
  del engine (issue; tocar la authority sería cambio de wire).
- s3s-fs experimental como harness del gate: plan B documentado arriba.
- **Deuda upstream de opendal** (issue #48): `normalize_path` hace `str::trim`
  (renombrado silencioso, mitigado con rechazo fail-loud en `key()`) y
  `build_rel_path` panica ante una key ecoada más corta que el `root` (minor;
  se defiende validando longitud en 7e). **rename de prefijo materializa el
  árbol en RAM** (issue #49, streaming pendiente). Cobertura que el harness
  in-process no alcanza (keys largas, siembra externa, root no vacío) → nightly
  MinIO, issue #50.
