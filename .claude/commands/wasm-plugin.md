---
description: Build a WASM plugin against the norte WIT interface — the manifest, the capabilities and what the guest may never assume
argument-hint: <plugin id, for example org.example.mimetype>
---
Build the WASM plugin `$ARGUMENTS`.

**Read ADR 0057 (the `location` capability) and the WIT in
`crates/norte-plugin-host/wit/` first.** What follows is what this repository
learned shipping `plugins/git-status`, which is the reference to copy from.

1. **The plugin lives OUTSIDE the workspace**, like a third party's would —
   `plugins/<name>/`, its own `Cargo.toml`, built to wasm and installed the way
   a stranger's plugin is installed. A plugin that compiles because it is inside
   our workspace is a plugin we have not really tested.

2. **The guest is `no_std`-shaped and untrusted.** It gets no filesystem, no
   network and no clock beyond what the WIT hands it. Everything reaches it
   through the host, which means through the policy engine — do not add a
   convenience that bypasses that, ever, not even temporarily.

3. **Declare capabilities in `plugin.toml`, and each one is consent the human
   gives.** `location = "read"` grants a *confined* read token. Two things about
   it that cost a security review to learn:
   - The token **dies with the call**: its `Drop` is the expiration. Do not
     design around holding one.
   - A confined token **cannot go up**. If the plugin needs an ancestor — a
     repository root, a project marker — declare `location-root-marker` and the
     host opens the ancestor that contains it, passing the `prefix`. It only
     climbs for a human actor, stops at protected roots and at 64 levels.

4. **Measure the per-page cost before believing it is free.** `git-status` was
   measured at 167 ms per page of 20 over 2000 entries, and that number is the
   whole content of issue #224. A columns plugin runs on every listing.

5. **Say what the plugin cannot see.** A location that is not `file://` mints no
   token, so the column is simply empty there — correct, and worth writing down
   so the same plugin does not look broken to somebody browsing a remote tree.
   Same for the cases it deliberately does not handle (#225 is exactly this list
   for git-status).

6. **Its gate is its own**: a `just` recipe that lints, tests and builds the
   wasm, plus an end-to-end test that runs the real module through the host.
   In-process contract tests are not enough — the wasm boundary is where the
   assumptions break.

Dispatch `security-reviewer` before committing. A plugin is a capability
surface, and that reviewer is the one that has found the escapes (#238–#240).
