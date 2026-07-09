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
