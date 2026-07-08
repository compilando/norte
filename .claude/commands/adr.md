---
description: Crea un ADR nuevo en docs/adr/ con formato MADR y numeración secuencial
argument-hint: <título de la decisión>
---
Crea un ADR para: $ARGUMENTS

1. Lista `docs/adr/` y toma el siguiente número secuencial (NNNN, 4 dígitos).
2. Crea `docs/adr/NNNN-<slug-kebab>.md` con formato MADR:
   - Título, estado (proposed/accepted/superseded), fecha, decisores.
   - Contexto y problema.
   - Opciones consideradas (mínimo 2, con pros/contras).
   - Decisión y justificación.
   - Consecuencias (positivas y negativas).
3. Añade la entrada al índice `docs/adr/README.md` (créalo si no existe).
4. Deja el cambio staged con mensaje preparado: `docs(adr): NNNN <título>`.
   No commitees sin confirmación.
