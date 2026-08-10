# 0047 - Volume label crosses the wire as bytes, not `String`

- Status: accepted
- Date: 2026-08-10
- Decision makers: Oscar González
- Related: hard rule 1 (treat filenames/paths as bytes); ADR 0039 (provider
  attributes on the wire — the precedent this decision reuses); design
  `docs/superpowers/specs/2026-08-10-volumes-design.md` (§A, the `Volume`
  type); plan `docs/superpowers/plans/2026-08-10-volumes.md` (task V3.5, not
  itself in the plan — an encoding-auditor finding V3's review deferred).

## Context and problem statement

`host.volumes` (proto 0.37.0, task V2 of the volumes plan) introduced
`Volume`, a host-enumerated disk/mount with a `label: Option<String>` field —
"what the OS or the filesystem calls it, when it says" (`USB Nico`, say).

`Volume::mount` is a `VPath`, deliberately never a `String`, because a mount
point is bytes with no encoding contract (`/proc/mounts` places none on what a
filesystem may be mounted at) and rule 1 requires those bytes to survive the
wire exactly. `label` shares the exact same status: an ext4 label is whatever
bytes `mkfs.vfat -n`/`e2label`/the OS handed the filesystem, a vfat label is
padded OEM-charset bytes, and nothing at the filesystem or OS-API level
guarantees UTF-8. A `String` field can only hold valid UTF-8, so it either
throws bytes away silently (a lossy conversion at read time, the class of bug
this repository has repeatedly found and fixed — see the encoding-auditor
findings behind `Volume::mount`, `fs_type` in the TUI row, and the canonical
hostile-name corpus in `norte-testkit`) or refuses a legal label outright.

It shipped as `Option<String>` anyway, because the Linux implementation
(`norte-core::volumes::linux`) has no label source at all today and always
sets it to `None` — the bug was real but unreachable, so the plan's own
encoding-auditor review flagged it as deferrable rather than blocking. The
task that makes it reachable is V4 (macOS's volume-name APIs, Windows'
`GetVolumeInformationW`), and fixing the representation after V4 ships would
mean breaking the wire shape of `label` a second time, once for real users on
those platforms. This ADR is the fix landing before that happens.

Windows adds a second wrinkle beyond "not necessarily UTF-8": `GetVolumeInformationW`
returns UTF-16 code units, which are not bytes in the Unix sense at all and are
not UTF-8 either — a third encoding situation alongside "Linux/macOS: arbitrary
OS bytes."

## Decision drivers

- Hard rule 1: never assume a name/path is UTF-8; carry bytes, not `String`,
  wherever a platform does not promise text.
- The repository already has two wire shapes for carrying non-UTF-8 bytes
  (`VPath` for a scheme+authority+segment path, `AttrValue::Bytes` for a flat
  blob with a namespaced typed-cell system around it, ADR 0039). A third,
  bespoke shape for one field is a cost with no offsetting benefit.
- The fix has to be decidable NOW, from documentation and a type change alone
  — V4 (the code that starts producing real macOS/Windows label bytes) is out
  of scope for this task, so the representation has to be right without
  requiring a working Windows implementation to prove it.

## Considered options

1. **Leave it `Option<String>`.** Free today (Linux never populates it), but
   breaks the moment V4 ships a non-UTF-8 macOS/Windows label — silently
   (`String::from_utf8_lossy` at the point that constructs it) or by refusing
   a legal label. Rejected: this is exactly the bug class rule 1 exists to
   forbid, and deferring the fix to V4 means bumping the wire twice instead of
   once.
2. **Reuse `VPath`.** `VPath` already solves "bytes across the wire" and nothing
   new would need to be built. Rejected: `VPath` is a scheme + authority +
   segment STRUCTURE for a filesystem path (percent-encoded segments, parent/
   join operations, a `file://`-shaped display form). A label is a flat blob
   with none of that shape — forcing it through `VPath` would mean inventing a
   fake scheme/segment for a value that is not a path, confusing every reader
   of the wire format and the type.
3. **A fourth, bespoke byte-carrying shape** (e.g. a raw JSON array of
   integers, or a new `Bytes`-flavored newtype not shared with anything else).
   Rejected: the repository already has two answers to "how do bytes cross the
   wire," and CLAUDE.md's own guidance for this exact situation is explicit —
   choose deliberately among existing precedent, because inventing a third (or
   here, fourth) shape is worse than reusing one.
4. **Reuse `AttrValue::Bytes`'s wire shape: `Option<Vec<u8>>`, base64 text on
   the wire, absent key for `None`.** `AttrValue::Bytes` (ADR 0039) already
   solves "an arbitrary, possibly non-UTF-8 blob, base64 on the wire" for
   exactly this kind of value (an SFTP owner name that is not UTF-8). A label
   is the same class of value. Chosen.

## Decision

`Volume::label` is `Option<Vec<u8>>` in both `norte_core::volumes::Volume` and
the wire type `norte_proto::methods::Volume`, encoded as base64 text on the
wire (absent key for `None`) via a dedicated `label_wire` serde module —
sharing its base64 encode/decode logic with `AttrValue::Bytes` (two small
`pub(crate)` helpers, `encode_bytes_b64`/`decode_bytes_b64_lenient`, extracted
into `norte-proto::attrs` and used by both) rather than duplicating it.

Unlike `AttrValue::Bytes`, a malformed or oversized (over `ATTR_BYTES_MAX`,
reused rather than a second bespoke cap) `label` payload does not degrade the
whole `Volume` to some `Unknown` placeholder — it degrades just the `label`
field to `None`. `AttrValue`'s "whole cell degrades" ceremony (ADR 0039 §3)
exists because an attribute cell is one of many arbitrary values a WASM
provider plugin controls and might get wrong; `Volume` is produced by the
trusted host service, not a plugin, and `mount` (the field that matters for
navigation) is still perfectly usable even if `label` is garbage — losing the
whole entry over one cosmetic field would be a worse failure than the one it
avoids.

The wire method (`host.volumes`, its params/result types, `VolumeKind`'s
`#[serde(other)]` forward-compat) is unchanged; this is a shape correction to
one field of an already-defined type. `PROTOCOL_VERSION` still bumps
(0.37.0 → 0.38.0, N-1 window shifted to 0.37.x) because the wire bytes for
`label` genuinely change shape (a bare JSON string becomes a base64 string),
even though `host.volumes` itself has not shipped to any real client yet —
this repository bumps and shifts the window on every wire-shape change
regardless of release status, so two different byte-for-byte behaviors never
share one version number.

Per-platform encoding is documented directly on `Volume::label`'s rustdoc in
both crates (the two cannot cross-link — `norte-proto` cannot depend on
`norte-core`), so V4's author has the answer without re-deriving it:

- **Linux**: `None` today; a future `/dev/disk/by-label` reader would produce
  arbitrary bytes, the same status as any other Linux filename.
- **macOS** (V4, unverified here): arbitrary OS bytes from the volume-name
  APIs, treated like any other macOS path component — not guaranteed UTF-8.
- **Windows** (V4, unverified here): `GetVolumeInformationW`'s UTF-16 output
  must be encoded as WTF-8 before it reaches this field — the same encoding
  CONVENTION `norte-vfs-local`'s private `native_path`/`wtf8` modules already
  apply to Windows path segments (WTF-8 is what lets an unpaired UTF-16
  surrogate — legal in a FAT/NTFS label — survive losslessly, where
  `String::from_utf16_lossy` would silently replace it with `U+FFFD`). Those
  helpers are `pub(crate)` to `norte-vfs-local` and neither `norte-core` nor
  `norte-proto` can depend on that crate for this purpose today, so V4 either
  re-derives the small WTF-8 codec or — the better option, left for V4 to
  decide rather than assumed here — hoists it into `norte-vfs`, which both
  `norte-vfs-local` and `norte-core` already depend on.

## Consequences

### Positive

- `label` can never lose or corrupt bytes on the way to the wire or the
  screen, on any platform, including once V4 starts populating it for real.
- No new wire shape: a peer implementation that already understands
  `AttrValue::Bytes`'s base64 convention needs no new mental model for
  `Volume::label`.
- The TUI's row renderer (`norte-tui::app::volume_item_display`) already ran
  `label` through the same masking (`display_name`) every other text field on
  the row uses; because `label` now reaches that function as the original
  bytes instead of a `String` an earlier layer may have already lossily
  decoded, the masking protects real evidence instead of a rewritten copy.
- The per-platform rustdoc means V4's author does not have to rediscover the
  Windows UTF-16-is-not-bytes distinction from scratch, and is warned away
  from a `String::from_utf16_lossy` shortcut before writing it.

### Negative

- `Option<Vec<u8>>` is one extra base64 encode/decode per volume compared to a
  bare JSON string — negligible at the scale of "a handful of mounted
  volumes," not worth optimizing.
- A human reading raw wire traffic sees `"label": "VVNCIE5pY28="` instead of
  `"label": "USB Nico"` — a real ergonomic cost for manual debugging, accepted
  because the alternative is data loss on a label that is not ASCII, and the
  same trade was already accepted for `AttrValue::Bytes`.
- `norte-vfs-local`'s WTF-8 codec is not actually reusable from `norte-core`
  or `norte-proto` today (both `native_path` and `wtf8` are crate-private);
  V4 either duplicates a small, easy-to-get-subtly-wrong piece of logic or
  does the extraction work this ADR recommends but does not itself do. Left as
  an explicit, documented gap rather than a silent one.
