# 0074 - A drop is a list someone else wrote

- Status: accepted
- Date: 2026-08-25
- Decision makers: Oscar González
- Related: ADR 0066 (multi-frontend, D10 all effects through the daemon and D11
  an unprivileged webview), ADR 0069 (how an image's bytes cross), ADR 0070
  (the window writes), issue #283, task 6.5 of the new-frontend plan.

## Context and problem statement

`dragDropEnabled` had been `false` in `tauri.conf.json` since the window was
built, on purpose: the plan for task 6.5 says "drag-out/drag-in only after a
platform/security design", and the rest of 6.5 — clipboard, open-with, terminal
— shipped without needing one.

Four questions had to be answered before any code, and the reason they are hard
is the same in all four: **a drop is the only gesture in this window whose
operands do not come from this window.**

1. **What comes in.** A drop carries URIs from another process. They can be
   `file://` for anywhere on the disk, and the sender composes the
   `text/uri-list` by hand if it wants to. That is an unvalidated source path
   arriving at a window that writes.
2. **What goes out.** Dragging *out* means offering the paths of everything
   marked to any application that accepts the drop — including one that should
   not see them.
3. **What about what is not local.** There is no `file://` to offer for an
   `sftp://`: either it is downloaded to a temporary file (where? who deletes
   it?) or it is refused.
4. **How it is said.** A drop is a gesture without confirmation by nature, and
   this window asks before copying, moving and deleting.

## Decision

**Only in. Only copy. Always ask.**

### Only in

Drag-*out* is not offered, and questions 2 and 3 therefore do not arise. The
paths of what is marked stay inside this process; nothing gets downloaded to a
temporary file that nobody owns.

The cost is real and it is accepted: dragging a file from norte to a browser
upload field does not work. The clipboard already covers moving names to
another application, it is explicit, and it goes through a channel the reader
triggered on purpose.

### Only copy

`Pendiente::Soltar` is a separate variant from `Pendiente::Transferir`, and the
verb is fixed at copy. Moving what another application dragged means deleting
it from wherever that process keeps it, and this window has not asked that. It
also cannot: the source is not in any panel, so there is nothing to refresh and
no marks to consume — and consuming them would clear a selection the reader
made for something else.

### Always ask

The drop opens the same confirmation as a copy: destination in its own field,
sources masked line by line, `MAX_LINEAS_DIALOGO` of them with the overflow
counted. That dialog is the only chance the reader gets to see that what
arrived is not what they dragged.

The count of what was trimmed is taken against **what arrived**, not against
what converted. "16 of 40 shown" has to stay true when four of those 40 fell on
the floor.

### The destination may be remote

The active pane's directory, whatever its scheme. Uploading to the server what
you drag off the desktop is the comfortable case, and the core has copied
between providers from the start. This is the same reasoning as #284, where
requiring a local pane was written, tested, and then removed.

## Consequences

- `UiAction::FilesDropped { paths: Vec<String> }`, bridge **39**. Native text,
  not `VPath`: converting is the host's job, and what does not convert is
  dropped and *said* — `host-drop-unusable`, never silence.
- A path with no `file_name()` (a root) is dropped here rather than skipped
  silently by the send loop, where it could no longer be counted.
- **A name that is not UTF-8 cannot cross.** The bridge is JSON, and
  `to_string_lossy` would name a *different* file, so the renderer discards it.
  The reader sees a shorter list than what they dragged. This is the known
  limit of this route; the panel is what sees the bytes whole (rule 1), and a
  drop is a convenience, not the way to copy hostile names.
- Only `DragDropEvent::Drop` is forwarded. `Over` and `Leave` are not: the host
  paints no drag highlight, and forwarding them would be traffic for every
  pixel the pointer crosses.
- Nothing bypasses `Efectos::SoloLectura`: `Pendiente::Soltar` is in the list
  of pending actions a read-only window refuses, next to copy and delete.
