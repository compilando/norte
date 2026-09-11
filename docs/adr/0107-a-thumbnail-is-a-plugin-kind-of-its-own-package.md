# 0107 — A thumbnail is a plugin kind, in a WIT package of its own

- Status: accepted
- Date: 2026-09-11
- Decision makers: Oscar González
- Related: ADR 0037 (plugins describe, the host paints), ADR 0069 (how
  image bytes reach the webview), ADR 0089 (the RPC catalogue and its six
  gates), ADR 0094 (a WIT package bumps when one of its interfaces changes;
  the host serves one version of each), ADR 0105 (the icon column: the last
  time a plugin kind grew)

## Context

The window's viewer paints an image when the file IS one the webview can
decode (PNG, JPEG, GIF, WebP, BMP, SVG) and it fits under a byte cap and a
pixel budget; past either, it says so and shows nothing. Everything else —
a photo too big for the cap, a TIFF, a RAW, a PDF's first page, a video's
first frame — has no picture at all. The spec of 2026-09-11 (the window
polished) asked for a `thumbnail` plugin kind: a guest that turns bytes into
a small raster the viewer can show.

Three things had to be decided: where the interface lives, how the bytes
cross to the window, and how much the host trusts what comes back.

## Decision

**1. A new WIT package, `norte:thumbnail@0.1.0`, not an interface added to
`norte:plugin`.** The host serves ONE version of each package (ADR 0094);
bumping `norte:plugin` to 0.11 would list every installed guest — eight
official ones and the embedded FTP provider — as built against the wrong
version, for a kind none of them implements. `norte:renamer` and
`norte:hook` set the precedent: a kind with its own world gets its own
package and its own version. The manifest says `category = "thumbnail"`
and `[[contributions.thumbnail]] mimetypes = [...]`, matched like a
previewer's (exact first, then glob), and the mimetypes are part of the
approval digest.

**2. The bytes cross the daemon as `plugin.thumbnail`**, a request next to
`plugin.preview`: `{ path, max_edge }` in, `{ plugin_id, plugin_name,
mimetype, bytes (base64), width, height }` or nothing out. The daemon reads
the file under the same read gate as a preview, capped at
`THUMBNAIL_MAX_BYTES` (8 MiB — a photo, not a video; the guest's store has
64 MiB and has to decode what it gets), and runs the guest off the actor. The host asks for it only when the viewer has no picture of
its own: a file the webview cannot decode, or one past the viewer's caps.
The window then paints the thumbnail through the very channel of ADR 0069
(`image_bytes`, a `blob:` the renderer revokes), labelled «via ‹plugin›».
The terminal ignores the kind: it has no pixels to put a raster on.

**3. What comes back is hostile until proven otherwise, and never reaches
the webview as the guest wrote it.** Two gates in the plugin-host. First,
the header: the returned bytes are capped (`THUMB_MAX_BYTES`, 4 MiB), only
the three encodings the webview paints (PNG, JPEG, WebP) pass, and only
when the magic bytes agree with the declared mimetype and the declared
dimensions match the raster's header and stay within `max_edge`. Second,
the host DECODES the raster itself with the `image` crate — pure Rust, with
`Limits` on dimensions and allocation — and re-encodes it (PNG, or JPEG
when the PNG would not fit the cap). What goes into the `blob:` URL is a
raster the host made; the guest's bytes never meet libpng, libjpeg or
libwebp in the webview, which sit outside every sandbox norte controls. A
truthful header over a malformed compressed stream — the polyglot that
passes the first gate — dies in a Rust decoder, not in a C one with the
desktop behind it. Anything that fails either gate is logged (the guest's
own message capped like a log line) and dropped; the viewer keeps what it
had. A guest describes an image; it does not get to put bytes into a
`blob:` URL. The dependency this buys: `image` 0.25 in `norte-plugin-host`
with `default-features = false` and the three decoders only (MIT/Apache-2.0,
maintained, the same crate `image-thumb` uses).

## Consequences

- Sites a new method touches, per ADR 0089: `norte-proto` (constants,
  types, catalogue row, `PROTOCOL_VERSION` 0.73.0, goldens), the daemon
  handler, the embedded backend, the SDK, the ui-host backend trait and its
  fake, and the viewer. `protocol-guardian` reviews the wire.
- `org.norte.image-thumb` is the first guest: it decodes PNG/JPEG/GIF/
  WebP/BMP/TIFF with the `image` crate and re-encodes a downscaled JPEG or
  PNG (`format`, `quality` in `[config]`). Its value is the photo that was
  too big for the viewer's cap: it now gets a picture.
- PDF pages and video frames are the kind's reason to exist and are NOT
  shipped: a PDF rasteriser or a video decoder in WASM is a plugin of its
  own, with its own size and licence conversation.
- `just plugins` installs it like the others; no existing guest needs a
  rebuild, which is the point of decision 1.
