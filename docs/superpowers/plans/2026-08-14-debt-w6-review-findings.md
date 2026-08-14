# Debt wave W6 — what the reviews found and nobody scheduled

Ten issues that came out of the W2/W4a/W4b branch reviews and the phase-B
surfaces work, plus two older ones that were waiting for a decision they now
have. None of them is a wave of its own; together they are one, because they
share a property: **the diagnosis is already written.** Each body says what is
wrong and why. That is the cheapest kind of work there is, and it is why this
goes before #171 and W5, which both need a design decision first.

Tiered by reading the BODIES. Two changed tier that way:

- **#204 is not a hole.** `connection.trust_host_key` is already refused for any
  actor that is not `Actor::User`, in the daemon dispatch. What is missing is
  the rustdoc saying so and the rule-4 classification. It is a note, not a gate.
- **#174 already has its ADR.** ADR 0051 chose: framing primitives to
  `norte-proto`, `norte-core`'s copy left alone and PINNED by an equality test.
  W3 built the fold-key half only — `norte-proto` has no `feed`/`hex_lower`
  today. So this is implementation, not a decision.

## In this wave

| # | crate(s) | what | reviewers |
| --- | --- | --- | --- |
| #198 | norte-tui | the #157 stat probe resolves into the NEXT comparison's freshly-cleared maps | `rust-reviewer` |
| #202 | norte-gui | `on_plan_ended`'s guard is racy in principle and its rustdoc reads as if it is not | — |
| #201 | norte-cli | the #187 signal test can pass without sending a signal at all | `test-engineer` |
| #181 | norte-core | a failed resync leaves `inner.client` populated: notifications stop routing and `TaskRef::join()` hangs FOREVER | `rust-reviewer` |
| #204 | norte-core | write down the classification of the `known_hosts` write (rule 4) and the gate that already exists | `security-reviewer` |
| #174 | norte-proto, norte-sync, norte-core | build ADR 0051's second half: framing to `norte-proto`, the journal's copy pinned equal | `protocol-guardian` |
| #208 | norte-frontend, tui, gui | three 0.42.0 fields the painters can read and are still inferring | `encoding-auditor` |
| #200 | norte-gui | there is no paste anywhere in the GUI, and whoever adds it adds the hazard filter with it | `encoding-auditor` |

Order is that table. It is cheapest-first EXCEPT #181, which jumps: a `join()`
that hangs forever is the worst outcome in the list, and the MCP bridge is the
first caller that cannot close a window to escape it.

## Not in this wave, and why

| # | what it needs first |
| --- | --- |
| #203 | a product decision: how a persistent unexplained `Busy` escalates (louder indicator? refusal after N minutes?) |
| #206 | a journal schema change — ADR 0046's "bumping `JOURNAL_FORMAT` is not enough" clause |
| #207 | a decision that changes what a PLAN does with a pair: blocker (new wire vocabulary) or `Skip` with a reason. Wants an ADR |
| #179 | the ownership window primitive, with the `ChainState` re-read on every acquisition. Not a five-line change and its own body says so |
| #199 | a design question the issue states: the GUI list is virtualised and has no run loop to hang a probe off |

## Cómo acabó

Siete de ocho. Cerradas: #198, #202, #201, #181, #204, #174, #208.

**#200 se queda abierta, y no por tamaño.** Al abrirlo: en esta GUI no existe
pegado de ninguna clase — ni `InputHandler` registrado, ni una lectura de
portapapeles para entrada de texto — así que no hay un filtro que cablear,
hay que construir la entrada por pegado Y su filtro a la vez. Y lo que decide
el trabajo no es eso, es el ENRUTADO: la TUI necesitó una cadena entera
(`route_paste`) porque un pegado tiene que llegar exactamente al mismo sumidero
que llegaría la misma tecla, y un `y` pegado en un modal de confirmación no
puede confirmar. Esa cadena hay que escribirla para las superficies de la GUI,
que no son las mismas.

Es una rama con `encoding-auditor`, no la cola de una ola. Lo que sí queda
hecho es la pregunta de seguridad que la abrió: **no hay bypass del filtro de
hazards, porque no hay pegado**.

Lo que sí conviene mover cuando se haga: `first_pasted_line` vive en
`norte-tui/src/main.rs` y es la primitiva compartible (dónde está el límite de
línea: CRLF, `\r` suelto, NEL/LS/PS). La cadena de enrutado no es
compartible; la primitiva sí.

## Gate

`just t <crate>` in the loop. `just ci-fast` once around #204. `just ci` at the
close. `just gui-ci` before any GUI commit — and note that a bare `cargo test`
in `norte-gui` fails 7 tests that nextest passes, because the active i18n
language is process-global.

## Reviewers

Dispatched by whoever does the work, before committing:

- #181 and #174: `rust-reviewer`; #174 also `protocol-guardian` (it moves
  primitives INTO the wire crate, even though no wire type changes).
- #204: `security-reviewer` reads the classification, since the whole change is
  a claim about a security-relevant write.
- #208 and #200: `encoding-auditor` — one decides which side's encoding names a
  path, the other is the first paste path in that frontend.
