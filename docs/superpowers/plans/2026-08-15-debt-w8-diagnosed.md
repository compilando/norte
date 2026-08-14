# Debt wave W8 — the five whose diagnosis was already written

One session, five issues, and the thing they have in common is that none of
them needs to be investigated: each body says what is wrong, where, and what
the fix looks like. Two needed a decision first and now have one.

| # | crate(s) | what | decided |
| --- | --- | --- | --- |
| #210 | norte-tui, norte-frontend | the compare and sync panes still pin the cursor to the last row | — |
| #182 | norte-proto, norte-core, norte-mcp | a daemon refusal reads as "internal error" because the message dies in `to_taxonomy` | a REAL taxonomy in `data` |
| #209 | norte-compare, norte-core | `fs.compare` still descends into the protected state directory | — |
| ~~#156~~ | norte-compare | ~~on-demand hydration is SERIAL~~ — **ya estaba hecho** (`1cc094b`), verificado y cerrado | — |
| #149 | norte-core, norte-tui, norte-gui | a copy never asks whether the destination has room | WARN, never refuse |

Order is that table: cheapest first, and #149 last because it is the only one
that adds a surface rather than fixing one.

## What each one actually is

**#210 — the same fix, two more homes.** `sticky_offset` and its tests already
exist (`norte_frontend::pane`); what the compare pane and the sync step list
lack is somewhere to KEEP the window between frames and a reconcile before the
draw. `ui::before_frame` is the shape to copy. The status bar's `pos/total`
gets cut by a long path on the same screen, and whoever is in there should fix
that too.

**#182 — the refusal that made the model retry.** `RpcError::protocol` carries
a message and no `data`, and `to_taxonomy` collapses that to
`Error::Internal { panic: false }` — which is what an MCP agent was told when
`sync.plan` refused its 17th retained plan, so it retried, which is what filled
the cap. The decision: give the refusals a taxonomy in `data`. The retained
plan cap is a `LimitExceeded` with a new token, whose vocabulary is already
OPEN by contract ("an unknown token is shown as-is, never a parse failure").
`norte-mcp`'s `map_plan_err` deletes its copy of the private constants.

**#209 — the half #165 could not close.** The scope registry refuses the state
directory and `fs.search`/`index.query` were given exclusions; `fs.compare`
walks from a legitimate root and still descends into it. `CompareOptions` is
`Copy`, so the exclusions ride with `Sides` — the thing the core already
computes and hands the engine (ADR 0051, #153's precedent).

**#156 — ya estaba hecho.** `1cc094b` lo construyó y nadie cerró el issue: `HYDRATE_CONCURRENCY = 12`, la pasada previa sobre las parejas del directorio, y el `id` asignado en orden de CLAVE con un test que fuerza los `stat` a terminar al revés. Segunda vez en dos olas que un issue sobrevive a su arreglo (#179 fue la otra) — **leer el código antes de planificar un issue viejo** es la regla que ya estaba escrita y que hay que aplicar ANTES, no al empezar la tarea. El texto original decía: `Walk::visit` holds both listings before it
emits a row, so the set of file pairs that will need hydrating is known up
front: one bounded `buffer_unordered` pass instead of one `stat` at a time.
`file://` is not "local disk" — it is whatever the OS mounted, SMB and NFS
included. The row ORDER is contractual and the concurrency must not touch it.

**#149 — warn, never refuse.** Free space is enumerated already (item 3). The
check happens before the first byte moves, says the numbers, and lets the human
decide. A destination that cannot answer (sftp, S3, an archive) degrades to
SILENCE, never to a false alarm — `Volume::free_bytes` is `Option<u64>` and
absent means "did not answer", never zero.

## Gate

`just t <crate>` in the loop, `just ci-fast` around #209, `just ci` at the
close. One protocol bump for the wave (#182's token), with its goldens and
schema regenerated in the same commit.

## Reviewers

- #182: `protocol-guardian` (the bump).
- #209: `security-reviewer` — it is the residue of a containment fix.
- #156: `rust-reviewer` on the concurrency, and the determinism test is the
  thing to point it at.
- #149: `encoding-auditor` is not needed; `rust-reviewer` is.
