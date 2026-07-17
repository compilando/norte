# 0025 — Anclaje HMAC del head del journal + audit export (M3-5)

- Estado: accepted
- Fecha: 2026-07-17
- Decisores: oscar, Claude
- Issues: #63 (tamper-evidence real), cierre de M3-5

## Contexto y problema

El journal (ADR 0023) lleva un hash-chain SHA-256 sin clave: detecta
corrupción y ediciones INGENUAS (que no recomputan la cadena), pero un
atacante con acceso de ESCRITURA a la DB pasa `verify_chain` con reescritura
total, truncación de cola o rollback/splice (hallazgo A1 del
security-reviewer en M3-1a). M3-5 exige además exportar el journal como
material de auditoría (CSV/JSONL) y que `verify_chain` reporte DÓNDE se
rompió la cadena (B2).

Threat model (spec §14): mismo uid, atacante = proceso que puede escribir la
DB pero que NO debería poder fabricar historia sin dejar rastro. La defensa
perfecta (WORM, notarización externa) queda fuera del alcance de un daemon
local sin infraestructura.

## Opciones consideradas

### A — Firma asimétrica del head (ed25519 en keyring)

- ✅ No repudio fuerte; verificable sin el secreto.
- ❌ Gestión de par de claves + rotación; `ring`/`ed25519-dalek` = dep
  estructural pesada; el "verificador sin secreto" no existe en un daemon
  mono-usuario (el mismo uid tiene el keyring).

### B — Anclas HMAC-SHA256 del head con clave en keyring (elegida)

Un fichero `journal-anchors.jsonl` APPEND-ONLY junto a la DB: cada línea
`{seq, head_hex, mac_hex}` donde `mac = HMAC-SHA256(key, "norte-anchor-v1" || seq_le || head)`
(context string = separación de dominio + versión del formato persistido;
golden test pinnea la línea exacta).
La clave vive en el keyring del SO (servicio `norte`, entrada
`journal-anchor`), se crea en el primer anclaje y JAMÁS toca disco plano
(regla 10). `audit verify` recomputa la cadena, reporta la primera rotura
(B2) y valida cada ancla contra la cadena actual.

- ✅ hmac+sha2 (RustCrypto, ya usamos sha2) = dep mínima; keyring ya está en
  el árbol (norte-connect, ADR 0015 C).
- ✅ Ventana de fabricación acotada: reescribir historia SIN la clave del
  keyring invalida las anclas; truncar la cola por detrás del último ancla
  se detecta (el ancla apunta a un seq/hash que ya no existe o no casa).
- ❌ NO es tamper-proof: un atacante con acceso al keyring (mismo uid, con
  sesión desbloqueada) puede re-anclar. Y SIN la clave puede atacar el
  propio fichero de anclas (mismo dir, mismo owner): borrarlo, recortarle
  las líneas posteriores a un rollback (las anteriores siguen siendo MACs
  válidos) o restaurar un snapshot coherente del PAR DB+anclas. Por eso
  `verify` trata la ausencia de anclas como FALLO (salvo
  `--allow-no-anchors`) e imprime SIEMPRE la cobertura (seq máximo anclado
  vs head), y `anchor` emite la línea por stdout para copiarla FUERA: la
  copia externa es lo que convierte recorte/rollback en detectables. El
  override `NORTE_ANCHOR_KEY` (env, para headless) anula la garantía
  frente a same-uid (environ legible): solo para entornos controlados.
- ❌ Anclar es un acto explícito (CLI/cron), no automático por entrada: entre
  anclas hay ventana sin cobertura (documentado; el usuario elige cadencia).

### C — Notarización externa / WORM

- ✅ Tamper-evidence real contra same-uid.
- ❌ Exige infraestructura (servidor remoto, papel, TPM…) que un file manager
  local no puede presuponer. Queda como extensión futura sobre el MISMO
  fichero de anclas (subir su hash a donde sea).

## Decisión

**B.** Módulo `norte-core::journal::audit`: `ChainStatus` (Intact/Broken con
`first_bad_seq`), `export_jsonl`/`export_csv` deterministas sobre
`entries()`, `Anchors` (append + verify con clave inyectada como bytes — el
core NO conoce el keyring; la clave la resuelve el CLI vía `norte-connect`).
CLI `norte audit verify|export|anchor` abre la DB en SOLO-LECTURA. OJO: el
daemon abre con `locking_mode=EXCLUSIVE` de SQLite (single-writer mecánico,
M3-4), así que el audit contra un daemon VIVO falla con `database is
locked` — se corre con el daemon parado (mensaje accionable en el CLI).
Alternativa futura si molesta: audit por el wire (método paginado) o
relajar a flock advisory propio.

## Consecuencias

- ✅ Cierra #63 con una garantía honesta y documentada; B2 resuelto
  (`ChainStatus::Broken { first_bad_seq }`).
- ✅ El export da material de auditoría estable (JSONL para máquinas, CSV
  para humanos) sin pasar por el wire (cero cambio de protocolo).
- ✅ M3-5 completa → M3 cerrado.
- ➖ Dep nueva `hmac` en norte-core (RustCrypto, hermana de sha2 ya presente;
  ~sin código propio, mantenimiento activo).
- ➖ El claim del módulo journal pasa de «detección de ediciones ingenuas» a
  «+ anclas HMAC con clave en keyring»; el rustdoc debe enumerar QUÉ sigue
  sin cubrir (atacante con keyring, ventana entre anclas, copia externa
  recomendada del fichero de anclas).
- ➖ `verify_chain() -> bool` cambia a `ChainStatus` (API interna del core;
  call-sites de tests migran).
