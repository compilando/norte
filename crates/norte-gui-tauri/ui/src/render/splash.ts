// `Screen` painter for the startup screen (bridge 69, ADR 0115): a function
// with `this: Screen`, hooked in as a property in `render.ts`. State stays
// in the class.
//
// What gets shown is decided by the host (`norte_frontend::splash`), so this
// screen says the same thing as the terminal's; only where each thing lands
// and who gets the click is chosen here.

import type { Screen } from "../render";
import type { SplashView } from "../types";

/**
 * `brief` mode's deadline is MET by the renderer: there is no event loop to
 * wake the host up, and the number comes in the screen itself
 * (`close_after_ms`). Lives in `Screen` — not in this module — and is armed
 * once per appearance; see `Screen.splashPlazo`.
 */
export function paintSplash(this: Screen, splash: SplashView | null): void {
  if (splash === null) {
    // It is gone (a key, a click, its own deadline): disarm whatever was
    // left, or a live timer would send the close for a screen that is no
    // longer there and burn a sequence number.
    if (this.splashPlazo !== null) {
      clearTimeout(this.splashPlazo);
      this.splashPlazo = null;
    }
    this.splashPuesto = false;
    this.splashRoot.replaceChildren();
    this.splashRoot.dataset["open"] = "false";
    return;
  }
  this.splashRoot.dataset["open"] = "true";

  const box = document.createElement("section");
  box.className = "splash";
  box.setAttribute("role", "dialog");
  box.setAttribute("aria-modal", "true");
  box.setAttribute("aria-label", this.t("splash-title"));
  // COVER: `brief` comes with NO sections on purpose, and with no list to
  // frame, a centered box is a frame around nothing. Same signal the
  // terminal uses, so both surfaces decide the same way without the mode
  // having to travel over the bridge.
  if (splash.sections.length === 0) {
    box.dataset["cover"] = "true";
  }

  // The art, in its own block and HIDDEN for whoever reads with their ears:
  // a compass drawn with bars and dashes spells out as noise.
  const art = document.createElement("pre");
  art.className = "splash-art";
  art.setAttribute("aria-hidden", "true");
  art.textContent = splash.art.join("\n");
  box.append(art);

  const version = document.createElement("p");
  version.className = "splash-version";
  version.textContent = `${splash.version} ${splash.revision}`.trim();
  box.append(version);

  const daemon = document.createElement("p");
  daemon.className = "splash-daemon";
  daemon.textContent = splash.daemon;
  box.append(daemon);

  for (const section of splash.sections) {
    const title = document.createElement("h2");
    title.className = "splash-section";
    title.textContent = section.title;
    box.append(title);

    const list = document.createElement("ul");
    list.className = "splash-rows";
    for (const row of section.rows) {
      const li = document.createElement("li");
      li.className = "splash-row";
      // The number the row shows, or a gap: past nine the row reads and
      // promises nothing, same as in the terminal.
      const num = document.createElement("span");
      num.className = "splash-number";
      num.textContent = row.number === 0 ? "" : String(row.number);
      const label = document.createElement("span");
      label.className = "splash-label";
      label.textContent = row.label;
      const detail = document.createElement("span");
      detail.className = "splash-detail";
      detail.textContent = row.detail;
      li.append(num, label, detail);
      if (row.number !== 0) {
        // A click on a numbered row opens it. The rest are not buttons and
        // are not announced as such: a click there only dismisses the
        // screen, which is what a click anywhere else does too.
        li.dataset["actionable"] = "true";
        li.addEventListener("click", (e) => {
          e.stopPropagation();
          this.send({ action: "splash_activate_row", number: row.number });
        });
      }
      list.append(li);
    }
    box.append(list);
  }

  const footer = document.createElement("p");
  footer.className = "splash-hint";
  footer.textContent = splash.hint;
  box.append(footer);

  // A click ANYWHERE dismisses it. Set on the root and not the box so the
  // surrounding air counts too: dismissing it is what is being attempted.
  this.splashRoot.onclick = (): void => {
    this.send({ action: "splash_close" });
  };
  this.splashRoot.replaceChildren(box);

  // The deadline is armed on the FIRST paint of this appearance and is not
  // touched again: the host sends the whole view on every patch, so
  // re-arming it here turned "1.2 seconds" into "1.2 seconds after the last
  // patch" — and during startup patches keep arriving nonstop, which is
  // exactly when this screen is up.
  const remaining = splash.close_after_ms;
  if (!this.splashPuesto && remaining !== null && remaining !== undefined) {
    this.splashPlazo = setTimeout(() => {
      this.splashPlazo = null;
      this.send({ action: "splash_close" });
    }, remaining);
  }
  this.splashPuesto = true;
}
