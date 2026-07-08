---
description: Añade una fixture hostil al corpus canónico de norte-testkit
argument-hint: <descripción del caso, p.ej. "nombre con surrogate sin parear">
---
Añade al corpus de `norte-testkit` una fixture para: $ARGUMENTS

1. Genera los bytes exactos (en `fixtures/names.toml` como hex si es nombre;
   en `fixtures/content/` como fichero binario si es contenido).
2. Documenta: qué caso real representa, en qué OS/provider aparece, y qué
   bug prevendría (referencia issue si existe).
3. Añádela a `fixtures::hostile_names()` / `fixtures::content_corpus()` según toque.
4. Comprueba que la suite contractual la recoge (el roundtrip nuevo corre en Mem y local).
5. Regla del proyecto: todo bug de encoding/paths entra aquí ANTES del fix (test-first).
