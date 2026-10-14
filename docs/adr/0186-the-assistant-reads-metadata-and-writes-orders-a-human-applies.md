# 0186 - The assistant reads metadata and writes orders a human applies

- Status: accepted
- Date: 2026-10-14
- Decision makers: Oscar González
- Related: ADR 0031 (the AI subsystem, opt-in), ADR 0050 (the agent plans
  and does not apply), ADR 0185 (the order line), spec
  `docs/superpowers/specs/2026-10-14-command-box-ai-design.md` §4, plan
  `docs/superpowers/plans/2026-10-14-ai-panel.md`.

## Context and problem statement

The order line (ADR 0185) gave norte a small language a person can read
and a program can check. The reader asked for an assistant on top of it:
"copy this month's PDFs to the other pane" in plain words, and questions
like "what takes the most space here?". The questions are where it can go
wrong: to answer, a model must look at the reader's files, and whatever it
looks at leaves the machine when the provider is remote. To act, it must
never be the one who acts.

## Considered options

1. **The model calls norte's operations as tools.** The usual agent shape.
   The model would apply; ADR 0050 forbids it, and an injected file name
   would be one tool call away from a deletion.
2. **The model writes shell.** Readable, but not checkable, and not
   journalled; refused in ADR 0185 already.
3. **The model writes ORDERS, reads METADATA through confined read-only
   tools, and the human applies through the order line's executor.**

## Decision outcome

Option 3.

- **One wire method, `ai.chat` (proto 0.87.0), stateless.** The client
  sends the conversation and where the reader is; the daemon returns one
  turn: a vetted order or an answer. No server session, so a reconnecting
  or second frontend loses nothing. A turn kind added later reads as
  `Unknown` on an older client.
- **Our own JSON tool protocol** (`{kind: order|answer|tool}`), not a
  provider's tool API: `norte-ai` speaks to Ollama, Anthropic and
  OpenAI-compatible servers, which share no tool interface.
- **Four read-only tools** — `list`, `stat`, `sizes`, `log` — confined to
  the two pane folders and outside `[ai] denied_prefixes`; names reach the
  model escaped the way the grammar writes them, inside a block marked as
  data; errors only as their category. Never file contents.
- **An order is vetted in the core**: it must parse; no catalogue command
  (some run with no dialog); nothing on another provider; a `mkdir` only
  inside the panes (it is the one action with no dialog). One retry when
  refused, then an answer — never an unvetted order.
- **No new policy actor.** The method is human-only, both folders pass the
  read gate, and it only reads. The AI gate (opt-in, local-only, denied
  prefixes) decides what may leave the machine, as for the rename plan.
- **The human applies**: Apply runs the order through the order line's
  executor, so every preview, guard and confirmation of ADR 0185 holds,
  and a read-only window refuses.
- **The panel** is a kind of its own (`layout.assistant`, `alt+Y` in all
  seven presets, `@` in the box), Inert to open. The conversation lives
  outside the slot and survives closing it.

### What was dropped from the spec

Streaming deltas (a turn arrives whole), and auto-detecting Ollama: AI
stays opt-in (ADR 0031), and the panel says what to add to `norte.toml`.

### Positive consequences

- The model proposes in the same language the reader can type; the reader
  learns the grammar from it.
- A prompt-injecting file name can at worst make the model PROPOSE an
  order, which the reader sees, with its preview, before anything runs.

### Negative consequences and residual risks

- `sizes` counts denied subtrees inside a pane: only a total leaves.
- Confinement is lexical: a symlink inside a pane that points outside is
  followed by the provider (the residual risk already named in
  `policy.rs`), so names and sizes beyond it can be read — never contents.
- The model's `note` is free text shown next to the order; it is painted
  below and smaller than the line that runs.
