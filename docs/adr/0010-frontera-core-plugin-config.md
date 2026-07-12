# 0010 — Frontera core/plugin/config para extensiones

- Estado: accepted
- Fecha: 2026-07-12
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §7 (sistema de plugins), §16.5 (RAR por delegación),
  plan M2 (`docs/notes/plan-m2.md`), kickoff M2 decisión FTP (fase 5bis).

## Contexto y problema

M2 multiplica los providers (sftp, object storage, archive, ftp) y en el
kickoff surgió la pregunta estructural: ¿qué merece ser crate del core y
qué debería ser plugin? Los mismos candidatos aparecen en el viewer
(¿mejoras tipo `bat` como plugin?). Sin un criterio explícito, cada
milestone re-litiga la frontera; con uno, la decisión por pieza es
mecánica. La spec ya define TRES niveles de extensión (§7): plugins WASM
sandboxed, scripting Lua y config declarativa (`openers.toml`) — la
pregunta es qué cae en cada cajón y por qué.

Restricciones que condicionan la respuesta:

- El plugin-host WASM (wasmtime + WIT) llega en **M4**; no existe en M2.
- Los plugins **jamás** tienen `exec` (§7.1): un plugin no puede invocar
  binarios externos; eso es exclusivo de openers declarativos de usuario.
- El criterio de salida de M2 ("sftp → S3 → zip local sin sorpresas")
  mide exactamente el data path: streaming, cancelación, resume, nombres
  hostiles en cada salto.
- Regla dura 10: secretos por keyring; threat model §14 (zip-bomb,
  servidor sftp hostil) exige código auditado en esas superficies.

## Opciones consideradas

### A. Providers remotos/archive como plugins desde el principio

- ＋ Aísla dependencias grandes (opendal, russh) fuera del árbol core.
- ＋ Valida la interfaz WIT `provider` con casos reales desde el día 1.
- － Bloquea M2 en infraestructura de M4 (el host no existe).
- － El data path cruzaría la frontera WASM: cada chunk de una copia
  pagaría serialización/copia; el criterio de salida mide ese camino.
- － Secretos (keyring) y límites anti zip-bomb en código de terceros o
  sandboxed-pero-no-auditado: superficie de ataque inaceptable.
- － Una API de plugins diseñada SIN implementaciones nativas maduras
  nace mal: no hay contra qué proyectarla.

### B. Providers de la cadena de salida en core; cola exótica como plugin; binarios externos como config

- ＋ M2 no depende de M4; el data path es nativo y auditado.
- ＋ La interfaz WIT `provider` de M4 se diseña como PROYECCIÓN de un
  trait `Provider` ya probado por 4-5 implementaciones nativas.
- ＋ La optionalidad de compilación ya la dan los feature-gates
  (backends de opendal; providers enteros si el binario pesa).
- ＋ Respeta la arquitectura de la spec: §7.1 reserva el nivel plugin
  para providers de terceros (WebDAV, GDrive, ERP interno).
- － Dependencias grandes entran al árbol (mitigado: feature-gates +
  cargo-deny).
- － La validación real de la interfaz WIT se pospone a M4 (mitigado:
  FTP como candidato de migración, ver decisión).

### C. Todo in-tree para siempre (sin nivel plugin para providers)

- ＋ Máxima simplicidad.
- － Contradice la spec (§7.1 promete providers de terceros).
- － El árbol acumula providers de nicho con sus deps y su mantenimiento.

## Decisión

**Opción B.** Criterio de asignación, en orden de comprobación:

1. **Core (crate `norte-vfs-*` / subsistema):** está en el data path del
   copy engine (streaming, cancelación, resume), toca secretos o
   superficie del threat model, lo exige el criterio de salida de un
   milestone, o hace falta para diseñar la propia API de plugins.
   → En M2: `norte-vfs-sftp`, `norte-vfs-object`, `norte-vfs-archive`,
   `norte-vfs-ftp` (fase 5bis, decisión de kickoff).
2. **Plugin WASM (M4):** presentación por mimetype (`previewer`),
   providers de cola larga vía WIT `provider`, `columns`, `hooks`,
   `command`. Nada con `exec`, nada con secretos propios fuera del
   keyring mediado por el host.
3. **Config declarativa (`openers.toml`):** integración con binarios
   externos (bat, delta, unrar, 7z…). Detección en runtime, degradación
   limpia («instala X»), jamás linkado ni empaquetado. Mismo patrón que
   la decisión RAR (spec §16.5).

Aplicaciones concretas decididas aquí:

- **Viewer + bat:** NO es plugin (un plugin no puede ejecutar binarios)
  ni es M2. Es entrada de `openers.toml` cuando exista (issue, M4). El
  syntax-highlight *dentro* del pane será plugin `previewer` con syntect
  compilado a WASM (issue, M4). El viewer builtin de M1 no se toca: la
  detección de encoding (§6.2) es competencia core que un delegado a bat
  rompería (asume UTF-8).
- **FTP:** entra in-tree en M2 (fase 5bis) porque el usuario lo necesita
  ya, y queda marcado como **candidato #1 a migrar a plugin-provider en
  M4** — es el provider menos "core" del lote (legacy, sin cifrado, dep
  aparte) y su migración será el dogfood que valide que un tercero puede
  escribir un provider sin tocar el core.

## Consecuencias

Positivas:

- M2 avanza sin dependencia de M4; frontera decidida una vez, aplicable
  mecánicamente a candidatos futuros (WebDAV → plugin; delta → opener).
- La API WIT `provider` de M4 nacerá proyectada desde un trait probado
  por local/mem/sftp/object/archive/ftp: seis implementaciones.
- El patrón delegación-a-binario queda unificado (bat, unrar, delta) en
  un solo mecanismo (`openers.toml`) con una sola política de seguridad.

Negativas / deuda asumida:

- russh + opendal + dep ftp entran al árbol con su coste de build y
  mantenimiento (vigilancia: feature-gates, cargo-deny, justificación
  por PR — regla 8).
- La migración FTP→plugin en M4 es trabajo doble asumido conscientemente
  (implementar in-tree ahora, proyectar a WIT después) a cambio de tener
  ftp en M2 y un dogfood realista en M4.
- Issues a abrir con hito M4: `openers.toml`, previewer syntax-highlight
  WASM, migración FTP→plugin. Sin issue, esta deuda no existe (regla de
  la casa).
