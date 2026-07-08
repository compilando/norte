---
description: Scaffolding de un provider VFS nuevo con suite contractual enganchada
argument-hint: <scheme, p.ej. sftp>
---
Crea el provider VFS para scheme `$ARGUMENTS`:

1. Crate `crates/norte-vfs-$ARGUMENTS` (usa el scaffolding de /new-crate; licencia Apache-2.0 OR MIT).
2. Impl del trait `Provider` con `todo!()` documentados por método.
3. Declaración de `Capabilities` honesta (empieza conservador).
4. Test de contrato ya enganchado: `norte_vfs::provider_contract! { name: $ARGUMENTS, setup: ... }`.
5. Checklist de semántica en el rustdoc del crate, a rellenar antes del primer release:
   symlinks, case-sensitivity, rename atómico, trash, paths máximos, encoding de nombres.
6. Recuerda: los providers NO se conocen entre sí; nada de deps a otros providers.
