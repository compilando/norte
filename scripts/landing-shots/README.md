# Landing shots

Every screen on the landing (`landing/`) is norte itself, taken by these
scripts from the current build. `just landing-shots` retakes them all; commit
what changed under `landing/shots/` and `landing/public/shots/`.

| file | does |
| --- | --- |
| `shoot.sh` | the whole run: ada's home, plugins, consent, every scene × theme × language |
| `demo-tree.sh` | builds ada's home: fixed names, sizes and dates, so a diff of the shots means the product changed |
| `sandbox.sh` | runs a command as ada under `bwrap`: `/home` holds only her home, passwd says `ada`, the hostname is `norte` |
| `plugins.sh` | installs the official plugins the shots show |
| `tui.sh` | plays a scene against `ntc` in a detached tmux, keeps each screen as `.ansi` |
| `gui.sh` | plays the same scene against `norte-gui` on Xvfb, keeps PNGs and, with `RECORD`, a video |
| `scenes/*.scene` | the scripts: `run`, `keys`, `open`, `type`, `wait`, `shot`, `frame` |

Needs `bwrap`, `tmux`, `magick`, `zip`, `zstd`; for the window also `Xvfb`,
`xdotool`, `ffmpeg`.

**Why a sandbox and not `HOME=`.** Setting `HOME` does not isolate norte: the
session, the config and the runtime dir each have their own XDG variable. One
left pointing at the real home showed its folders in a shot and wrote the
shot's panes into the real `session.json`. `bwrap` hides `/home` entirely.

**Consent is given, not forged.** `scenes/approve.scene` approves the plugins
in the extension manager the way a person does. `y` on an approved row
REVOKES, which is why it runs once per fresh home, and why media-info is
left unapproved: `scenes/grant.scene` photographs the question.

**Never edit `shoot.sh` while a run is going**: bash reads a script as it
runs it. The scene files are read per step too; edit them between runs.

**A scene that steps around a bug says so, with the issue.** None does
today: the disk map (#372), the highlighted code (#373, #379), the marks
and the docked viewer in the window (#377, #378) were all put back once
fixed. Shooting found nine bugs; look at the shots as a user would.
