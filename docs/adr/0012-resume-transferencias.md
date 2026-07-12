# 0012 — Resume de transferencias: `.norte-partial`, reanudación y GC

- Estado: accepted
- Fecha: 2026-07-12
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §5 («resume»), §15 (criterio de salida M2); ADR 0005
  (contrato Provider, `read` con rango, caps `APPEND`/`RANDOM_WRITE`);
  ADR 0004 (convenciones wire); plan M2 fase 4.

## Contexto y problema

M1 dejó el sink transaccional: los bytes van a un staging
`.norte-partial.<hash>.<pid>-<seq>` y `commit()` los publica atómicamente;
cancelar o fallar llama `abort()` y borra el staging (destino LIMPIO). Falta
la otra mitad del contrato de la spec: **reanudar** una transferencia
interrumpida sin recopiar lo ya transferido — crítico para remotos (una
copia sftp/S3 de GB que se corta a la mitad no puede empezar de cero).

Cuatro decisiones de diseño:

1. **Dónde vive el «cuánto llevo»** — y cómo no casarse con POSIX (en S3 la
   reanudación es *multipart*, no un offset de archivo).
2. **Qué pasa con el staging al cancelar** (hoy se borra; reanudar exige
   conservarlo).
3. **Nombre del staging** (hoy es único por `pid+seq`, imposible de
   reencontrar en una segunda invocación).
4. **Integridad**: un parcial que ya no corresponde al origen (origen
   cambiado, parcial corrupto) no debe reanudarse a ciegas.

## Opciones consideradas

### A. Modelo de reanudación en el trait

- **A1 — el engine hace `seek`/`APPEND` sobre el archivo parcial**: asume
  semántica POSIX de archivo con offset. En S3 no hay «archivo parcial con
  offset»: hay un *multipart upload* con partes ya subidas. Casaría el
  engine con el FS local y rompería en la fase 7.
- **A2 — método `open_resumable(p) -> (sink, already: u64)`**: el provider
  informa cuántos bytes YA hay durables para `p` (`0` = empezar de cero) y
  devuelve un sink que AÑADE después de ellos. El engine solo sabe «llevo
  `already`, sigo desde ahí leyendo el origen con rango» — abstracción que
  encaja igual sobre un `.norte-partial` local (append) que sobre un
  multipart S3 (partes ya subidas · tamaño de parte). Default del trait:
  `(write(p), 0)` — un provider sin reanudación empieza de cero, sin romper.

### B. Semántica del staging al interrumpir

- **B1 — conservar SIEMPRE el parcial**: cada cancelación deja un
  `.norte-partial` — cambia el contrato de M1 («destino limpio») para todas
  las copias y exige GC agresivo. Sorpresa para el usuario que hoy espera
  limpieza.
- **B2 — reanudación OPT-IN** (`TransferOptions.resume`): con `resume=Off`
  (default) el comportamiento es EXACTAMENTE el de M1 (cancelar/fallar →
  `abort` → limpio). Con `resume=On`, cancelar o un fallo transitorio
  CONSERVA el parcial (vía `ByteSink::keep`) para que la próxima copia del
  mismo `src→dst` reanude. La invariante de la casa se mantiene y se enriquece:
  «cancelar deja destino limpio **o** `.norte-partial`, jamás un archivo a
  medias sin marcar» — ahora el `.norte-partial` es reanudable, no basura.

### C. Nombre del staging

- **C1 — seguir con `pid+seq`**: único, sin colisiones, pero irreencontrable
  entre invocaciones → no hay reanudación cross-proceso (el caso «se me cayó
  la copia, la relanzo»).
- **C2 — nombre ESTABLE por destino** (`.norte-partial.<hash-del-nombre-final>`):
  una segunda copia al mismo `dst` reencuentra el parcial y reanuda. El hash
  es SHA-256 truncado a **128 bits** (no `DefaultHasher`, que ni es estable
  entre versiones de Rust ni resiste colisión): así el nombre es estable
  cross-versión y dos destinos DISTINTOS jamás comparten staging por
  colisión — ni accidental (birthday 2^64) ni adversarial con nombres desde
  un archivo no confiable (2^64) (hallazgo H1/H3 del encoding-auditor).
  Contrapartida residual: dos copias CONCURRENTES al MISMO `dst` con
  `resume=On` compartirían staging — pero eso ya es un conflicto lógico
  (el `commit` no-replace lo caza) y no es el caso de uso (secuencial:
  reintento o relanzamiento). No soportado, documentado.

### D. Integridad al reanudar

- **D1 — confiar en la longitud**: reanudar desde `already` sin más. Rápido;
  ciego a un origen que cambió de contenido (mismo tamaño) o a un parcial
  corrupto.
- **D2 — verificación por política** (`TransferOptions.verify`):
  `Length` (default: `already` debe ser ≤ tamaño del origen, si no se
  descarta el parcial y se empieza de cero) o `Hash` (además, el hash de
  `origen[..already]` debe coincidir con el del parcial; si no, se descarta).
  `Hash` cuesta releer `already` bytes de ambos lados — opt-in.

## Decisión

- **A2 + B2 + C2 + D2.**
- **Trait** (`norte-vfs`): `async fn open_resumable(&self, p: &VPath) ->
  Result<(Box<dyn ByteSink>, u64), Error>`, default `(self.write(p).await?,
  0)`. Y `ByteSink::keep(self: Box<Self>) -> Result<(), Error>`: suelta el
  staging SIN publicar y SIN borrar (durabilízalo — `sync` en local), para
  que `open_resumable` lo reencuentre; default `keep = abort` (un provider
  sin reanudación no deja parcial: degrada a limpio, coherente con B2).
- **`LocalProvider`**: staging con nombre estable
  `.norte-partial.<sha256-128>` (32 hex del nombre final; se elimina
  `pid+seq`); el GC reconoce el staging por su FORMA exacta, no por el
  prefijo suelto (un `.norte-partial.backup` del usuario jamás se barre —
  H2);
  `open_resumable` abre `O_WRONLY|O_APPEND|O_CREAT`, `stat`ea su longitud y
  la reporta; `keep` hace `sync_all` y suelta sin renombrar; declara
  `APPEND`. GC: `list_partials(dir)` + `gc_partials(dir, older_than)`
  (nombre reconocible por el prefijo, mtime como edad).
- **Engine**: `copy_file` con `resume=On` llama `open_resumable`, obtiene
  `already`; si `already > tamaño_origen` o (con `verify=Hash`) el hash de
  `origen[..already]` no cuadra → descarta el parcial (`abort`) y reabre de
  cero; si cuadra, lee el origen con rango `offset=already` y continúa. En
  cancelación o fallo transitorio con `resume=On`: `keep` (el parcial
  sobrevive); con `resume=Off`: `abort` (limpio, M1). El progreso refleja
  `already + escritos` desde el arranque (la barra no «retrocede» al
  reanudar).
- **Wire (0.5.0 → 0.6.0)**: `TransferOptions` gana `resume: ResumePolicy
  {Off, On}` (default `Off`) y `verify: VerifyPolicy {Length, Hash}`
  (default `Length`), como campos opcionales de `fs.copy`/`fs.move`
  (`#[serde(default)]` → compatible; un cliente N-1 obtiene `Off`/`Length`,
  el comportamiento de siempre). Golden + bump + protocol-guardian.
- **S3 (fase 7, no ahora)**: `open_resumable` consultará el multipart upload
  en curso (`ListParts`) y reportará `already = Σ tamaños de parte`; el sink
  subirá partes nuevas; `keep` deja el multipart abierto; `commit` hace
  `CompleteMultipartUpload`. El engine no cambia: sigue viendo «llevo
  `already`, sigo desde ahí». El ADR nace, como pide el plan, con multipart
  en mente.

## Consecuencias

Positivas:

- El criterio de salida M2 («sftp→S3→zip sin sorpresas») incluye reanudar
  un salto cortado; el engine lo hace sin conocer el mecanismo del provider.
- Default `Off` preserva el contrato de M1 al pie de la letra: cero sorpresas
  para quien no pide resume.
- La abstracción `already` no hipoteca POSIX: la fase 7 encaja S3 sin tocar
  el engine.

Negativas / deuda asumida:

- Nombre estable ⇒ dos copias concurrentes al MISMO destino con `resume=On`
  corromperían el staging compartido — no soportado, documentado (ya es un
  conflicto lógico; el caso de resume es secuencial).
- Un `.norte-partial` reanudable que nunca se reanuda es basura hasta el GC
  (manual vía comando, o el journal/daemon-idle de M3 lo barrerá); el GC de
  fase 4 es por edad y prefijo, sin saber si «pertenece» a una copia viva.
- `verify=Hash` relee `already` bytes de ambos lados: coste O(parcial) al
  reanudar — por eso es opt-in; `Length` es el default barato. El engine de
  fase 4 lo trata como `Length` (leer el parcial exige superficie nueva —
  digest del staging — que encaja con S3/ETags en fase 7): wire-completo,
  engine parcial (patrón `Ask` de M1), issue #35.
- `keep` añade un tercer estado terminal al sink (commit/abort/keep): la
  suite contractual crece y todo provider nuevo lo implementa (o hereda el
  default `keep=abort`).
- El nombre estable se deriva de los BYTES exactos del destino: en macOS
  (NFC vs NFD) o FS case-insensitive, dos formas del MISMO destino lógico
  dan hashes distintos → la reanudación se pierde (recopia desde cero,
  huérfano hasta el GC). Es el reverso SEGURO de H1 (jamás staging
  compartido); solo desperdicia la reanudación en ese borde. Derivar el
  hash del nombre normalizado queda como mejora (H4 del encoding-auditor).
