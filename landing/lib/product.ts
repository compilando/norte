/**
 * The facts the page repeats. One place, so a release bump is one edit and the
 * hero, the download tiles and the footer cannot drift apart.
 */

export const REPO = "https://github.com/compilando/norte";

export const RELEASE = {
  version: "0.3.0-alpha.4",
  label: "v0.3 alpha",
  protocol: "0.81.0",
  /** GitHub renames the window packages on every release (the version is in the
   *  file name), so the tiles point at the release page, never at a fixed asset. */
  latest: `${REPO}/releases/latest`,
  all: `${REPO}/releases`,
  tuiInstaller: `${REPO}/releases/latest/download/norte-tui-installer.sh`,
  cliInstaller: `${REPO}/releases/latest/download/norte-cli-installer.sh`,
} as const;

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
