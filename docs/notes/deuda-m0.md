# Deuda registrada durante M0 (convertir en issues al cierre)

- **testkit: eje de normalización NFC/NFD en MemProvider** (encoding-auditor
  fase 6, M4): APFS trata é NFC y NFD como el mismo archivo; MemProvider no
  puede simularlo (knob futuro o CapabilityFlag). Bloquea testear la trampa
  nº 1 de CLAUDE.md contra el testkit antes del engine de colisiones.
- **testkit: estrategia `arb_case_variant`** (encoding-auditor fase 6, B4):
  generador de variantes de caja ASCII de un nombre para proptests de
  colisión en destino case-insensitive.
- **testkit: fixtures de detector** (encoding-auditor fase 6, M6 opcionales):
  UTF-16LE sin BOM + fixture binaria con NUL cuando llegue el detector (M1+).
- **vfs-local: case-rename en Windows** (encoding-auditor fase 8, A2): sin
  identidad real de archivo en std (VolumeSerial+FileIndex vía windows-sys),
  M0 devuelve Conflict al case-rename en Windows. M1: dep windows-sys
  justificada o rename no-replace (MoveFileExW sin REPLACE_EXISTING).
- **vfs-local: rename/commit no-replace** (fase 8, M7): TOCTOU check→rename
  aceptado en M0; M1: renameat2(RENAME_NOREPLACE) linux, renamex_np macOS,
  MoveFileExW windows. Y mapear ErrorKind::CrossesDevices (EXDEV) en map_io
  cuando el move cross-device del core lo necesite.
- **vfs-local: nombres cerca de NAME_MAX** (fase 8, M3): el sufijo de staging
  alarga el nombre; con 242–255 bytes el partial da ENAMETOOLONG. Staging con
  nombre por hash + fixture name_max_255 en el corpus.
- **vfs-local: sondeo de capabilities robusto y lazy** (fase 8, M4): sufijo
  aleatorio por intento + comparación (dev,ino), APIs de plataforma (pathconf
  _PC_CASE_SENSITIVE, FileCaseSensitiveInformation por-dir en NTFS), sondear
  en primera escritura, no en el constructor.
- **vfs-local: GC de .norte-partial huérfanos** (fase 8, A1): kill -9 deja
  staging huérfano con sufijo único; el journal (M3) debe barrerlos al
  arrancar (replay) — ya no se pisan por nombre.
- **vfs-local: test de cancelación de streams** (fase 8, B7): drop del stream
  a mitad → el productor suelta el fd (contar /proc/self/fd en linux,
  TempDir::close en windows). Encaja con la matrix de fase 12.
- **proto: ConflictKind para colisión por normalización** (fase 8, B4): en
  macOS una colisión NFD/NFC sale como CaseCollision; variante nueva = cambio
  de wire (golden + bump + ADR) — decidir en M1 con el engine de colisiones.
