# Fase 9 — Papelera lógica `.norte-trash/` (providers remotos)

- Fecha: 2026-07-14
- Estado: diseño aprobado (pendiente ADR + implementación)
- Relacionado: ADR 0009 (papelera nativa, M1), ADR 0016 (object storage),
  ADR 0013 (sftp), spec §5 «Papelera universal», rule dura 3 (cancelación)
  y 4 (journal).

## Contexto

M1 entregó papelera **nativa** (crate `trash`, `LocalProvider` +
`MemProvider`, `CapabilityFlags::TRASH`, `DeleteMode::{Trash,Permanent}`
con default `Trash`). Los providers remotos (sftp, object) y archive no
tienen papelera del OS. ADR 0009 dejó explícitamente la «papelera lógica
propia `.norte-trash/`» para M2 fase 9. Este documento la diseña.

Objetivo: que sftp y object puedan ofrecer borrado **recuperable** sin
papelera nativa, moviendo el árbol borrado a un directorio propio
`.norte-trash/` dentro de la conexión, con metadatos de restauración
para M3. `archive` sigue read-only → jamás declara TRASH.

## Decisiones tomadas (brainstorming)

1. **Alcance**: ADR + sftp + object, planificado junto, implementado en
   sub-bloques 9a/9b/9c.
2. **Opt-in por conexión, default OFF**: config `logical_trash: bool`
   (`#[serde(default)]` = false). Off → el provider NO declara TRASH →
   el frontend cae en la degradación existente (diálogo rojo
   «PERMANENTE», reenvía `Permanent`). Evita el coste sorpresa de copiar
   10 GB en S3 al borrar. Coherente con ADR 0009 «opcional por conexión».
3. **Metadatos de restauración desde ya**: cada ítem va a
   `.norte-trash/<id>/` con el payload + un `.norte-info` (ruta original
   en bytes crudos + timestamp de borrado). Congela un layout estable →
   M3 restaura leyendo `.norte-info`, sin migración rompedora.

## Arquitectura

### Capability y opt-in

Sin flag de wire nuevo. `CapabilityFlags::TRASH` se mantiene; sftp y
object lo declaran **solo si** la config de conexión activa
`logical_trash`. `archive` nunca. La papelera lógica es un *nuevo
respaldo* de la misma capability — la semántica B2 de ADR 0009 (el
engine JAMÁS degrada solo; el frontend consulta capability, avisa y
reenvía `Permanent`) queda intacta.

### Layout `.norte-trash/`

En la **raíz del provider** (por conexión). Por ítem borrado:

```
.norte-trash/
  <id>/
    <basename-original>      ← archivo/árbol movido, bytes preservados
    .norte-info              ← metadatos de restauración
```

- `<id>`: único, sin colisión, ordenable. Formato `<epoch_ms>-<counter>`
  (contador monótono por sesión evita colisión mismo-ms; sin dep `uuid`).
- `.norte-info`: ruta original + timestamp de borrado (ms). La ruta se
  guarda como su **forma wire** `VPath::to_wire()` — percent-encoded
  ASCII, sin controles ni saltos de línea (el codec escapa C0+DEL a
  `%XX`), losslessly round-trippeable vía `VPath::parse`. Line-safe → sin
  base64. Formato por líneas: cabecera de versión, `path: <wire>`,
  `deleted-ms: <u64>` (estricto: nada tras la 3.ª línea).
- **Guard confused-deputy** (hallazgo encoding-auditor, ALTA):
  `info_decode(bytes, expected_root)` ancla la ruta a la conexión de la
  papelera — rechaza scheme/authority distintos (un `.norte-info`
  envenenado en un share apuntaría el restore a otro host). Traversal ya
  lo corta `VPath::parse`. Sobrescritura intra-conexión = política de
  restore (M3).
- El wrapper `<id>/` aísla cada ítem → `.norte-info` no colisiona con el
  basename original ni entre ítems.
- Borrar `.norte-trash/` o algo ya dentro → ruta normal (permanente si
  el usuario re-borra). No se oculta `.norte-trash/` de los listados
  ahora; M3 da una vista de papelera propia.

### Módulo compartido `norte-vfs::trash`

Helpers **puros** (sin I/O), unit-testables:
- generación de `<id>` (a partir de un `now_ms` + counter que pasa el
  provider — nada de `Date::now` dentro).
- construcción del `VPath` del entry `.norte-trash/<id>/<basename>`.
- `info_encode(original: &VPath, deleted_ms) -> Vec<u8>` /
  `info_decode(&[u8]) -> Result<(VPath, u64)>`.

La **relocalización** la implementa cada provider (los providers no se
conocen entre sí; solo conocen el trait + este módulo del crate `vfs`
del que ambos dependen).

### Firma del trait (SIN cambio)

`trash(&self, p: &VPath) -> Result<(), Error>` se mantiene **tal cual**.
El trait `Provider` NO tiene ningún método que reciba
`CancellationToken` — su modelo de cancelación es **por drop** (p.ej.
`list_stream` cancela soltando el `BoxStream`, no sondeando un token) y
`norte-vfs` no depende de `tokio-util`. Añadir un token a `trash()`
rompería esa convención uniforme y metería un dep nuevo en el crate-trait
fundacional (rule 8). Descartado en planificación.

La garantía de cero pérdida de datos NO viene de un token: viene del
**orden copiar-todo → borrar-todo** (ver abajo). Drop/abort en cualquier
punto deja el origen intacto o toda key borrada ya respaldada. La
cancelación de grano fino a mitad del walk S3 sigue el mismo modelo que
las ops largas existentes de object (rename = copy+delete O(n), cuya
cancelación mid-op es deuda ya registrada, **#51**) → deuda junto a #51,
no bloqueante.

### Estrategia de relocalización por provider

- **sftp** (rename atómico-ish):
  `create_dir(.norte-trash/<id>)` → `rename(p → .norte-trash/<id>/<basename>)`
  → `write(.norte-info)`. Un rename mueve el árbol entero. Cumple el
  contrato fast-trash de ADR 0009 (`entries_total = 1`).

- **object/S3** (sin rename atómico → copy+delete largo). Orden crítico
  para cancelación segura: **copiar-todo primero, borrar-todo después.**
  - Fase A: walk del subárbol, copia cada key →
    `.norte-trash/<id>/<basename>/…`, escribe `.norte-info`.
  - Fase B: borra cada key original.
  - **Cancel en Fase A** → ningún original borrado → origen intacto;
    trash con copias huérfanas bajo `<id>/` (basura limpiable, jamás
    origen a medias). **Cancel en Fase B** → borrado parcial, pero cada
    key borrada ya tiene su copia en trash → recuperable. En ambos casos:
    **cero pérdida de datos.** Limitación documentada: crash a mitad de
    Fase B deja estado duplicado (origen parcial + copia completa en
    trash) — recuperable, coherente con la no-atomicidad de S3 ya
    aceptada para move/rename (ADR 0016).

## Errores · journal · config

- **Config**: config de conexión gana `logical_trash: bool`
  (`#[serde(default)]` = false). El constructor del provider lo guarda →
  gate de `capabilities()` (bit TRASH) y de `trash()`. Llamado con la
  opción off → `Error::Unsupported` (defensa en profundidad; el engine no
  lo llamará sin la cap).
- **Errores**: `trash()` propaga errores tipados (`thiserror`). Cancel a
  mitad → variante de cancelación existente. Colisión de `<id>`
  (imposible dentro de una sesión; posible cross-sesión en bucket
  compartido) → detecta `<id>` existente, incrementa counter / reintenta
  una vez.
- **Journal (rule 4)**: trash es mutación → entrada `Removed` como hoy;
  el `.norte-trash/<id>/` + `.norte-info` ES la pre-imagen recuperable.
  M3 restaura leyendo `.norte-info` → `rename`/`copy` de vuelta. Sin tipo
  de journal nuevo ahora.

## Testing

- Helpers puros: unit + roundtrip hostil (`info_encode`/`decode` sobre
  nombres no-UTF8) → fixture al corpus de `norte-testkit`.
- sftp/object: contract-level — trashear un ítem (incl. nombre hostil +
  árbol anidado) → fuera del origen, presente bajo `.norte-trash/<id>/`,
  `.norte-info` decodifica a la ruta original.
- **Cancelación** (rule 3): cancel de trash S3 a mitad de Fase A →
  origen intacto. Cancel a mitad de Fase B → toda key borrada recuperable
  desde trash.
- Opt-in: `logical_trash=false` → sin cap TRASH → `trash()` =
  `Unsupported`.

## Decomposición (PRs < 400 líneas, un propósito c/u)

- **9a** — ADR nuevo (0019) + módulo `norte-vfs::trash` (puro: `trash_id`,
  `plan` de paths del entry, `info_encode`/`info_decode` sobre
  `to_wire`). SIN base64, SIN cambio de firma del trait, SIN config
  todavía (el campo `logical_trash` entra por provider en 9b/9c).
- **9b** — sftp logical trash + tests de contrato/cancelación.
- **9c** — object/S3 logical trash (copy-all/delete-all) + tests de
  cancelación.

## No-objetivos (YAGNI ahora)

- Vista/UI de papelera y restauración interactiva → M3.
- Purga/limpieza automática de `.norte-trash/` → M3.
- Ocultar `.norte-trash/` de listados → M3.
- Papelera lógica en archive (read-only) — nunca.
