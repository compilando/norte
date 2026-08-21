# 0069 - How image bytes reach the webview, and what the window still refuses to do

- Status: accepted
- Date: 2026-08-21
- Decision makers: Oscar González
- Related: ADR 0066 (renderers use a Rust UI host), ADR 0067 (the renderer is a
  painter, not a framework), ADR 0068 (a row is named by key and generation),
  the phase 4.3 entry in
  `docs/superpowers/plans/2026-08-19-multi-frontend-tauri-transition.md`.

## Context and problem statement

Task 4.3 of the Tauri transition asks for "image preview with explicit
decode/size caps", "plugin preview and styled preview", and "external
opener/terminal commands through Rust only". The plan attaches a stop sign to
the first of those:

> Never pass arbitrary file paths to `<img src="file://...">`. Decode through
> the host or a narrowly scoped safe asset protocol with a separate security
> review.

Three things are bundled under one task heading, and they carry very different
risk. Splitting them is most of the decision:

**Plugin preview is already text.** `plugin.preview` and
`plugin.preview_styled` return lines of spans with a `role` from
`norte-theme`'s closed vocabulary — the host paints, the plugin only
describes (ADR 0037). Nothing binary crosses, and the masking discipline is
the one every other third-party string already uses. It needs no decision and
is not discussed further here.

**Images need bytes in the DOM**, and today they cannot get there. The CSP in
`tauri.conf.json` is `default-src 'none'` with `img-src 'self'`, and
`webview_boundary.rs` asserts that no `http://`, `https://`, `ws://`, `*`,
`'unsafe-inline'` or `'unsafe-eval'` appears anywhere in it. `file://` is
already refused by the platform, so the plan's stop sign is enforced rather
than merely written down. Anything we do has to open a door deliberately, and
name how wide.

**The external opener spawns a process**, which is a different kind of act
altogether, in a window whose phase-4 contract is that it does not mutate.

## The image options

### Option A — `data:` URI in the view

The host reads a bounded prefix of the file, base64-encodes it and puts the
string in `ViewerView`. CSP becomes `img-src 'self' data:`.

- **For**: no new IPC surface, no new command, no lifetime to manage. The
  bytes travel by the same path as every other painted thing, so the existing
  sequence/patch guarantees apply unchanged.
- **Against**: base64 inflates by a third, and the string lands in the patch
  stream — the same stream that `payload.rs` guards against carrying a
  hundred thousand rows. A 4 MB image becomes a 5.5 MB JSON message that also
  has to be re-sent on every `Resync`. And `data:` in `img-src` is a
  well-known XSS amplifier in documents that also render untrusted markup;
  this one does not today, but the CSP is a property of the whole document,
  not of the one element we had in mind.

### Option B — a `blob:` URL from bytes fetched over a dedicated command

A new `preview_bytes` Tauri command returns raw bytes; the renderer wraps them
in a `Blob`, takes an object URL, and revokes it when the preview closes. CSP
becomes `img-src 'self' blob:`.

- **For**: no base64 inflation, the bytes never enter the patch stream, and
  the URL is revocable so the lifetime is explicit. `blob:` cannot be
  fabricated by content — a blob URL only exists because this document created
  it — which makes it a strictly narrower grant than `data:`.
- **Against**: a second path for content to reach the screen, so it needs its
  own staleness discipline (a preview that lands after the viewer closed must
  not paint) and its own caps, and neither is inherited from the bridge.

### Option C — a custom `asset:` protocol scoped to approved paths

Register a protocol handler that serves files under paths the host has
approved.

- **For**: streams, so an arbitrarily large image never sits in memory twice.
- **Against**: it is a URL space that maps to the filesystem, which is exactly
  the shape the plan's stop sign is about. Every bug in the scoping is a file
  read; `norte-vfs-local`'s `ConfinedRoot` exists because getting this right
  is hard even with `openat2(RESOLVE_BENEATH)`. It also bypasses the daemon,
  so an image would be read by a path the policy engine never sees — which
  contradicts hard rule 9.

## Decision

**Option B for images. The external opener is deferred to phase 5.**

The image bytes come from `HostBackend::read` with an explicit byte range, the
same call the text viewer already uses, so the read goes through the daemon
and the policy engine like every other read (hard rule 9). They cross to the
renderer through one new command whose only job is that, they become a
`Blob`, and the object URL is revoked when the preview closes or is replaced.

The CSP gains exactly `blob:` on `img-src` and nothing else.
`webview_boundary.rs` keeps asserting the whole prohibited list, and gains an
assertion that `data:` is *not* in `img-src` — so choosing A later has to be a
deliberate edit to a test that says why, rather than a relaxation nobody
notices.

Three caps, all in the host, all refusals rather than truncations:

- **Bytes**: a preview reads at most a fixed prefix. A file over the cap is
  not previewed and says so; a truncated image is a decoded image of something
  that is not the file.
- **Declared dimensions**: the host parses the header only and refuses
  anything whose declared width × height exceeds a pixel budget, *before*
  handing bytes to the webview. A 64 KB PNG can declare 60000×60000 and cost
  gigabytes in the decoder — the "decompression bomb" shape that
  `err-limit-exceeded` already exists for elsewhere in norte.
- **Formats**: a closed allow-list, decided by sniffing the magic bytes and
  never by the filename extension. An extension is a claim by whoever named
  the file.

Nothing about this makes the renderer a decoder: it hands a blob to `<img>`
and the platform decodes. What the host guarantees is that the blob is small,
that its declared size is sane, and that it is one of the formats we chose.

### Why the opener is not in this phase

`app.open` and a terminal command spawn a process with the user's privileges,
on a path the user selected. That is not a read, and it is not covered by
"read-only graphical beta". It also has a design question of its own that this
ADR does not answer — what norte does about `xdg-open` semantics, quoting, and
a file whose name is a flag — and issue #144 (openers without cwd) is already
open against the terminal frontend's version of it. It goes with the phase 5
mutation work, where there is a confirmation path and a journal to hang it on.

The window says so rather than showing a dead button: `ActionAck::Unavailable`
with `host-open-file-not-implemented`, which is the answer it already gives.

## Consequences

### Positive

- The stop sign in the plan is now a mechanism: `file://` stays refused by the
  CSP, and the one grant we make is the narrowest of the three considered.
- An image is read through the daemon, so the policy engine sees it. Option C
  would have made the window the only surface that reads files behind policy's
  back.
- The patch stream keeps its size guarantee. A preview cannot make a
  `ViewSnapshot` megabytes wide, so `payload.rs`'s guard keeps meaning what it
  says.
- The caps are refusals with reasons, which is the project's existing answer
  to a limit (`err-limit-exceeded`, the archive limits, the viewer's byte
  budget) rather than a new one.

### Negative

- A second path for content, with its own staleness rule to get right. The
  viewer already had one bug of exactly this shape — a read that landed after
  the user pressed `Esc` and opened the viewer by itself — and this path can
  have it too. It needs the same request-token discipline and a test.
- Bytes are held twice for a moment: once in the Rust command's response and
  once in the JS `Blob`. Bounded by the byte cap, which is why the cap is not
  optional.
- Header parsing in the host is code that reads attacker-controlled bytes to
  decide a number. It must be the boring kind: fixed offsets, no allocation
  driven by the input, and a refusal for anything it does not fully
  understand.
- A leaked object URL is a leaked buffer for the life of the document. The
  revoke has to be on the same path as the close, not on a `finally` that a
  future refactor can drop.

### Neutral

- Plugin preview lands in the same task and inherits none of this: it is text
  with spans, painted by the host, masked like every other third-party string.
