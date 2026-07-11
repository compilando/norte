# 0009 — Papelera: crate `trash`, capability y degradación explícita

- Estado: accepted
- Fecha: 2026-07-11
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §5 («Papelera universal»), plan M1 fase 8, ADR 0005.

## Contexto y problema

La spec exige trash nativo donde exista (freedesktop, Recycle Bin,
macOS) y «degradación explícita a borrado permanente con aviso» donde
no. Borrar es LA operación peligrosa de un file manager: la semántica
tiene que ser inequívoca en el wire, en el engine y en la UI.

## Opciones consideradas

### A. Implementación nativa

- **A1 — propia**: freedesktop es una spec asumible (info files,
  topdirs, cross-device), pero Recycle Bin exige COM (`IFileOperation`)
  y macOS `NSFileManager` vía objc — tres integraciones de plataforma
  con esquinas oscuras, para reimplementar algo que existe.
- **A2 — crate `trash` 5.x** (MIT, mantenido, MSRV 1.85 ≤ la nuestra):
  cubre los tres OS, y en linux/windows ofrece `os_limited::{list,
  purge, restore}` — la base del «restaurar» de M3. Alternativa
  evaluada y descartada: A1 (coste alto, valor nulo).

### B. Semántica de degradación

- **B1 — el engine degrada solo** (sin papelera → borra permanente):
  «explícito» dejaría de serlo — un cliente pediría trash y perdería
  datos permanentemente sin enterarse.
- **B2 — el engine JAMÁS degrada**: `DeleteMode::Trash` sin capability
  `TRASH` = `Unsupported`. El FRONTEND consulta capabilities, avisa
  («borrado PERMANENTE: aquí no hay papelera») y reenvía con
  `Permanent` si el usuario confirma. La degradación es una decisión de
  usuario informado, nunca del sistema.

## Decisión

- **A2 + B2.** Trait: `Provider::trash(p)` (default `Unsupported`);
  `LocalProvider` lo implementa con el crate `trash` en
  `spawn_blocking` y declara `CapabilityFlags::TRASH`; `MemProvider`
  también (papelera lógica: el subárbol desaparece — suficiente para el
  contrato).
- **Wire (0.2.0 → 0.3.0)**: flag `TRASH` (1<<7) y
  `FsDeleteParams.mode: DeleteMode { Trash, Permanent }` con
  `#[serde(default)]` = **Trash** — el default del protocolo es el
  SEGURO; un cliente 0.2 que no manda `mode` obtiene papelera (mejora
  recuperable, jamás pérdida sorpresa). `Permanent` es la elección
  explícita.
- **Task**: `Trash` es UNA operación sobre la raíz (el OS mueve el
  árbol entero — sin walk, cancelable antes de disparar); `Permanent`
  conserva el walk post-order actual. El journal (M3) registra la
  entrada como `Removed` con restauración vía `os_limited::restore`
  donde exista.
- **TUI**: F8 = papelera si el provider la declara (diálogo normal);
  sin capability, el MISMO diálogo pasa a rojo/aviso «PERMANENTE» y
  reenvía `Permanent`. `shift+f8` = permanente explícito siempre.
- **CLI**: `norte rm` sigue siendo permanente (banco de pruebas del
  engine, documentado).

## Consecuencias

Positivas: default seguro en el wire; degradación con usuario informado
(spec literal); M3 hereda list/restore/purge del mismo crate; una sola
integración de plataforma auditada.

Negativas / deuda: **skew de versiones** — contra un core <0.3, `mode`
se IGNORA (tolerancia de structs, ADR 0004) y el borrado es PERMANENTE:
los clientes DEBEN condicionar `Trash` a la capability `TRASH` (que un
core <0.3 jamás anuncia), NUNCA a su propia versión de protocolo — el
frontend conforme muestra el aviso de permanente y envía `Permanent`.
Recíproco N-1: un cliente 0.2 contra core 0.3 en un provider SIN `TRASH`
pasa de "delete funciona (permanente)" a `Unsupported` duro — falla en
seguro (en M1 no muerde: local y Mem declaran `TRASH`; revisar cuando
lleguen remotos/archive en M2). `trash` arrastra deps de plataforma
(objc2/windows);
en providers remotos (M2) no hay trash nativo — la «papelera lógica
`.norte-trash/`» de la spec queda para M2 con los remotos; el borrado
Trash no reporta progreso granular (una op del OS): entries_total = 1.
Excepciones de plataforma documentadas (issues #25/#26): en Windows
`FOF_NO_UI` auto-responde el «nuke warning» — un ítem no reciclable
(unidad sin $Recycle.Bin, red, tamaño sobre el límite) se DESTRUYE
dentro del delete; en freedesktop el caso cross-device degrada a
copy+delete interno del crate (GB posibles, incancelable a mitad).
