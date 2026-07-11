# 0008 — norte-encoding: frontera de detección/decodificación

- Estado: accepted
- Fecha: 2026-07-11
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §6, plan M1 fase 7 (riesgo "superficie de deps"),
  ADR 0003 (licencias).

## Contexto y problema

El viewer necesita detección de encoding (spec §6: BOM → chardetng →
heurística NUL) y decodificación. `chardetng` + `encoding_rs` son deps
estructurales (tablas grandes, superficie amplia) y CLAUDE.md exige ADR
para dependencias estructurales y decisiones de licencia. Además hay que
fijar DÓNDE vive esto respecto a la regla 7 (frontends sin lógica de
negocio).

## Opciones consideradas

- **O1 — usar chardetng/encoding_rs directamente en norte-tui**: menos
  crates, pero la GUI (M3+) y terceros duplicarían la integración, y la
  superficie de deps quedaría desperdigada.
- **O2 — crate `norte-encoding` (MIT OR Apache-2.0)** que AÍSLA ambas
  deps tras una API mínima (`detect`/`decode`/`decode_forced`/
  `detect_eol`/`reload_cycle`); el resto del workspace jamás las importa
  directamente. Alternativas de deps evaluadas: ports de chardet (peor
  acierto) o solo-UTF-8 (violaría spec §6).
- **O3 — meter la detección en el core**: la decodificación es
  PRESENTACIÓN (no muta, no decide política; los bytes viajan por el
  protocolo tal cual) — en el core inflaría el AGPL con lógica de cliente.

## Decisión

**O2.** Frontera escrita: *decode = presentación, vive del lado cliente
del protocolo*; la LECTURA va siempre por el core (`Engine::read` con
rango, ADR 0005). Semántica fijada por tests + corpus del testkit:

- Detección: BOM manda (certeza) → NUL en TODO el buffer = binario
  (UTF-16 sin BOM cae aquí: recuperable a mano) → chardetng sobre 64 KiB
  (spec §6.2), con `Utf8Detection::Allow` (esto no es un navegador) e
  `Iso2022JpDetection::Deny`.
- `decode(…, complete)`: streaming — una cabecera TRUNCADA deja la
  secuencia partida pendiente, jamás la marca como pérdida.
- `decode_forced`: SIN BOM-sniffing — lo forzado por el usuario MANDA
  (spec §6.2 "siempre corregible a mano"); un BOM espurio es dato.
- `Display` técnico estable (la lib no localiza; el frontend mapea).

## Consecuencias

Positivas: GUI/terceros reutilizan sin AGPL ni duplicación; la
superficie chardetng/encoding_rs queda en UN punto auditable; el corpus
de contenidos del testkit (9 detectables + 3 solo-forzables) es la vara.

Negativas: dos deps grandes en el árbol (compiladas solo donde se usan);
`GB18030`/`GBK` comparten decoder en encoding_rs — se expone la etiqueta
GB18030 (la que nombra la spec). El viewer presupuesta 256 KiB de
cabecera; si crece, decodificación e índice de líneas deben salir del
hilo del loop (anotado en main.rs).
