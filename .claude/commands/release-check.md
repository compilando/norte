---
description: Dry-run de release — semver, deny, changelog, docs, schema del protocolo
---
Ejecuta el checklist de release sin publicar nada:

1. `cargo semver-checks` (si está instalado; si no, avisa y continúa).
2. `cargo deny check` (licencias + advisories).
3. Changelog: verifica que release-plz derivaría entradas de los commits desde el último tag.
4. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`.
5. Regenera el JSON Schema del protocolo y diffea contra `norte-proto/schema/`;
   cualquier diff sin bump de versión de protocolo es BLOCKER.
6. `just ci` completo.
Informe final: GO / NO-GO con lista de bloqueos.
