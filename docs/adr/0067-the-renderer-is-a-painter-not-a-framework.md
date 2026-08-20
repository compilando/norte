# 0067 - The reference renderer paints, and brings no framework to do it

- Status: accepted
- Date: 2026-08-20
- Decision makers: Oscar González
- Related: ADR 0065 (a frontend is retired before its replacement exists),
  ADR 0066 (renderers use a Rust UI host, decisions D2, D7, D11 and D14), the
  plan `docs/superpowers/plans/2026-08-19-multi-frontend-tauri-transition.md`
  (phase 3, tasks 3.1–3.6).

## Context and problem statement

Phase 3 of the multi-frontend plan builds a vertical slice in Tauri to answer
one question with measurements: **can a webview be norte's graphical frontend
without moving semantic state into JavaScript?** Task 3.1 requires the DOM
framework to be chosen and recorded, with dependency benefit, size,
maintenance and alternatives (hard rule 8).

The choice is not neutral here, because `norte-ui-host` already does the work a
UI framework usually does. The host owns the semantic state, and what crosses
the bridge is either a full `ViewSnapshot` or a `ViewPatch` that names exactly
what changed and the sequence it applies to (ADR 0066, D7). The renderer is
handed a precise diff. A virtual-DOM framework's core service — computing a
diff by re-rendering and reconciling — is therefore a second diff on top of one
that already exists, paid for on every cursor movement in a dense list.

There is a second constraint that narrows the field further. The production
webview runs under a closed Content Security Policy with no `unsafe-inline`, no
`unsafe-eval` and no remote origins (D11), and no runtime dependency may need
any of them.

## Options considered

### Option A — React

- **Advantage:** the most familiar and best documented option; the largest pool
  of components and answers.
- **Drawback:** the heaviest runtime, and a reconciliation pass over a diff
  Rust already computed.
- **Drawback:** dense virtualized lists need another library on top
  (`react-window` and friends), plus `memo` discipline to stop re-render
  cascades — exactly the surface where the phase's performance budgets bite.

### Option B — Solid or Svelte

- **Advantage:** fine-grained reactivity, no virtual DOM, small runtimes; good
  templating ergonomics.
- **Advantage:** signals map reasonably onto per-row updates.
- **Drawback:** each brings a compiler or plugin into the build chain for a
  spike whose variable under test is Tauri, not the JavaScript toolchain.
- **Drawback:** component-owned reactive state is an invitation to keep
  semantic state in the renderer, which D14 forbids; the boundary would be
  guarded by discipline rather than by having nowhere to put it.

### Option C — Plain TypeScript, no framework

- **Advantage:** zero runtime dependencies beyond `@tauri-apps/api`, so the CSP
  surface is the code we wrote.
- **Advantage:** the patch stream maps one-to-one onto DOM operations: a cursor
  patch touches two `<div>`s, and nothing re-renders.
- **Advantage:** there is no framework state for semantic state to hide in. The
  renderer's only state is a copy of the last snapshot, kept to paint.
- **Drawback:** DOM code by hand is more verbose, and common concerns
  (virtualization, focus, list keying) are ours to write and to test.
- **Drawback:** it does not scale to phases 4–6 for free; if the surface grows
  faster than the hand-written layer, the decision has to be revisited.

## Decision

**Option C.** The spike renderer is plain TypeScript with Vite for bundling and
one runtime dependency, `@tauri-apps/api`, which is how a webview talks to its
own process. The build chain is TypeScript, ESLint, Prettier and Vitest, all
pinned in a committed `package-lock.json`.

Dependency justification (rule 8), for the one runtime dependency:

- **Benefit:** the official, typed binding for `invoke`/`listen`. The
  alternative is calling `window.__TAURI_INTERNALS__` directly, which is
  private API and would break on a Tauri upgrade without a compile error.
- **Size:** a few kilobytes in the bundle; the whole production bundle is
  ~15 kB of JavaScript and ~3 kB of CSS, uncompressed.
- **Maintenance:** published by the Tauri project, versioned with it.
- **Alternatives:** `withGlobalTauri` (rejected — it puts an IPC object on
  `window` for any script in the page to find), or hand-rolled internals
  (rejected — private API).

Two rules keep the choice honest, and both are enforced by lint rather than by
memory:

- **No HTML from data.** `innerHTML` and `outerHTML` are forbidden; text goes
  in with `textContent`. A filename, a plugin string and a help line are data.
- **No `style` attribute.** The CSP blocks it, so dynamic geometry is set
  through CSSOM (`el.style.setProperty`). A lint rule rejects
  `setAttribute("style", …)` so the failure is at review time, not at runtime.

## What the toolkit costs in the dependency tree

Choosing Tauri 2 on Linux is choosing WRY, which is choosing GTK3 and
WebKitGTK. `cargo deny` says what that means, and it is recorded here because
it is a licensing and dependency decision, not a detail:

- **Eight `RUSTSEC` "unmaintained" advisories** for the gtk-rs GTK3 bindings
  (`atk`, `gdk`, `gtk`, `gtk-sys`, `gdk-sys`, `gdkwayland-sys`, `gtk3-macros`,
  `atk-sys`): gtk-rs declared them unmaintained when it moved to GTK4, and
  Tauri 2 has no path off them. Plus `proc-macro-error` 1.x (build-time, via
  `gtk3-macros`) and five `unic-*` data crates. None is a vulnerability; all
  are "nobody is looking after this any more".
- **Five MPL-2.0 crates** (`cssparser`, `cssparser-macros`, `dtoa-short`,
  `selectors`, `option-ext`). MPL is file-level copyleft, not viral over a
  binary that links it, so it does not reach the rest of the tree — but it was
  not on the allow list, and now it is, per crate.

All of it is scoped in `deny.toml` to the crates that carry it, with the reason
written next to each entry. It is accepted **for a spike that does not publish**
(`publish = false`, out of the default gate). If the spike becomes the product,
this list is revisited before general availability, not after: an unmaintained
GTK3 binding under a shipped file manager is a different decision from an
unmaintained GTK3 binding under an experiment.

## Consequences

### Positive

- The renderer has no state that could grow into a second presentation engine,
  which is what D14 exists to prevent.
- The measured cost of the choice is small and visible: one dependency, a
  ~15 kB bundle, and no compiler between the source and what ships.
- Painting is directly attributable. When a patch is slow, the code that
  applied it is the code we wrote.

### Negative

- Everything a framework would have provided is ours: virtualization, focus
  management, dialog behaviour, and their tests. Phase 4 will multiply that
  surface.
- If the hand-written layer starts to grow faster than the features it serves,
  this decision has to be reopened — and the bridge is deliberately
  framework-neutral so that reopening it costs a rewrite of the renderer only,
  not of the contract.
- Contributors used to React will find no components, only DOM.
