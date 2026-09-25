import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { pack, parseReel, parseScreen, type Packed } from "./ansi";
import type { Lang, Scene } from "./i18n";
import { PRESETS, type Theme } from "./product";

/**
 * What scripts/landing-shots/shoot.sh wrote, read at build time. The page is
 * static: nothing here runs in the browser.
 */

const ROOT = process.cwd();
const TUI = path.join(ROOT, "shots", "tui");
/** The width shoot.sh gives tmux; every screen is padded to it. */
export const COLS = 132;

function read(file: string): string | null {
  return existsSync(file) ? readFileSync(file, "utf8") : null;
}

export function tuiScene(lang: Lang, scene: Scene | "grant"): Packed | null {
  const raw = read(path.join(TUI, lang, `${scene}.ansi`));
  return raw === null ? null : pack(parseScreen(raw, COLS));
}

export function tuiTheme(lang: Lang, theme: Theme): Packed | null {
  const raw = read(path.join(TUI, lang, "themes", `${theme}.ansi`));
  return raw === null ? null : pack(parseScreen(raw, COLS));
}

export function tuiReel(lang: Lang): Packed[] {
  const raw = read(path.join(TUI, lang, "reel.ansi"));
  return raw === null ? [] : parseReel(raw, COLS).map(pack);
}

/** A theme's accent: the background of its selection, read from the preset. */
export function themeAccent(theme: Theme): string | null {
  const file = path.join(ROOT, "..", "crates", "norte-theme", "presets", `${theme}.toml`);
  const m = (read(file) ?? "").match(/^selection\s*=\s*\{[^}]*\bbg\s*=\s*"(#[0-9a-fA-F]{6})"/m);
  return m ? m[1] : null;
}

/** Window captures: public/shots/gui/<lang>/<name>.webp, when shoot-gui.sh ran. */
export function guiShot(lang: Lang, name: string): string | null {
  const rel = `/shots/gui/${lang}/${name}.webp`;
  return existsSync(path.join(ROOT, "public", rel)) ? rel : null;
}

export function guiShots(lang: Lang): string[] {
  const dir = path.join(ROOT, "public", "shots", "gui", lang);
  return existsSync(dir) ? readdirSync(dir).filter((f) => f.endsWith(".webp")) : [];
}

/**
 * The first chord each preset binds to each command, read from the preset
 * files themselves so the table cannot say what the program does not do.
 */
export function presetKeys(commands: string[]): Record<string, Record<string, string>> {
  const dir = path.join(ROOT, "..", "crates", "norte-frontend", "presets", "keymap");
  const out: Record<string, Record<string, string>> = {};
  for (const preset of PRESETS) {
    const text = read(path.join(dir, `${preset}.toml`)) ?? "";
    const keys: Record<string, string> = {};
    for (const m of text.matchAll(/on = \[([^\]]*)\], run = "([^"]+)"/g)) {
      const run = m[2];
      if (commands.includes(run) && !(run in keys)) keys[run] = m[1].replace(/"/g, "");
    }
    out[preset] = keys;
  }
  return out;
}
