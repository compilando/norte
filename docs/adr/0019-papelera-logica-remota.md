# 0019 — Papelera lógica `.norte-trash/` para providers sin trash nativo

- Estado: accepted
- Fecha: 2026-07-15
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §5 («Papelera universal»), §14 (threat model);
  ADR 0009 (papelera: crate `trash`, capability `TRASH`, semántica B2 de
  no-degradación), 0013 (provider sftp), 0016 (provider object storage),
  0004 (tolerancia de versiones). Plan M2 fase 9. Extiende ADR 0009.

## Contexto y problema

ADR 0009 resolvió la papelera para el FS local (crate `trash` nativo:
freedesktop / Recycle Bin / macOS) y dejó APUNTADO que los providers
remotos —sin papelera del OS— usarían la «papelera lógica `.norte-trash/`»
de la spec, «para M2 con los remotos». M2 ya trajo sftp (0013) y object
storage (0016), ambos SIN declarar `TRASH`: un `DeleteMode::Trash` contra
ellos responde `Unsupported` y el frontend degrada a `Permanent` con aviso
(B2 de 0009). Eso cumple «jamás pérdida sorpresa», pero deja el peldaño S3
y el sftp del criterio de salida SIN recuperación — incumple la «papelera
universal» de la spec.

Preguntas a cerrar:

1. **Dónde vive** la papelera lógica y con qué layout en disco.
2. **Formato de metadatos** — respetando la regla dura 1 (nombres = bytes,
   jamás asumir UTF-8).
3. **Mecanismo de movimiento** por provider (sftp tiene rename atómico; S3
   no tiene rename — es copy+delete O(n)).
4. **Dónde vive el código** sin que los providers se conozcan entre sí
   (regla dura, ADR 0005) ni dupliquen lógica.
5. **Atomicidad y cancelación** del movimiento.
6. **Skew de versiones** ahora que remotos SÍ declaran `TRASH`.

## Decisión

### D1 — Layout: `.norte-trash/` en la raíz del provider

Un único directorio `.norte-trash/` colgando de la RAÍZ del provider
(`scheme://authority/.norte-trash/`), con dos subdirectorios:

```
.norte-trash/
  files/<id>          # el nodo movido (archivo o subárbol entero)
  meta/<id>.json      # procedencia para restaurar (M3)
```

Un solo espacio de nombres remoto (un `base` sftp, un bucket S3) = un solo
`.norte-trash/`; NO se replica el modelo freedesktop de *topdirs*
por-punto-de-montaje, que existe para el caso cross-device del FS local
(irrelevante en un backend remoto de namespace único). La raíz se deriva
del propio `VPath` destino (subir por `parent()` hasta la raíz), NO se pasa
como parámetro: el helper opera SIEMPRE en el mismo namespace del nodo que
borra.

`<id>` es determinista a partir del instante de borrado (`deleted_at`),
formateado a ancho fijo hex `{secs:016x}{nanos:08x}`; colisión (mismo
instante, o reintento) → se reintenta con sufijo `-{n}` apoyándose en el
`Conflict{Exists}` que `rename`/`write` garantizan (contrato
anti-sobrescritura, jamás pisa un `<id>` ocupado).

### D2 — Metadatos bytes-safe (NO espejo freedesktop)

El sidecar `meta/<id>.json` es formato PROPIO, no `.trashinfo` de XDG. El
`.trashinfo` de freedesktop guarda la ruta original como URL-encoding de
UTF-8: incompatible con la regla 1 (un nombre no-UTF8 no tiene forma
`.trashinfo` legal). La interop con papeleras Linux existentes ya la cubre
el crate `trash` en el provider LOCAL; un `.norte-trash/` remoto lo posee
norte y nadie más lo lee.

Contenido (JSON ASCII, sin dependencia nueva — se formatea a mano; ver
D4):

```json
{"v":1,"orig_hex":"<hex del VPath wire de origen>","deleted_at_ms":<u64>}
```

`orig_hex` = hex de los BYTES de `VPath::to_wire()` del origen (el wire ya
es ASCII percent-encoded y round-trips por `VPath::parse`; hex encima lo
hace trivialmente embebible en JSON sin escapes ni dependencia de base64).
Es la CLAVE de restauración de M3. `deleted_at_ms` = épocas del borrado.
El sidecar es procedencia FORWARD-LOOKING: fase 9 solo mueve; `list` /
`restore` / `purge` de la papelera lógica son M3 (igual que 0009 dejó
list/restore/purge del crate para M3).

### D3 — Mecanismo: `rename` (uniforme), no un camino por provider

Tanto sftp (0013) como object (0016) YA implementan `Provider::rename`:

- **sftp**: `rename` remoto = movimiento ATÓMICO del subárbol entero, una
  operación (coherente con la semántica «Trash = 1 op» de 0009).
- **object**: `rename` = `CopyObject` server-side + `DeleteObject`, y para
  un «directorio» (prefijo) copy-all LUEGO delete-all (0016): un fallo a
  mitad deja DUPLICADOS, jamás pérdida. No atómico, O(n).

Por tanto el movimiento a la papelera es `rename(target →
.norte-trash/files/<id>)` UNIFORME: el helper no distingue providers, y
cada backend aporta su propia semántica de `rename` ya auditada. Un
provider gana papelera lógica declarando `TRASH` y delegando su `trash()`
en el helper — sin conocer a los demás.

### D4 — El código: helper genérico en `norte-vfs`

`norte_vfs::trash::logical_trash<P: Provider + ?Sized>(provider, target,
now)` — función libre, genérica sobre el trait, en el crate del trait
(Apache/MIT). No introduce dependencias (el JSON del sidecar se formatea a
mano sobre un hex inline; `serde_json` NO entra en `norte-vfs`). Pasos:

1. **Guardas**: `target` raíz del provider → `InvalidPath`; `target` ya
   DENTRO de `.norte-trash/` (primer segmento `.norte-trash`) →
   `InvalidPath` (no se recicla la papelera; jamás no-op silencioso que
   engañe al caller sobre un borrado que no ocurrió).
2. **`mkdir`** de `.norte-trash/`, `files/`, `meta/` (idempotente:
   `Conflict{Exists}` = OK).
3. **Mover el dato PRIMERO**: `rename(target → files/<id>)`, reintentando
   `<id>-{n}` ante `Conflict{Exists}`. El paso destructivo (quitar el nodo
   de su sitio) se completa antes de tocar metadatos: un dato a salvo en
   `files/<id>` SIN sidecar sigue siendo recuperable (es el dato); un
   sidecar sin dato es inútil. Nunca al revés.
4. **Escribir el sidecar** `meta/<id>.json` (best-effort tras el punto en
   que el dato ya está a salvo).

### D5 — Atomicidad, cancelación y progreso

`Provider::trash` sigue siendo, para el engine, UNA operación sobre la raíz
(`entries_total = 1`, cancelable ANTES de disparar, no a mitad — 0009). En
sftp esto es literal (rename atómico). En object, el `rename` de un prefijo
grande es internamente copy+delete O(n) e INCANCELABLE a mitad: misma
naturaleza que la excepción freedesktop cross-device de 0009 (#26) —
documentada, no un fallo. El borrado Trash no reporta progreso granular por
diseño.

### D6 — Skew de versiones (recíproco N-1 revisado)

0009 avisó: «recíproco N-1: un cliente 0.2 contra core 0.3 en un provider
SIN `TRASH` pasa de "delete funciona (permanente)" a `Unsupported` duro —
revisar cuando lleguen remotos/archive en M2».

Revisión: al declarar `TRASH` en sftp y object, un `DeleteMode::Trash` (el
DEFAULT del wire, 0009) contra ellos ahora RECUPERA en vez de fallar o
borrar permanente — mejora estricta y segura. El cliente conforme sigue la
regla de 0009: condiciona `Permanent`/aviso a la AUSENCIA de la capability
`TRASH`, nunca a su versión de protocolo. `archive` es `READ_ONLY`: jamás
declara `TRASH` ni recibe deletes (falla en seguro). No hay cambio de wire
(la capability `TRASH` y `DeleteMode` existen desde 0.3.0): esta fase NO
bumpea el protocolo — solo hace que dos providers cumplan una capability ya
definida.

## Consecuencias

Positivas: la «papelera universal» de la spec cubre ya sftp y S3 — el
criterio de salida (sftp → S3 → zip) recupera en cada peldaño escribible;
un solo helper auditado, cero duplicación entre providers; el sidecar deja
el terreno listo para list/restore/purge de M3; el default seguro del wire
(Trash) por fin NO degrada en remotos.

Negativas / deuda: la papelera lógica CONSUME espacio en el remoto hasta
que M3 traiga `purge`/GC (un `.norte-trash/` que crece sin vaciarse); el
movimiento a papelera en object es O(n) e incancelable para prefijos
grandes (#26-análogo, documentado); el sidecar `meta/` es procedencia
forward-looking sin consumidor hasta M3 (se escribe pero aún no se lee).
GC de huérfanos de la propia papelera y política de retención → M3, issue
vinculada.

Excepciones de plataforma heredadas de 0009 (issues #25/#26) sin cambio:
Windows `FOF_NO_UI` puede DESTRUIR ítems no reciclables dentro del delete
nativo local; freedesktop cross-device degrada a copy+delete interno del
crate. Ninguna la toca esta fase (son del provider LOCAL); se re-anotan
aquí para trazabilidad del cierre del skew.
