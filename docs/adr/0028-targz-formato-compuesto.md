# 0028 — tar.gz como formato compuesto `tar+gz` (capa de compresión opaca)

- Estado: accepted
- Fecha: 2026-07-21
- Decisores: oscar (dirección), Claude (propuesta)
- Relacionado: ADR 0018 (provider archive, difirió tar.gz en «Decisión»), issue #55, issue #56 (anidamiento, sigue diferido)

## Contexto y problema

El ADR 0018 dejó tar PLANO: `Locator::Tar{offset,size}` hace passthrough por
rangos al provider interior porque los datos viven contiguos y sin comprimir
en el contenedor. gzip rompe las dos premisas: no es seekable (descompresión
secuencial desde el inicio) y los offsets del contenedor no se corresponden
con los del contenido. A la vez, `.tar.gz`/`.tgz` es el tar más común del
mundo real — sin él, el provider archive cojea donde más se usa.

La gramática de schemes compuestos del ADR 0018 (whitelist, no sintaxis;
marcador `!`) admite `tar+gz+file://…` sin migración de wire, pero hoy el
parseo (`scheme_format_prefix` = primer token antes de `+`) interpretaría
`tar+gz+file` como formato `tar` sobre un interior `gz+file` huérfano.

## Opciones consideradas

### A — Token compuesto `tar+gz` en la whitelist (ELEGIDA)

`ARCHIVE_FORMATS = ["zip", "tar", "tar+gz"]`; `scheme_format_prefix` pasa a
longest-match. La capa gz es OPACA dentro del formato: un solo split, un solo
marcador `!`, un provider (`Format::TarGz`).

- Pro: cambio mínimo de proto (una entrada de whitelist + longest-match);
  la reserva normativa del 0018 ya cubre (`tar+gz+` empieza por `tar+`);
  cero impacto en zip/tar existentes.
- Pro: el frontend solo necesita mapear extensiones (`.tgz`, `.tar.gz`).
- Contra: no generaliza (cada combinación futura — `tar+zst`, `tar+bz2` —
  es una entrada nueva de whitelist). Aceptado: son pocas y explícitas.

### B — Mecanismo general de capas de compresión

Gramática `<formato>+<capa>*+<scheme>` con resolución por capas.

- Pro: extensible sin tocar la whitelist por cada combinación.
- Contra: es el problema del anidamiento (#56) con otro nombre — exige
  redefinir `archive_split` por capas y `ArchiveRef` deja de ser un par
  formato/interior. Sobredimensionado para UNA capa real hoy.

### C — Spool a disco del tar descomprimido

Descomprimir una vez a un fichero temporal y servir el tar plano desde ahí.

- Pro: reads O(1) tras el spool; reutiliza el path tar existente.
- Contra: presupuesto de disco, ciclo de vida del temporal (limpieza,
  multi-sesión), y el spool COMPLETO castiga el caso común (listar y leer
  un archivo pequeño). Diferida como OPTIMIZACIÓN de reads calientes
  (issue de deuda), no como base.

## Decisión

Opción A, con esta semántica:

1. **Proto**: `tar+gz` entra en `ARCHIVE_FORMATS`; `scheme_format_prefix`
   resuelve por longest-match. `archive_compose`/`archive_split` no cambian
   de forma; la guardia de anidamiento sigue rechazando interiores
   compuestos (`tar+gz+zip+file` → Err). Se expone `scheme_archive_format`
   público para que los frontends no dupliquen la gramática.
2. **Índice secuencial**: `tar::Archive::entries()` (solo `Read`) sobre
   `flate2::read::MultiGzDecoder` (miembros gzip concatenados existen).
   `Locator::Gz{offset,size}` con offsets del stream DESCOMPRIMIDO. La
   validación por `container_len` del tar plano no aplica; el truncamiento
   se detecta fail-loud en índice o read (jamás datos cortos en silencio).
3. **Read forward-decode**: decoder fresco por lectura, descarte hasta el
   offset (cancelable: se chequea el cierre del canal por chunk) y entrega
   en chunks por canal acotado (drop del stream = corte, regla 3). Coste
   O(descomprimido-hasta-offset) por read: documentado y aceptado en v1.
4. **Anti-bomba**: `Limits.max_decompressed_bytes` (default 64 GiB) acota
   el TOTAL descomprimido del pase de índice — una gzip bomb es CPU
   infinita aunque la memoria sea streaming. El read queda acotado por el
   tamaño de la propia entrada.
5. **Dependencia**: `flate2` pasa a dep directa de `norte-vfs-archive` (ya
   transitiva vía `zip`/`russh`, mismo backend zlib-rs, licencias ya en la
   allowlist de deny).

## Consecuencias

- Positivas: el tar más común del mundo real se navega/lee con la misma UX
  que zip/tar; sin migración de wire (la gramática del 0018 ya lo admitía);
  superficie de proto mínima y revisada (longest-match + helper público).
- Negativas: reads de colas de tgz grandes son O(n) — mitigación futura en
  la issue de spool/restart-points; cada formato comprimido futuro añade
  entrada de whitelist (aceptado hasta que #56 justifique la opción B).
- Neutras: el índice tar.gz no participa del caché de CD de zip (#61); el
  caché LRU por generación (mtime+size del contenedor) aplica igual.
