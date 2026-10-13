# 0183 - The release check is opt-in

- Status: accepted
- Date: 2026-10-13
- Decision makers: Oscar González
- Related: spec §16.6 (no telemetry), §17 ("update notification without
  unattended installation"), ADR 0182, ADR 0035 (what a project layer may
  set).

## Context and problem statement

Spec §17 asks for update notification without unattended installation. A
reader on alpha.9 never learns that alpha.12 exists unless they visit the
releases page. The only way to know is to ask the network, and §16.6
promises that nothing leaves the machine unless the reader chooses.

## Considered options

1. **On by default**, with `check = false` to turn it off. More readers hear
   about releases, but norte talks to GitHub without anyone asking.
2. **Opt-in**: off unless the first-run wizard or Settings turns it on.
3. **Only by hand**: `norte update check` and nothing in the background.

## Decision

Option 2.

- `[updates] check = true` in `norte.toml`. Absent means off. The project
  layer and profiles cannot set it, as with `[daemon]` and `[log]`: a
  repository does not decide that this machine talks to the network.
- **The wizard asks**, as its fourth step, with the cursor on "no": pressing
  Enter without reading keeps norte off the network. Settings has the
  toggle (`updates.check`, Behaviour).
- **At most once a day.** The answer is cached in
  `<state>/update-check.json`, and every start in between reads the cache.
  The cache keeps the highest stable release and the highest release
  overall. A reader on a prerelease is offered prereleases, and a reader on
  a stable release only stable ones.
- **It says, never installs.** One line in the status bar of the terminal
  and the window, composed once in `norte_frontend::banners::update_line`.
  `norte update check` asks whatever the setting says, because the reader
  asked, and prints the release's URL.
- **The logic lives in `norte-update`, not the core.** The window may not
  depend on `norte-core` (ADR 0066), and all three binaries need the same
  answer.
- A failed request answers from the old cache if there is one. Without a
  cache, no notice. A frontend never shows an error for it.

## Consequences

- The request reveals the reader's IP and norte's version (as the
  User-Agent) to GitHub, once a day, and only when the reader turned the
  check on. The wizard's question and the setting's description say so.
- The release list is untrusted input. It is capped at 2 MiB and 10 s, and
  a tag reaches the screen only if it parses as semver. The cache file is
  re-parsed the same way when read.
- A request that fails stamps the cache too, so a machine offline asks
  once a day, not on every start.
- Two exemptions from the hard rules, stated so nobody re-asks:
  - **`std::fs` outside `norte-vfs-local` (rule 2).** The cache is a
    state-directory file, like `norte_config::command_history`. It is
    always read and written inside `spawn_blocking`.
  - **No cancellation token (rule 3).** The check is one request bounded
    by a 10 s timeout. Building the `reqwest` client on the executor may
    read the root certificates, as in `norte-ai`.
- `RELEASES_URL` names `compilando/norte`. A move or a fork changes one
  constant.
- Not done: no package-manager integration and no in-app download, which
  §17 excludes ("without unattended installation").
