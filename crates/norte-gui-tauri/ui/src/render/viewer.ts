// `Screen` painters for viewer (wave W10): functions with `this: Screen`,
// hooked in as properties in `render.ts`. State stays in the class.

import type { Screen } from "../render";
import type { MetadataSlotView, PreviewSlotView, ViewerView } from "../types";
import { nota, viewerBar, viewerBody, badge } from "./dom";
import type { SlotDom } from "./dom";

/** The viewer covers the screen while it is open. */
export function paintViewer(this: Screen, viewer: ViewerView | null): void {
  if (viewer === null) {
    this.viewerRoot.replaceChildren();
    this.viewerRoot.dataset["open"] = "false";
    this.viewerRows = 0;
    this.visorPintado = null;
    // The viewer closed: the buffer is RELEASED. An object URL that is not
    // revoked keeps its bytes alive as long as the document lives.
    this.soltarImagen();
    return;
  }
  // The same viewer in a window of the same size and with the same cell is
  // already painted: nothing built below would change. Reading the window
  // size and the root's variables does not force a reflow; measuring the
  // body, which is what the end of this function does, does.
  const cell = this.cell();
  const signature = `${String(window.innerWidth)}x${String(window.innerHeight)}|${String(cell.w)}x${String(cell.h)}`;
  const previous = this.visorPintado;
  if (previous?.viewer === viewer && previous.firma === signature) {
    return;
  }
  this.visorPintado = { viewer, firma: signature };
  if (
    previous !== null &&
    previous.firma === signature &&
    desplazado(previous.viewer, viewer)
  ) {
    // Only SCROLLED: same file, same header, same size. The body, the marks
    // and the bars are swapped in place. Redoing the whole box on every
    // wheel step, and measuring the new body — a reflow — is what made
    // scrolling heavy.
    const box = this.viewerRoot.querySelector<HTMLElement>(".viewer");
    const old = box?.querySelector<HTMLElement>(".viewer-body");
    const meta = box?.querySelector<HTMLElement>(".viewer-meta");
    if (box && old && meta) {
      meta.textContent = marcasDelVisor(this, viewer);
      box.querySelectorAll(".viewer-bar").forEach((b) => {
        b.remove();
      });
      const body = cuerpoDelVisor(viewer);
      old.replaceWith(body);
      ponerBarras(this, viewer, box, body);
      return;
    }
  }
  this.viewerRoot.dataset["open"] = "true";
  const box = document.createElement("section");
  box.className = "viewer";
  box.setAttribute("role", "document");
  box.setAttribute("aria-label", viewer.path_display);

  const head = document.createElement("header");
  head.className = "viewer-head";
  // The path in its own node, as in a slot's header and for the same
  // reason: loose as text it is an anonymous flex item that does not shrink,
  // so it pushed out of view whatever came after it — the "via …" and the
  // lossy-decoding notice — and they came out cut off.
  const path = document.createElement("span");
  path.className = "viewer-path";
  path.textContent = viewer.path_display;
  head.append(path);
  if (viewer.path_hostile) {
    path.append(badge(this.t("hostile-name")));
  }
  const meta = document.createElement("span");
  meta.className = "viewer-meta";
  meta.textContent = marcasDelVisor(this, viewer);
  head.append(meta);
  if (viewer.preview_by !== "") {
    // What is shown was produced by a PLUGIN. In its own node and with its
    // own color: a previewer can show anything at all — that is its job —
    // and whoever is looking has a right to know they are not seeing the
    // file's bytes.
    const via = document.createElement("span");
    via.className = "viewer-via";
    via.textContent = viewer.preview_by;
    head.append(via);
    if (viewer.preview_lossy) {
      // The decoding GIVEN to the previewer was lossy: the `?`s in its
      // output come from that and not from the file. Separate from
      // `had_errors`, which is the raw view's: they are two decodings, and
      // confusing them blames the file for what the read did.
      const notice = document.createElement("span");
      notice.className = "viewer-via-lossy";
      notice.textContent = this.t("viewer-plugin-preview-lossy");
      head.append(notice);
    }
  }

  if (viewer.image_refused !== "") {
    // An image was recognized and this window REFUSES to paint it. It is
    // said, instead of silently falling back to the hexview: a file the
    // user knows is a photo that shows up as bytes with no word about it
    // looks like norte is broken, not norte being careful.
    const no = document.createElement("p");
    no.className = "viewer-image-refused";
    no.setAttribute("role", "status");
    no.textContent = viewer.image_refused;
    head.append(no);
  }

  const body = cuerpoDelVisor(viewer);

  // The body and the VERTICAL bar go in a row; the HORIZONTAL one below both.
  // The bars are our own because the host only sends the visible window: the
  // `pre` measures what is shown and `overflow` has nothing to move.
  const canvas = document.createElement("div");
  canvas.className = "viewer-canvas";
  canvas.append(body);
  box.append(head, canvas);
  ponerBarras(this, viewer, box, body);
  // The wheel scrolls through the HOST, like the docked viewer and the log:
  // it decides the visible window. With `shift`, sideways — the usual gesture
  // for a horizontal axis, and the viewer does not wrap.
  box.onwheel = (e) => {
    e.preventDefault();
    const c = this.cell();
    const steps = (delta: number, cellSize: number): number => {
      if (delta === 0) {
        return 0;
      }
      // At least ONE: a trackpad sends deltas of a few pixels, and rounding
      // to zero turned the gesture into nothing.
      const n = Math.round(Math.abs(delta) / cellSize);
      return Math.sign(delta) * Math.max(1, n);
    };
    const lines = e.shiftKey ? 0 : steps(e.deltaY, c.h);
    const cols = e.shiftKey ? steps(e.deltaY, c.w) : steps(e.deltaX, c.w);
    if (lines !== 0 || cols !== 0) {
      this.send({ action: "viewer_scroll", lines, cols });
    }
  };
  if (viewer.image !== null) {
    // The bytes do NOT come in the frame: they are requested separately and
    // painted when they arrive. Until then the raw view shows, which is the
    // honest thing — that is what the file is — instead of an empty gap.
    this.pintarImagen(viewer, body);
  }
  this.viewerRoot.replaceChildren(box);
  // How many lines fit is known by WHOEVER PAINTS. The host used to estimate
  // it with layout cells minus a guessed chrome amount, so it sent more
  // lines than were visible — trimmed without saying so — and a page
  // advanced by a different number: every page skipped what had been
  // trimmed.
  const rows = Math.max(1, Math.floor(body.clientHeight / cell.h));
  if (rows !== this.viewerRows) {
    this.viewerRows = rows;
    this.send({ action: "set_viewer_rows", rows });
  }
  // And how many COLUMNS, for the same reason: it is what the host tells the
  // previewer (proto 0.66.0) next time, and the whole viewport counts the
  // chrome — an image shrunk to it spilled out on the right.
  const cols = Math.max(1, Math.floor(body.clientWidth / cell.w));
  if (cols !== this.viewerCols) {
    this.viewerCols = cols;
    this.send({ action: "set_viewer_cols", cols });
  }
}

/** Revokes the live `blob:`, if there is one. Idempotent. */
export function soltarImagen(this: Screen): void {
  if (this.imagenUrl !== null) {
    URL.revokeObjectURL(this.imagenUrl);
    this.imagenUrl = null;
  }
  this.imagenDe = null;
}

/**
 * Requests the image's bytes and paints it when they arrive.
 *
 * Once per file: the key is the path PLUS what the header declares, so
 * reopening the same file after changing it asks for it again, but any
 * repaint does not.
 *
 * The bytes already arrive validated by the host — format by magic bytes,
 * declared dimensions against the budget, size — so nothing is decided here:
 * it is wrapped and painted (ADR 0069).
 *
 * Only needs the BODY, not the frame: the image replaces the body in place,
 * whoever its parent is. Receiving the frame is what let one of the two
 * branches rebuild it and take out whatever was next to the body.
 */
export function pintarImagen(this: Screen, viewer: ViewerView, body: HTMLElement): void {
  const img = viewer.image;
  if (img === null) {
    return;
  }
  const key = `${viewer.path_display}|${img.format}|${String(img.width)}x${String(img.height)}`;
  if (this.imagenDe === key && this.imagenUrl !== null) {
    // Already requested — or painted — and it is the same one: not
    // requested again.
    //
    // The BODY is swapped for the image, in place, the same as the branch
    // below does when the bytes arrive. This used to rebuild the whole box
    // (`box.replaceChildren(header, image)`), and that took out everything
    // the box had besides the body: with the bars' canvas inside, the image
    // ended up a direct child of the frame and the slot was left with no
    // height. One branch rebuilding what another only replaces is the same
    // old divergence, inside a single function.
    body.replaceWith(this.nodoImagen(this.imagenUrl, img, viewer.image_zoom));
    return;
  }
  this.soltarImagen();
  this.imagenDe = key;
  void this.fetchImage()
    .then((bytes) => {
      // While it was in flight, the viewer could have changed or closed.
      // Painting the previous image over the current file is the same kind
      // of error as opening a viewer nobody asked for.
      if (this.imagenDe !== key || bytes.byteLength === 0) {
        return;
      }
      const url = URL.createObjectURL(new Blob([bytes]));
      this.imagenUrl = url;
      body.replaceWith(this.nodoImagen(url, img, viewer.image_zoom));
    })
    .catch(() => {
      // Without an image, the raw view stays, which is the real file.
      this.imagenDe = null;
    });
}

/**
 * The attribute sheet: label and value, and nothing else.
 *
 * Everything arrives formatted and sanitized from the host — the size with
 * its human shape and its exact number, the date in ISO, each attribute
 * through the same door as its column. Nothing is formatted here.
 */
export function paintMetadata(this: Screen, dom: SlotDom, slot: MetadataSlotView): void {
  // The title SAYS which listing it follows. Plain "Details" does not say
  // whose details they are, and with two listings open the only way to find
  // out was to move the cursor and watch whether the sheet moved.
  const title =
    slot.follows_display === ""
      ? this.t("metadata-title")
      : `${this.t("metadata-title")} · ${slot.follows_display}`;
  dom.root.setAttribute("aria-label", title);
  if (slot.follows_display === "") {
    dom.title.replaceChildren(document.createTextNode(title));
    return this.paintMetadataBody(dom, slot);
  }
  // The path in its own node and with the class that ellipsis-truncates it,
  // same as a listing's header: as loose text of the header, `.slot-title`
  // cuts it without saying so — `overflow: hidden` and nothing else — and a
  // path cut off flat names a different directory that also exists.
  // A HARD space behind the separator: `.slot-title` is a flex, and a normal
  // space at the end of a text node collapses against the span next to it —
  // "Details ·⟨file⟩/…" glued together.
  const label = document.createTextNode(`${this.t("metadata-title")} ·\xa0`);
const path = document.createElement("span");
  path.className = "title-path";
  path.textContent = slot.follows_display;
  dom.title.replaceChildren(label, path);
  if (slot.follows_hostile) {
    path.append(badge(this.t("hostile-name")));
  }
  return this.paintMetadataBody(dom, slot);
}

/** The sheet's body: the fields, or the note saying why there are none. */
export function paintMetadataBody(
  this: Screen,
  dom: SlotDom,
  slot: MetadataSlotView,
): void {
  dom.scroller.className = "metadata";
  if (slot.note !== "") {
    dom.scroller.replaceChildren(nota(slot.note));
    return;
  }
  const list = document.createElement("dl");
  list.className = "metadata-fields";
  for (const f of slot.fields) {
    const dt = document.createElement("dt");
    dt.textContent = f.label;
    const dd = document.createElement("dd");
    dd.dataset["hostile"] = String(f.hostile);
    dd.textContent = f.value;
    if (f.hostile) {
      dd.append(badge(this.t("hostile-name")));
    }
    list.append(dt, dd);
  }
  dom.scroller.replaceChildren(list);
}

/**
 * The docked viewer (#291): the same body as the full-screen viewer — it is
 * the same viewer elsewhere, as in the TUI — with the title's path and, if
 * there is no file, the note saying why.
 *
 * The line WINDOW that fits in the slot comes from wherever the host has
 * the viewer scrolled to: the viewer's wheel and keys move it through the
 * host, same as the full one.
 */
export function paintPreview(this: Screen, dom: SlotDom, slot: PreviewSlotView): void {
  const title =
    slot.viewer === null ? this.t("panelbar-viewer") : slot.viewer.path_display;
  dom.root.setAttribute("aria-label", title);
  dom.title.textContent = title;
  if (slot.viewer?.path_hostile === true) {
    dom.title.append(badge(this.t("hostile-name")));
  }
  dom.scroller.className = "preview";
  if (slot.viewer === null) {
    dom.scroller.onwheel = null;
    dom.scroller.replaceChildren(nota(slot.note));
    return;
  }
  // The wheel scrolls through the HOST, like the log: it decides the visible
  // window, and the viewer's keys — with the slot focused — move the same
  // scroll position.
  dom.scroller.onwheel = (e) => {
    e.preventDefault();
    this.send({
      action: "preview_scroll",
      slot_id: slot.slot_id,
      delta: e.deltaY > 0 ? 3 : -3,
    });
  };
  const via = slot.viewer.preview_by;
  const header: HTMLElement[] = [];
  if (via !== "") {
    // What is shown was produced by a PLUGIN: it is said, as in the full one.
    const mark = document.createElement("span");
    mark.className = "viewer-via";
    mark.textContent = via;
    header.push(mark);
  }
  dom.scroller.replaceChildren(...header, viewerBody(slot.viewer));
}

/**
 * The viewer header's marks, on one line.
 *
 * Kept apart because scrolling changes them (the row and the column)
 * without changing anything else in the header, and `paintViewer` rewrites
 * them on their own.
 */
function marcasDelVisor(screen: Screen, viewer: ViewerView): string {
  // Each mark is a DATUM the host resolved: encoding, line ending, whether
  // the user forced it, whether decoding had errors, whether the file was
  // truncated. None of it is computed here.
  const marks = [viewer.encoding, viewer.eol];
  if (viewer.hex) {
    marks.push("hex");
  }
  if (viewer.forced) {
    marks.push(screen.t("viewer-forced"));
  }
  if (viewer.had_errors) {
    // `viewer-lossy`, which is what this mark has been called in the
    // catalogue since the TUI's viewer has existed: making up `viewer-errors`
    // was asking for a key that is not there, and `t` answers with the key
    // itself.
    marks.push(screen.t("viewer-lossy"));
  }
  if (viewer.truncated) {
    marks.push(screen.t("viewer-truncated"));
  }
  // The row and the COLUMN, in words: it is what whoever cannot see the bars
  // reads, since they are visual indicators and are `aria-hidden`.
  marks.push(
    `${String(viewer.first_line + 1)}/${String(Math.max(1, viewer.total_rows))}`,
  );
  if (viewer.first_col > 0) {
    marks.push(
      `${String(viewer.first_col + 1)}/${String(Math.max(1, viewer.total_cols))}`,
    );
  }
  return marks.join(" · ");
}

/** The viewer's body, with what makes it focusable and announceable. */
function cuerpoDelVisor(viewer: ViewerView): HTMLElement {
  const body = viewerBody(viewer);
  body.setAttribute("tabindex", "-1");
  body.setAttribute("aria-describedby", `viewer-meta-${String(viewer.first_line)}`);
  return body;
}

/**
 * The viewer's bars: the vertical one next to the body, in its canvas; the
 * horizontal one at the bottom of the box.
 */
function ponerBarras(
  screen: Screen,
  viewer: ViewerView,
  box: HTMLElement,
  body: HTMLElement,
): void {
  // No bar at all over an IMAGE. An image file is read as hexadecimal
  // underneath, so the counts that arrive describe that dump — and drawing
  // them over the picture would be a bar talking about content that is not
  // on screen. The image is scaled to the slot: there is nothing to scroll.
  if (viewer.image !== null) {
    return;
  }
  const vertical = viewerBar(
    true,
    viewer.total_rows,
    viewer.first_line,
    screen.viewerRows,
  );
  if (vertical !== null) {
    body.parentElement?.append(vertical);
  }
  const horizontal = viewerBar(
    false,
    viewer.total_cols,
    viewer.first_col,
    screen.viewerCols,
  );
  if (horizontal !== null) {
    box.append(horizontal);
  }
}

/**
 * `now` is `before` scrolled: the same file read the same way, with the
 * same header except for the marks. Never an image, because its body gets
 * replaced by the picture when it arrives.
 */
function desplazado(before: ViewerView, now: ViewerView): boolean {
  return (
    before.image === null &&
    now.image === null &&
    before.path_display === now.path_display &&
    before.path_hostile === now.path_hostile &&
    before.preview_by === now.preview_by &&
    before.preview_lossy === now.preview_lossy &&
    before.image_refused === now.image_refused
  );
}
