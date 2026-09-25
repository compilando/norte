/**
 * The facts the page repeats. One place, so a release bump is one edit and the
 * hero, the download tiles and the footer cannot drift apart. Each number is
 * read from the repository; where from is written next to it.
 */

export const REPO = "https://github.com/compilando/norte";

export const RELEASE = {
  /** Cargo.toml, `version`. */
  version: "0.3.0-alpha.4",
  label: "v0.3 alpha",
  /** crates/norte-proto/src/methods.rs, `PROTOCOL_VERSION`. */
  protocol: "0.84.0",
  /** GitHub renames the window packages on every release (the version is in the
   *  file name), so the tiles point at the release page, never at a fixed asset. */
  latest: `${REPO}/releases/latest`,
  all: `${REPO}/releases`,
  tuiInstaller: `${REPO}/releases/latest/download/norte-tui-installer.sh`,
  cliInstaller: `${REPO}/releases/latest/download/norte-cli-installer.sh`,
} as const;

/** crates/norte-frontend/src/keymap/catalogue.rs, the `live(…)` entries. */
export const COMMANDS = 190;

/** crates/norte-theme/presets/*.toml, in the order the gallery shows them. */
export const THEMES = [
  "catppuccin-mocha",
  "default",
  "nord",
  "gruvbox-dark",
  "vscode-dark",
  "retro-crt",
  "retro-crt-amber",
  "catppuccin-latte",
  "gruvbox-light",
  "vscode-light",
] as const;
export type Theme = (typeof THEMES)[number];

/** crates/norte-frontend/presets/keymap/*.toml */
export const PRESETS = ["orthodox", "vim", "cua", "krusader", "far", "norton", "total-commander"] as const;

export const LINKS = {
  docs: `${REPO}/tree/main/docs`,
  readme: `${REPO}#readme`,
  source: `${REPO}#development`,
  architecture: `${REPO}/blob/main/ARCHITECTURE.md`,
  spec: `${REPO}/blob/main/docs/spec/norte-spec.md`,
  adr: `${REPO}/tree/main/docs/adr`,
  changelog: `${REPO}/blob/main/CHANGELOG.md`,
  contributing: `${REPO}/blob/main/CONTRIBUTING.md`,
  security: `${REPO}/blob/main/SECURITY.md`,
  issues: `${REPO}/issues`,
  plugins: `${REPO}/blob/main/docs/plugins.md`,
  theming: `${REPO}/blob/main/docs/theming.md`,
  licensing: `${REPO}#licensing`,
} as const;
