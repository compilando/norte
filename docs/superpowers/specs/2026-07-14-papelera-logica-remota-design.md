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
- `.norte-info`: ruta original (**bytes crudos de VPath**, seguro
  no-UTF8) + timestamp de borrado (ms). Formato por líneas: `path:` con
  base64 de los bytes crudos (bytes-safe, sin adivinar encoding),
  `deleted_ms:` con el stamp.
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

### Firma del trait

`trash()` gana token de cancelación (cambio **vfs-interno, no wire**):

```rust
async fn trash(&self, p: &VPath, cancel: &CancellationToken) -> Result<(), Error>
```

Threading del token que `ops.rs` ya tiene en el punto de delete.
Actualiza `LocalProvider`/`MemProvider`/macro de contrato — `LocalProvider`
lo ignora (op del OS de un tiro, ADR 0009).

### Estrategia de relocalización por provider

- **sftp** (rename atómico-ish):
  `create_dir(.norte-trash/<id>)` → `rename(p → .norte-trash/<id>/<basename>)`
  → `write(.norte-info)`. Un rename mueve el árbol entero; token chequeado
  una vez antes de disparar. Cumple el contrato fast-trash de ADR 0009
  (`entries_total = 1`).

- **object/S3** (sin rename atómico → copy+delete largo). Orden crítico
  para cancelación segura: **copiar-todo primero, borrar-todo después.**
  - Fase A: walk del subárbol, copia cada key →
    `.norte-trash/<id>/<basename>/…`, escribe `.norte-info`. Token en el
    inner loop.
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

- **9a** — ADR (extiende 0009 o ADR nuevo) + módulo `norte-vfs::trash`
  (puro) + cambio de firma `trash()` con token + campo de config.
- **9b** — sftp logical trash + tests de contrato/cancelación.
- **9c** — object/S3 logical trash (copy-all/delete-all) + tests de
  cancelación.

## No-objetivos (YAGNI ahora)

- Vista/UI de papelera y restauración interactiva → M3.
- Purga/limpieza automática de `.norte-trash/` → M3.
- Ocultar `.norte-trash/` de listados → M3.
- Papelera lógica en archive (read-only) — nunca.
