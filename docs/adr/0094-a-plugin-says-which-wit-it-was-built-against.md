# 0094 — A plugin says which WIT it was built against, and the host says whether it serves it

- Status: accepted
- Date: 2026-09-03
- Decision makers: Oscar González
- Related: ADR 0022 (manifest and sandbox), ADR 0032 (the recorded debt of
  the shared package), ADR 0041 decision 4 (the package split), ADR 0093
  (provider plugins), #241 (the approval anchor covers the binary), spec §7.1
  and the M4 exit criterion ("a third party can ship a plugin without
  changing the core")

## Context

A WIT package's version is part of every interface name a component imports
or exports: a previewer built today exports `norte:plugin/previewer@0.8.0`
and imports `norte:host/host-log@0.1.0`. When the host moves `norte:plugin`
to 0.9.0, that binary no longer matches the world the host links, and
instantiation fails inside wasmtime with an error that names one interface
and nothing else. The extension manager shows the plugin as approved and
enabled; the first preview says "internal error".

This has been verified empirically three times inside the repository, where
it costs nothing because every guest is recompiled from the current WIT.
Splitting the WIT into four packages (ADR 0041 decision 4) made a `provider`
change stop breaking previewers. It did not make any bump survivable, and
ADR 0041 decision 3 guarantees bumps: the WIT moves when a real plugin needs
a gap closed, and D4 of the demo-plugin program is the first.

Until now the project had no statement of what a bump does to someone else's
binary, and no way for norte to say which binary is affected. That is the gap
between "the mechanism exists" and "a third party can ship a plugin".

## Decision

**The host reads which `norte:*` packages a binary names, and a binary that
names a version the host does not serve is listed as broken with both
versions — never loaded, never silently skipped.**

### 1. Read from the binary, not declared in the manifest

`norte_plugin_host::wit_packages(bytes)` walks the component's import and
export sections with `wasmparser` — no compilation — and returns the
`(package, version)` pairs among names of the form `norte:<pkg>/<iface>@<v>`.
Both sections, because a previewer *exports* `norte:plugin` and *imports*
`norte:host`, and either can be stale.

A manifest field (`wit = "0.8.0"`) was the obvious alternative and is wrong
twice: it is self-declared, so it can lie or go stale independently of the
binary; and it would enter the approval digest, so a rebuild against a new
WIT would change what the human approved for no security reason. The binary
is the truth, and the catalogue already reads it once to anchor the approval
(#241).

### 2. One served version per package, no window

`SERVED_WIT` in the host names exactly one version of each package:
`norte:host`, `norte:plugin`, `norte:provider`, `norte:location`. A structural
test keeps it equal to the `package` lines of the `.wit` files, so a bump
that forgets the table turns the gate red rather than listing freshly built
guests as broken.

There is no compatibility window. Serving 0.8.0 and 0.9.0 side by side would
mean keeping every old world linked, and every host function reachable from
it, for as long as the promise lasts; the first bump the project needs (D4:
a `bg` colour on a span and the viewer's width in the preview input) is an
additive change that a window would still have to carry as a separate world.
The promise this ADR makes instead is cheaper and honest: **your binary keeps
working until the package it names moves, and when it moves norte tells you
so, by name, in three places.**

### 3. Listed, not loaded

`Catalog::load_dir` reads `plugin.wasm` once, derives the digest and the
packages, and on a mismatch pushes the plugin to `errors` with
`ManifestError::WitMismatch { package, built_against, served }`. It is not a
manifest error, and the variant says so, but `errors` is the list the
manager, `plugin.list` and `norte plugin list` already show, and a broken
manifest and a stale binary are the same fact to a reader: "installed, not
running, here is why".

`norte doctor` distinguishes them: `plugin-wit-mismatch` is its own warning,
because the fix is different — rebuild, not edit.

The state file is untouched. A rebuilt guest is a new binary, and the
approval anchor covers the binary, so it is approved again; the author guide
says so and says why.

### 4. What a bump is, and how it is announced

- `norte:host` changes rarely and on its own; every world imports it, so a
  bump there stales every plugin.
- `norte:plugin`, `norte:provider` and `norte:location` bump their minor
  version for any change, additive or not — the name changes either way.
- The CHANGELOG entry that bumps a package names it, its new version, and the
  interfaces it touched, and carries the sentence a release note needs:
  "plugins built against `norte:plugin@0.8.0` need a rebuild". Every in-tree
  guest and the template are rebuilt in the same commit.

## Consequences

- A stale binary is visible where a person looks, with the two versions and
  one verb. The wasmtime error is no longer the first thing a plugin author
  sees.
- Reading the import section adds a parse of every `plugin.wasm` at
  discovery, on the bytes the catalogue already reads for the digest. The
  artifact cap the runtime enforces at instantiation (64 MiB) is now enforced
  at discovery too, by size, before the file is read — a binary over it is
  listed as broken with its size — and at `norte plugin install`, which
  refuses to copy it. The parse itself is linear, iterative and does not
  decompress. The version string a binary names is narrowed to a version
  shape at the reader (ASCII, 64 bytes), so what reaches the manager and the
  doctor cannot carry a terminal escape.
- The policy is one version, stated. A window is a later decision, if a
  third-party ecosystem ever makes recompiling on release day the wrong ask;
  nothing here prevents it, and `SERVED_WIT` is the table it would extend.

## Amendment 2026-09-03: what "any change" means, learned on the first bump

The first bump under this policy came the same day: `norte:location` gained
`read-prefix` (0.1.0 → 0.2.0) for the media-info demo. The `norte-columns`
world in the `norte:plugin` package imports that interface, so the
`norte-plugin.wit` file changed too — and decision 4 said "minor bump for any
change".

`norte:plugin` was **not** bumped, and the rule is refined: **a package bumps
when one of its own interfaces changes**, because the version travels in the
interface names, and only those names decide whether a binary loads. A
world's import list is not an interface name: a previewer compiled against
`norte:plugin/previewer@0.8.0` still exports that exact name and still loads.
A columns guest compiled against `norte:location@0.1.0` imports a name that
no longer exists, and the catalogue lists it with both versions — which is
this ADR working as intended, on the package that actually moved.

Bumping `norte:plugin` as well would have staled every previewer to announce
a change that did not touch them.

## Alternatives considered

- **A `wit` field in the manifest.** Self-declared and stale; see decision 1.
- **A compatibility window (N and N-1).** Every old world linked forever, for
  a promise the first real bump would already strain; see decision 2.
- **Do nothing and document the wasmtime error.** The error names an
  interface, not a version, and is raised at first use rather than at
  install; the human who approved the plugin learns it is broken from a
  preview that says "internal error".
