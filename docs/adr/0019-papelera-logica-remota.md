# 0019 — Papelera lógica `.norte-trash/` en providers remotos

- Estado: accepted
- Fecha: 2026-07-14
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: ADR 0009 (papelera nativa), 0016 (object), 0013 (sftp),
  spec §5. Diseño: `docs/superpowers/specs/2026-07-14-papelera-logica-remota-design.md`.

## Contexto y problema

ADR 0009 entregó papelera nativa (crate `trash`) para local/Mem y dejó
la «papelera lógica `.norte-trash/`» de la spec §5 para M2 con los
remotos. sftp y object no tienen trash del OS: necesitan borrado
recuperable propio sin degradar en silencio a permanente.

## Decisión

- **Opt-in por conexión, default OFF** (`logical_trash: bool`,
  `#[serde(default)]` = false). Off → el provider NO declara
  `CapabilityFlags::TRASH` → el frontend cae en la degradación B2 de ADR
  0009 (aviso «PERMANENTE», reenvía `Permanent`). Evita el coste sorpresa
  de copiar en S3 al borrar.
- **Layout** en la raíz del provider: `.norte-trash/<id>/{<basename>,
  .norte-info}`. `<id>` = `<epoch_ms>-<counter>` (monótono por sesión).
  `.norte-info` guarda la ruta original como `VPath::to_wire()`
  (percent-encoded ASCII, lossless, line-safe) + `deleted-ms`.
- **Guard de restore (confused-deputy)**: un `.norte-info` en un
  share/bucket compartido es atacante-controlable. `trash::info_decode`
  exige un `expected_root` (la raíz de la conexión) y RECHAZA
  (`InvalidPath`) cualquier ruta con distinto scheme/authority → el
  restore (M3) jamás escribe el payload en otra conexión/host. El
  traversal (`.`/`..`/`%2F`/NUL) ya lo bloquea `VPath::parse`. La
  sobrescritura de un fichero existente DENTRO de la misma conexión es
  política del restore (confirmación reforzada, M3), no del parser.
- **Sin cambio de firma del trait**: `Provider::trash(&self, p)` intacto.
  El trait no recibe `CancellationToken` (modelo por drop, sin dep
  `tokio-util` en el crate fundacional; rule 8). La garantía de cero
  pérdida viene del orden **copiar-todo → borrar-todo** en object.
- **Relocalización por provider**: sftp = `create_dir` + `rename` +
  `write(info)` (un tiro, `entries_total = 1`). object/S3 = copy-all →
  delete-all (cancelable sin pérdida en cualquier punto).
- **Módulo compartido `norte-vfs::trash`** (puro, sin I/O): construcción
  de id/paths y encode/decode del `.norte-info`. Los providers no se
  conocen entre sí; solo conocen el trait + este módulo.

## Consecuencias

Positivas: borrado recuperable en remotos con layout estable (M3 restaura
leyendo `.norte-info`); sin dep nuevo; default seguro (sin sorpresas de
coste). Negativas / deuda: cancelación de grano fino a mitad del walk S3
sigue siendo drop-based (deuda junto a #51); crash a mitad de la fase de
borrado deja estado duplicado (origen parcial + copia completa en trash),
recuperable, coherente con la no-atomicidad de S3 (ADR 0016). Decomposición
en 9a (este módulo + ADR), 9b (sftp), 9c (object).
