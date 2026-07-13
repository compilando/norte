# 0018 — Provider archive: zip/tar read-only como directorios virtuales

- Estado: accepted
- Fecha: 2026-07-13
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §5 ("Archivos como directorios:
  `zip://<vpath-del-zip>!/ruta/interna`"), §6.1 (fila ZIP: bit 11 / cp437),
  §14 threat model (zip bomb); ADR 0001 (gramática VPath), 0005 (contrato
  Provider), 0016 (provider anterior, mismo patrón de frontera). Plan M2
  fase 8.

## Contexto y problema

M2 exige navegar zip/tar como directorios virtuales, solo lectura (escritura
= v1.1). A diferencia de sftp/s3, un archivo comprimido no es un backend
propio: vive DENTRO de otro provider (local, sftp, s3…). Preguntas:

1. **Direccionamiento**: cómo referencia un `VPath` una entrada interior sin
   romper la gramática de ADR 0001 (el spec sugiere `zip://…!/…`, pero un
   URI no anida en otro URI: el parser corta authority en el primer `/` y
   los segmentos prohíben `/`).
2. **Acceso a los bytes**: cómo lee el provider el contenedor sin violar la
   regla 2 (nada de `std::fs` fuera de vfs-local) ni la 9 (todo pasa por el
   VFS).
3. **Nombres de entrada**: zip/tar traen nombres hostiles POR DISEÑO
   (zip-slip `../`, absolutos, cp437 vs bit 11, duplicados, file-vs-dir).
4. **Zip bomb**: límites de índice y descompresión (threat model).
5. **Read-only**: cómo lo sabe el core/UI sin round-trip de un `write`
   fallido.

## Opciones consideradas

### A. Direccionamiento

- **A1 — handle de montaje con authority sintética** (`zip://m1/...` tras un
  `fs.mount_archive`): gramática intacta, pero estado en el daemon (los
  paths mueren con la sesión: bookmarks/historial rotos), método de
  protocolo nuevo y GC de montajes. Descartada.
- **A2 — URI exterior percent-encoded en un segmento**: los segmentos
  prohíben `/` (ni vía `%2F`, ADR 0001) — inviable sin un segundo nivel de
  escaping ad-hoc. Descartada.
- **A3 — scheme compuesto + segmento marcador `!`**:
  `zip+file:///home/o/a.zip/!/docs/x.txt`, `tar+sftp://user@host/d/a.tar/!/x`.
  La gramática de `Scheme` YA admite `+` (`[a-z][a-z0-9+.-]*`, ADR 0001, sin
  cambio de wire); `!` es un segmento válido. El PRIMER segmento igual a `!`
  parte exterior/interior: stateless, sobrevive reinicios, componible.
  Precedente: `tar:…!` de Apache Commons VFS, `svn+ssh`. ELEGIDA.

  Reglas NORMATIVAS de A3 (funciones canónicas en `norte-proto`:
  `VPath::archive_compose` / `VPath::archive_split`, doctested + goldens):
  - **Descomposición por whitelist, no por sintaxis**: un scheme es
    compuesto si y solo si su prefijo hasta el PRIMER `+` es un token de
    formato registrado (v1: `zip`, `tar`; la lista vive en norte-proto y
    ampliarla es cambio de protocolo). `s3+v2.x-y` (scheme de provider
    legítimo, pinneado en goldens) NO es compuesto: `s3` no es formato →
    `archive_split` = `Ok(None)`. Reserva normativa: ningún provider
    registrará jamás un scheme que empiece por `<formato-registrado>+` —
    el registro de formatos manda sobre el de schemes.
  - Split en el PRIMER segmento `!` del path; el resto es interior.
  - `compose` rechaza un segmento `!` literal en AMBOS lados (exterior:
    ese archivo no es direccionable como contenedor — marginal, asumido;
    interior: coherencia con C2, que omite esas entradas del índice, y
    con el anidamiento futuro). Un path compuesto llegado del wire con
    `!` extra en el interior no se rechaza en parse: resuelve `NotFound`
    en el provider (el índice jamás contiene componentes `!`).
  - Scheme compuesto SIN marcador `!` en el path (`zip+file:///a.zip`) es
    MALFORMADO: `archive_split` → `Err`. La raíz del interior lleva
    siempre el marcador: `zip+file:///a.zip/!`.
  - Los helpers operan SOLO sobre segmentos ya parseados (jamás sobre el
    string wire: `%21` ≡ `!` y un splitter textual sería vulnerable);
    toda clave derivada (caché) usa `to_wire()` canónico.
  - v1 una sola capa: tras quitar `<formato>+`, un interior que empiece
    a su vez por token de formato (`zip+tar+file`) → rechazo; el
    anidamiento (spec: "límite de profundidad configurable") queda
    diferido con issue. La sintaxis lo admite sin migración
    (`zip+tar+file:///a.tar/!/inner.zip/!/x`, resolución derecha→izquierda).

### B. Acceso a los bytes del contenedor

- **B1 — el provider abre el FS local**: rompe reglas 2/9 y limita a
  archivos locales. Descartada.
- **B2 — composición sobre `Arc<dyn Provider>` interior**: el core resuelve
  el provider del exterior y lo inyecta (`ArchiveProvider::new(inner, …)`).
  Lecturas por `Provider::read(range)` (pread, ADR 0005): zip usa range-reads
  (central directory al final); tar escaneo secuencial. Funciona igual sobre
  local/sftp/s3/mem (tests sin FS ni Docker: MemProvider). Parsers sync
  (`zip`, `tar` crates) corren en `spawn_blocking` sobre un adaptador
  `Read+Seek` que puentea con `Handle::block_on` (patrón ADR 0002). ELEGIDA.

### C. Nombres de entrada

- **C1 — decodificar nombres al indexar** (cp437/chardetng → String): viola
  la regla 1 (nombres = bytes) y corrompe roundtrips. Descartada.
- **C2 — bytes crudos + validación estructural**: el nombre se conserva
  byte-exacto como `Segment`s (la regla 1 hace innecesario decodificar; el
  bit 11 se conserva como metadato para la futura feature de display
  "reinterpretar nombres como…", issue aparte). Solo se valida ESTRUCTURA:
  entradas cuyo nombre no mapea a segmentos válidos (`..`, `.`, componente
  vacío, NUL, path absoluto, componente `!`, nombre > límite) se OMITEN del
  árbol con `tracing::warn!` + contador `skipped` (zip-slip defense:
  rechazar la operación entera dejaría un zip malicioso ilegible completo =
  DoS de un solo archivo; omitir con señal es lo que hacen TC/MC).
  Duplicados: última gana (semántica zip) + warn. Conflicto file-vs-dir en
  el mismo path: gana dir + warn (patrón de ataque conocido). La semántica
  de omisión queda en el rustdoc de `list` del provider (contrato, no
  sorpresa). ELEGIDA — validada por encoding-auditor en fase 8e.

### D. Límites anti-bomba

- **D1 — sin límites** (el consumidor decide): un central directory de 10⁷
  entradas revienta la RAM del índice antes de que nadie lea un byte.
  Descartada.
- **D2 — límites en la construcción del índice**: `Limits { max_entries:
  500_000, max_name_bytes: 4_096, max_depth: 64 }` (constantes v1,
  overridable en tests; configurables = issue). Excedidos →
  `Error::Io { retryable: false }` + warn. La descompresión de `read` es
  streaming consumer-driven (sin ratio-limit propio: el que lee decide
  cuánto acepta) y el anidamiento — el vector de bombas recursivas — ya
  está vetado en v1 (A3). Corrupto/truncado → `Io { retryable: false }`
  (variante `Corrupt` dedicada = issue; no amerita bump extra ahora).
  ELEGIDA.

### E. Read-only en capabilities

- **E1 — inferirlo** (ausencia de flags de escritura): APPEND/RANDOM_WRITE
  ausentes no implican no-write (sftp tampoco los tiene todos). Frágil.
- **E2 — flag `READ_ONLY = 1 << 8`**: la UI veta/atenúa mutaciones sin
  round-trip y el copy engine rechaza destino-archivo upfront. Wire change
  (bitflags serializan por nombre): golden + protocol-guardian. Toda
  mutación del provider responde `Unsupported` (regla 4 no aplica: no hay
  writes que journalar). ELEGIDA.

  **Bump 0.8.0 → 0.9.0, justificado**: por ADR 0004 un flag nuevo es
  aditivo-compatible y NO exigiría bump; se bumpa igualmente como señal de
  negociación (un cliente ≥0.9 puede asumir que el daemon entiende schemes
  compuestos y READ_ONLY), consistente con la práctica del proyecto
  (0.7→0.8 en fase 7f, también aditivo). Coste reconocido: en 0.x el minor
  es major efectivo (ventana N/N-1) → 0.7.x queda fuera; degradación N-1
  bien definida: un cliente 0.8 ignora el flag desconocido, intenta la
  mutación y recibe `Unsupported` (semántica de ADR 0004).

## Decisión

- **A3 + B2 + C2 + D2 + E2.** Crate `norte-vfs-archive` (Apache/MIT,
  `#![forbid(unsafe_code)]`, providers no se conocen entre sí — recibe
  `Arc<dyn Provider>`, jamás un tipo concreto).
- Formatos v1: **tar plano** (crate `tar`: ustar/GNU/pax battle-tested) y
  **zip stored+deflate** (crate `zip`, `default-features = false` +
  `deflate`; `name_raw()` para bytes crudos). Entrada cifrada o método no
  soportado: se lista (metadatos) pero `read` → `Unsupported`. tar.gz/tgz =
  issue (capa de compresión ortogonal, scheme `tar+gz+…` futuro). 7z y
  RAR-read (spec §3, tabla de crates) se difieren igualmente con issue;
  la spec (§5 sintaxis `zip://…!`, §5 capabilities sin READ_ONLY) se
  alinea con este ADR en su próxima revisión, como hizo 0004 con
  `TaskState`.
- Goldens nuevos de VPath (además del golden del flag): path compuesto
  válido con marcador, alias `%21` ≡ `!` (misma forma canónica), `!` en
  exterior, marcador final (raíz interior) y `!` múltiple — el corpus que
  fija que cualquier reimplementación split-textual es incorrecta.
- Índice por archivo cacheado (LRU cap 8 por provider, RAII como los
  listings de ADR 0017), clave = `to_wire()` canónico del exterior,
  invalidado por `(mtime_ms, size)` del `stat` exterior en cada operación;
  un exterior con `mtime_ms = None` se considera SIEMPRE stale (rebuild
  por operación: correcto aunque caro; ningún provider actual lo produce
  para archivos).
- Capabilities: `READ_ONLY | CASE_SENSITIVE | CASE_PRESERVING`.
- **Suite contractual read-only dedicada** (`readonly_provider_contract!`
  en norte-vfs): la suite RW existente siembra vía `write` del propio
  provider en ~20 de ~24 casos — no puede correr contra un provider
  READ_ONLY. La variante RO exige al factory un árbol canónico pre-sembrado
  (documentado en la macro) y verifica el contrato de lectura + que TODA
  mutación responda `Unsupported`. La suite RW no se toca.
- `read` de entrada tar = passthrough con range al interior (contigua, sin
  descompresión); zip = hilo blocking + canal acotado; drop del stream
  cancela (regla 3).
- Symlinks de tar: se listan como `Symlink`, `read_link` da el target crudo,
  `read` → `TypeMismatch` (coherente con lstat del contrato).

## Consecuencias

Positivas:

- Paths estables y componibles sin estado de sesión; bookmarks/historial a
  interiores de archivo sobreviven reinicios.
- Cero cambio de gramática wire de VPath; un solo bump de proto (0.9.0) por
  el flag.
- Archivos dentro de CUALQUIER provider (local, sftp, s3) gratis, y la
  suite contractual corre sobre MemProvider: sin FS del host, sin Docker,
  sin gate solo-Linux (primera suite de provider 100 % portable).
- La regla 1 (bytes) elimina de raíz la clase de bugs de decodificación
  cp437/UTF-8 que corrompen nombres en otros FM.

Negativas / riesgos asumidos:

- `!` como marcador es convención en banda: un archivo REAL llamado `!` en
  el path exterior no es direccionable como contenedor (marginal,
  documentado, detectado en compose).
- Entradas hostiles omitidas son invisibles para el usuario hasta que el
  contador `skipped` se exponga al frontend (issue de seguimiento).
- Range-read de zip sobre providers remotos de latencia alta es chatty
  (central directory + saltos); mitigado por caché de bloques de 256 KiB y
  el índice LRU. Optimización futura si duele: readahead adaptativo.
- Dos deps nuevas (`zip`, `tar`), justificación regla 8: parsers de formato
  hostil battle-tested (fuzzeados años), puro Rust (`flate2`/miniz_oxide),
  alternativa hand-rolled = deuda de seguridad permanente.
