# 0005 — Ensanchado del contrato Provider y políticas del copy engine

- Estado: accepted
- Fecha: 2026-07-11
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: plan M1 fase 2 (`docs/notes/plan-m1.md`), issues #6, #7, #8;
  ADR 0004 (convenciones wire).

## Contexto y problema

M2 trae providers remotos (sftp, object storage). Cada método nuevo del trait
`Provider` rompe a TODO implementador, así que el trait debe ensancharse
ANTES de que existan más implementadores (hoy: `LocalProvider`,
`MemProvider`). A la vez, el copy engine necesita las políticas que la spec
exige (§5: colisiones ask/overwrite/skip/rename-auto/newer; §17.9: symlinks
follow/preserve/skip; reintentos con backoff) y que la fase 5 (diálogos del
TUI) consumirá.

Tres frentes con decisión de diseño:

1. **Qué entra en el trait ahora** (y qué forma tiene).
2. **Qué semántica exacta tienen las políticas** del engine en M1 (headless,
   sin diálogo posible todavía).
3. **Qué cambia en el wire** (y por tanto exige golden + bump + esta ADR).

## Opciones consideradas

### A. Forma del read con rango

- **A1 — método nuevo `read_range()` junto a `read()`**: no rompe
  implementadores. Contra: dos métodos para lo mismo para siempre; el
  default "no soportado" esconde providers que no lo implementan hasta el
  primer resume fallido en producción.
- **A2 — cambiar la firma: `read(&self, p, range: Option<ByteRange>)`**
  (spec §5 literal). Rompe implementadores HOY, que es exactamente cuando
  hay dos y los mantenemos nosotros. `None` = archivo completo.

### B. API de symlinks

- **B1 — solo `symlink()` de creación**: insuficiente — la política
  `Preserve` del copy necesita LEER el destino del link en el origen
  (`read_link`) además de crearlo en el destino.
- **B2 — `read_link()` + `symlink()` con destino en BYTES crudos**: el
  destino de un symlink es una cadena de bytes arbitraria (relativa,
  absoluta, rota, no-UTF8) — regla dura 1: jamás `String`, y tampoco
  `VPath` (un target relativo `../x` no es un VPath válido y NO debe
  resolverse). Windows necesita saber si el link es a archivo o a dir
  (`CreateSymbolicLinkW` distingue): parámetro `kind: SymlinkKind`.

### C. Política de colisiones en el engine M1

- **C1 — implementar `Ask` con pausa de Task ya**: exige canal
  pregunta/respuesta en el modelo de Task (pausa, timeout, multiplexado).
  Sobredimensionado sin TUI que pregunte.
- **C2 — wire completo, engine parcial**: el enum `CollisionPolicy` viaja
  completo por el wire (Ask incluido, para que el protocolo no cambie en
  fase 5), pero el engine M1 trata `Ask` como `Fail` (Conflict → el caller
  decide y reintenta). La resolución interactiva por archivo llega en fase
  5 con el mecanismo de pausa.

### D. Overwrite atómico vs remove+write

- **D1 — replace atómico vía `WriteOpts` en el trait**: correcto a largo
  plazo, pero exige otro parámetro de trait y semántica por-provider que
  los remotos de M2 informarán mejor.
- **D2 — `Overwrite` = `remove()` + write normal**: dos mutaciones
  observadas por el journal (Removed + Created), reversibles una a una.
  Ventana no atómica documentada. `WriteOpts` queda para M2.

## Decisión

- **A2**: `read(&self, p: &VPath, range: Option<ByteRange>)`.
  `ByteRange { offset: u64, len: Option<u64> }` (tipo de proto, serde;
  `len: None` = hasta EOF). Lo exige el resume de M2 (`.norte-partial` +
  offset, spec §5) y el viewer (fase 7, lectura parcial de archivos
  grandes). Providers que no saben hacer rango: `Unsupported` (los dos
  actuales sí saben).
- **B2**: `read_link(&self, p) -> Result<Vec<u8>>` y
  `symlink(&self, link: &VPath, target: &[u8], kind: SymlinkKind)`.
  `SymlinkKind { File, Dir }`; unix lo ignora, Windows elige la llamada.
  Providers sin symlinks (object storage M2): `Unsupported` + sin flag
  `SYMLINKS` en capabilities.
- **C2**: `CollisionPolicy { Fail, Ask, Skip, Overwrite, RenameAuto,
  Newer }` en el wire (params opcionales de `fs.copy`/`fs.move`, default
  `Fail` — campo opcional nuevo = compatible, ADR 0004). Engine M1:
  - `Fail`/`Ask`: Conflict (comportamiento actual). `Ask` de verdad, fase 5.
  - `Skip`: la entrada en conflicto no se copia; cuenta en el progreso como
    saltada; la task termina `Completed`.
  - `Overwrite`: **D2** (remove + write; jamás sobre un dir destino con
    un archivo origen o viceversa: eso sigue siendo Conflict TypeMismatch).
  - `RenameAuto`: sufijo ` (n)` ANTES de la última extensión del último
    segmento (split en el último byte `.` que no sea el primero; sin `.`,
    sufijo al final). n = 1..=1000, byte-safe; agotado → Conflict.
  - `Newer`: overwrite si `mtime_src > mtime_dst`; skip si no; cualquiera
    de los dos sin mtime comparable → Conflict (jamás adivinar).
- **SymlinkPolicy { Follow, Preserve, Skip }**, default `Preserve` (lo que
  hace `cp -a`; cero sorpresas de ciclos). M1:
  - `Preserve`: `read_link` en origen + `symlink` en destino. Si el destino
    no declara `SYMLINKS`: `Unsupported` (el usuario elige Skip o Follow).
  - `Skip`: los symlinks no se copian (contados como saltados).
  - `Follow`: symlink a ARCHIVO se copia como su contenido (el `read()`
    del provider sigue el link); symlink a DIRECTORIO → `Unsupported` en
    M1 (seguir dirs exige visited set por (dev,ino) contra ciclos, spec
    §17.9 — issue aparte para M2).
- **Reintentos en el engine**: toda operación de provider dentro de una
  Task reintenta ante `ProviderUnavailable { retryable: true }` e
  `Io { retryable: true }`: 3 reintentos, backoff determinista
  100 ms · 2^n, chequeando cancelación DURANTE la espera. Un archivo a
  medias se reinicia entero (resume con offset llega en M2 sobre el rango
  de A2). Otros errores: jamás se reintentan.
- **`ConflictKind::Normalization`** (issue #8): colisión donde los bytes
  difieren pero la forma normalizada (NFC) coincide — lo que produce macOS
  NFD contra un origen NFC. Además `ConflictKind` gana fallback
  `#[serde(other)] Unknown` oculto (mismo patrón que `Error::Unknown`,
  ADR 0004) para que la PRÓXIMA variante no rompa a clientes N-1.
- **Capabilities nuevas**: `APPEND` (1<<5), `RANDOM_WRITE` (1<<6) — las
  consultará el resume/verificación de M2; `LocalProvider` las declara ya.
- **`PROTOCOL_VERSION` 0.1.0 → 0.2.0** (variante nueva en `ConflictKind`,
  flags nuevos, tipos nuevos). Golden fixtures nuevas para cada tipo.
  Coexistencia: 0.1.0 se RETIRA aquí — no existen clientes remotos hasta el
  daemon (M2); **0.2.0 es la base de la garantía N/N-1** de la spec §11.
  (Un cliente 0.1.0 hipotético reventaría al deserializar `normalization`:
  su `ConflictKind` no tenía fallback.)

## Consecuencias

Positivas:

- M2 implementa providers contra un trait estable; el resume y el viewer
  tienen el rango que necesitan; la fase 5 del TUI solo añade la UI de
  `Ask` sin tocar wire ni engine.
- El journal (M3) ve `Overwrite` como Removed+Created reversibles.
- `ConflictKind` queda a prueba de variantes futuras (fallback Unknown).

Negativas / deuda asumida:

- `read` con rango rompe la firma: los dos providers y todos los tests se
  tocan ahora (asumido: es el momento más barato).
- `Overwrite` no es atómico (ventana remove→write); un crash entre medias
  deja el destino borrado sin reemplazo — el journal M3 lo revierte;
  `WriteOpts` con replace atómico queda para M2.
- `Follow` sobre dir-symlinks queda `Unsupported` hasta el visited set
  (issue nueva, M2).
- `RenameAuto` con 1000 intentos hace hasta 1000 stats en el peor caso
  (aceptable: caso patológico), y sobre un nombre ya en `NAME_MAX` el
  candidato ` (n)` excede el límite → `InvalidPath` fail-loud (acortar el
  stem exige truncado byte-safe multibyte: M2 si duele).
- El guard anti "sobrescribirse a sí mismo" (copy/move con Overwrite) es
  conservador: byte-igual siempre, variante de caja vía `to_lowercase` en
  destinos case-insensitive, y claves reales ecoadas por el provider. Los
  pares que el FS pliegue más ancho quedan expuestos hasta la identidad
  real ((dev,ino)/FileId) — issue de M2.
- Los reintentos solo envuelven operaciones IDEMPOTENTES (stat/read/
  read_link) y el reinicio de archivo completo; reintentar mutaciones cuyo
  efecto pudo aplicarse (timeout post-commit en remotos) duplicaría efectos
  o perdería journal — mapear esa ambigüedad por operación es issue de M2.
