# Prompt de arranque — M2 (remotos + archivos)

> Uso: sesión NUEVA de Claude Code en la raíz del repo. Pegar el bloque
> de abajo tal cual. Requisitos previos: M1 completo y CI verde (hecho el
> 2026-07-12), plan en `docs/notes/plan-m2.md`.

---

Continúa con **norte**: arranca el **hito M2** (spec §15, "remotos +
archivos"). Antes de escribir código:

1. Lee `docs/notes/plan-m2.md` (el plan de 10 fases), `CLAUDE.md`,
   `ARCHITECTURE.md` y los ADRs 0004/0005/0009 (wire, provider ancho,
   trash — los tres condicionan M2).
2. Resuelve conmigo las **4 decisiones abiertas** del plan (criterio de
   salida vs archive read-only, FTP plano, #27/paginación, MessagePack).
   Una pregunta cada una, con tu recomendación.
3. Con las decisiones cerradas, ejecuta el plan fase a fase empezando
   por la **fase 1 (deuda dura del engine: issues #16–#19)**.

## Método (el que funcionó en M0/M1 — no lo cambies)

- Una fase = un commit convencional (mensaje en español) con `just ci`
  verde local + push + CI de GitHub verde antes de pasar a la siguiente.
- Test-first: la matriz de tests antes de la implementación; todo bug de
  encoding/paths mete su fixture al corpus de `norte-testkit` ANTES del
  fix.
- Reviewers como subagentes en background sobre el diff, antes de cada
  commit sustancial: `rust-reviewer` siempre; `protocol-guardian` si
  tocas `norte-proto` o handlers; `encoding-auditor` si tocas
  vfs/nombres/archive; `security-reviewer` en daemon, auth de socket,
  keyring y `connections.toml` (fases 2, 3 y 6 como mínimo). Aplica
  hallazgos válidos, razona los que no.
- Verificación E2E real por fase: pty (`script -qec` + teclas por stdin)
  para lo interactivo; para remotos, contenedor real (openssh/MinIO) al
  menos una vez en local aunque el job de CI sea nightly.
- Cambios de wire: golden actualizado + bump de `PROTOCOL_VERSION` +
  ADR + revisión de protocol-guardian. Sin excepciones.
- Deuda: issue de GitHub con hito, o no existe. `TODO` sin issue no pasa.
- ADR nuevo (slash command `/adr`) para: transporte/daemon (fase 2),
  resume (fase 4), sftp (fase 5), secretos/keyring (fase 6), object
  storage (fase 7), archive (fase 8), papelera remota (fase 9).
- Crates nuevos con `/new-crate`: `norte-vfs-sftp`, `norte-vfs-object`,
  `norte-vfs-archive` — providers = MIT/Apache, y los providers NO se
  conocen entre sí.
- Al cerrar cada fase: actualiza la memoria persistente (estado +
  trampas nuevas).

## Fuera de alcance en M2 (recházame si te lo pido)

MCP/policy/journal-undo (M3), plugins WASM/Lua/IA (M4), GUI (M5),
escritura dentro de archives y RAR (M2+, vía decisión 1), índice/search.

## Criterio de salida

Literal de la spec: **"copiar de sftp a zip local vía S3 sin
sorpresas"** — la fase 10 lo verifica de punta a punta con contenedores
reales: colisiones, cancelación limpia, resume y nombres hostiles
correctos en cada salto de la cadena.
