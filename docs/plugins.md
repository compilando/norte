# Plugin author guide

norte loads plugins as WebAssembly components. A plugin is a directory with a
manifest and a binary; the host decides what the binary may do from the
manifest, a human approves that, and only then does anything run. This guide
takes you from an empty directory to a plugin running in norte, and tells you
what the host will and will not do for you.

Start from [`plugins/template/`](../plugins/template/): it is the smallest
plugin that builds, and every file in it is commented.

## What a plugin is

A **WASM component** implementing one of norte's WIT worlds, plus a
`plugin.toml`. The component runs in a sandbox with no filesystem, no
network and no way to run a program — it gets exactly the capabilities its
manifest declares, and those are what the human approves. The host mediates
everything: it reads the file and hands you the bytes, it opens the socket,
it confines the directory you may read under.

Seven kinds, one world each, chosen by `category` in the manifest:

| Kind | World | You export | It gives the user |
| --- | --- | --- | --- |
| `previewer` | `norte-plugin` | `previewer` (and `command`) | a rendering of a file in the viewer, plain or styled |
| `command` | `norte-plugin` | `command` (and `previewer`) | a verb in the palette, run with an argument |
| `decorator` | `norte-decorator` | `decorator` | an icon left of the name, or a badge and a theme role right of it, on each row of a listing (ADR 0105) |
| `columns` | `norte-columns` | `columns` | a value per entry for a column the user adds |
| `provider` | `norte-provider` | `provider` | a backend behind a URL scheme of your own (`webdav://…`) |
| `renamer` | `norte-renamer` | `renamer` | a proposed new name per marked entry, reviewed before anything is renamed (ADR 0095) |
| `hook` | `norte-hook` | `hook` | a sentence in the status bar after a mutation the journal recorded — it observes, it never vetoes (ADR 0100) |

The `norte-plugin` world exports both `previewer` and `command`; a plugin of
one of those kinds implements the other as "not supported". The WIT files
are in [`crates/norte-plugin-host/wit/`](../crates/norte-plugin-host/wit/),
and the header of `norte-plugin.wit` records every change to them.

## The manifest, field by field

```toml
[plugin]
id = "org.example.mine"        # reverse-DNS, 1–128 chars, your namespace
name = "Mine"                  # shown in the manager; masked as third-party text
publisher = "example"
version = "0.1.0"              # informative
category = "previewer"         # one of the six kinds
description = "…"              # optional, ≤ 280 chars, not in the approval digest

[contributions]                # what the plugin adds; see per kind below

[capabilities]                 # what it asks for; see below

[config.some-key]              # optional settings; see below
```

`id` names the install directory and the approval; choose it once.
`description` is the one cosmetic field: editing it does not invalidate an
approval.

### Contributions per kind

```toml
# previewer: which files, by the mimetype norte guesses from the extension
previewer = [{ mimetypes = ["text/*", "application/json"] }]

# command: verbs in the palette; `id` is what `norte plugin run` names
[[contributions.command]]
id = "hello"                   # ≤ 64 chars
title = "Say hello"            # ≤ 120 chars; at most 32 commands

# decorator: the host asks you about every visible page, name and kind
# (file, dir, symlink, other). `slot` says where your glyph goes: `icon`
# is the fixed-width column LEFT of the name, `badge` (the default) the
# git-status place to its right. One row can carry both, from two plugins.
[[contributions.decorator]]
slot = "icon"

# thumbnail (ADR 0107): which files you can turn into a small raster for the
# window's viewer, by mimetype like a previewer. You get the file's bytes
# (capped at 8 MiB) and the longest edge allowed; you answer PNG, JPEG or
# WebP bytes with their dimensions, and the host checks all three before
# painting. The terminal ignores this kind.
[[contributions.thumbnail]]
mimetypes = ["image/png", "image/jpeg"]

# columns: each column the user can add, by id, with its header
[[contributions.columns]]
id = "dims"
header = "Dims"

# provider: the scheme you serve, and the port the host grants when the URL
# has none. `file`, `sftp`, `ftp`, `s3` and archive formats are the core's.
[[contributions.provider]]
scheme = "webdav"
default-port = 8443

# renamer: each way of renaming you offer, by id, with the title the palette
# shows under `[rename]`. `plan(id, location, names)` gets the id back.
[[contributions.renamer]]
id = "by-date"
title = "Prefix with modification date"

# hook: the journal events you listen to, one of after-created,
# after-removed, after-trashed, after-renamed, after-mode-changed. Only
# `after-*`: a hook sees what already happened (ADR 0100).
[[contributions.hook]]
on = "after-renamed"
```

A renamer never renames. It returns `{ current, proposed }` pairs for the
names it was given; the host drops identity pairs and anything it did not
ask about, and the human reviews the plan in the same screen the AI plan
uses — with the core checking every target, journaling and undo as for any
batch rename. Reading the files (EXIF, ID3, a modification time) is what
the `location` capability is for: without it, `plan` gets no location and
should say so in its error rather than guess — the sentence you return in
`Err` reaches the user's status bar (masked and capped at 200 characters),
so write it for them: "approve the `location` capability", not a stack
trace.

A hook never changes anything. `on-events` receives the journal entries
since the last call that match your `on` — `op`, who caused it (`user`,
`agent` or `plugin`, never which agent), the path in wire form (without
userinfo), the old name of a rename in `path-to`, the leaf name in raw
bytes, and the batch id when it was part of one — plus `dropped`, how many
events norte's queue lost since your last call: when it is not zero, count
with "at least". It returns effects; the only one is `notify(text)`, a
sentence for the status bar that norte masks, caps and prefixes with your
plugin id — one per call, four in a burst, then one per second. With
`location = "read"` each event also carries a token for the entry's parent
directory, so you can `stat` the result; never for `$HOME` or `/`. A hook
may not declare `net`. With `fs-write = { sidecar = [names] }` a hook may
also return `write-sidecar { seq, name, content, if-exists }`: norte writes
`name` (one of the declared ones) in the parent directory of event `seq`, as
a plugin actor through the policy engine, journaled and undoable —
`replace` sends the previous file to the trash first. A rule
`actor = "plugin", action = "deny"` in `policy.toml` stops every such write,
and the reader is told once (ADR 0101). Fail three calls in a row — a trap, a timeout, an
`Err` — and norte switches your hooks off and tells the reader; disabling
and re-enabling the plugin re-arms it. The events you listen to show at
approval as `hook:after-renamed`-style badges. There is no `before-*`, on
purpose: a veto is a policy decision, not a plugin's.

Contributions are part of the approval digest: they say *when* and *how*
the plugin fires, which is as much a part of what the human approves as the
capabilities are.

### Capabilities, and what approving shows

Everything in `[capabilities]` shows as a badge when the human approves.
Absent means denied.

| Key | Values | What it grants |
| --- | --- | --- |
| `fs-read` | `"scoped"` | the `host-log::read-scoped` door: read a blob the host seeded under a token. A previewer gets the file's bytes in `preview-input` without it — declare it only if you call `read-scoped` |
| `fs-write` | `{ sidecar = [".norte-renames.log"] }` | **hooks only** (ADR 0101): the exact file names the hook may ask norte to write next to what changed, via `write-sidecar`. norte writes them as a plugin actor through the policy engine and the journal; `replace` trashes the old file first. At most 16 names, 64 KiB each. `"scoped"` is rejected. |
| `net` | `{ hosts = ["203.0.113.5", "198.51.100.7:8443", "[2001:db8::1]:443"] }` | outbound TCP to exactly those addresses (bare IP = any port). Matched as text against the address the guest connects to, so write IPv6 the way Rust prints it (`::1`, `[::1]:443`). A provider also gets the connection's own `ip:port`, resolved by the host. No DNS: the guest connects by IP. |
| `location` | `"read"` | `read`, `read-prefix` (at most N bytes — what a header needs), `stat` and `list-dir` under an opaque token for the directory being listed (columns). Every call and every byte is charged against a per-page budget; the guest never learns the path. |
| `location-root-marker` | `".git"` | with `location`: the host opens the nearest ancestor containing that name instead of the listed directory, and tells you the prefix |
| `exec` | `"none"` or absent | there is no other value. A plugin never runs a program. |

One declaration is rejected at parse time because nothing honours it yet:
`ai`. A manifest with it does not install, and the error says so. A hook
event outside the five that exist is rejected the same way, with the value.

A provider's claimed scheme shows among the badges as `provider:webdav`.

### `[config]`

Settings the user edits in the manager and your guest reads through
`host-config::get`/`all`. Four types, each with a validated default:

```toml
[config.greeting]
type = "string"        # ≤ 280 chars
default = "hello"
description = "…"      # ≤ 280 chars

[config.verbose]
type = "bool"
default = false

[config.width]
type = "int"
default = 80
min = 20
max = 200

[config.style]
type = "enum"
values = ["emoji", "ascii"]   # ≤ 16
default = "emoji"
```

At most 32 keys, names `[a-z0-9-]{1,32}`. The schema is part of the approval
digest; the *values* are not: the host writes them to `config.toml` next to
your manifest, validates them against the schema at discovery, and hands
them to your guest as strings before every call. A value that does not
validate excludes the plugin entirely rather than loading it half-configured.

## Directory layout

```
~/.config/norte/plugins/org.example.mine/
├── plugin.toml     # yours
├── plugin.wasm     # yours: the built component, always this name
├── help.md         # yours, optional: a help page (see below)
└── config.toml     # the host's: the user's values for [config]
```

The host reads only these names and only inside this directory: a
`plugin.wasm` or `help.md` that is a symlink to somewhere else is treated as
absent.

## Building

You need the `wasm32-wasip2` target and a `wit` directory next to your
`Cargo.toml` — inside the norte repository a symlink to
`crates/norte-plugin-host/wit`, outside it a copy.

```sh
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
```

The template's `Cargo.toml` is the whole recipe: `[workspace]` empty so
cargo does not look upwards, `crate-type = ["cdylib"]`, `wit-bindgen`, and a
release profile with `panic = "abort"` and `opt-level = "s"`. In `lib.rs`:

```rust
wit_bindgen::generate!({
    world: "norte-plugin",
    path: "wit",
    generate_all,   // the world imports interfaces from norte:host
});
```

A styled previewer returns lines of `span`s. A span carries `text`, an
optional `role` (a name from norte's theme — `keyword`, `number`, `title`,
`info` — which the host paints with whatever colour the user's theme gives
it), an optional `fg` and, since `norte:plugin@0.9.0`, an optional `bg`, both
`(r, g, b)`. When a role and a colour are both present, the role wins for the
foreground; `bg` is painted as given. Foreground and background together are
what lets a previewer draw pixels: the `▀` half block with `fg` for the top
pixel and `bg` for the bottom one. `preview-input.columns` is the width of
the viewer in cells when the host knows it (the TUI sends the terminal's
width, the window its viewport's); `none` means no hint, and the guest picks
its own width. Treat it as a hint, never as a promise of a buffer that size.

The host caps what a guest may return: 4 MiB per call, 10 000 styled lines,
256 spans per line, 4 KiB of text per span, 8 cells per decorator badge,
1024 log lines of 4 KiB. Exceeding a cap rejects the whole answer; nothing is
truncated silently. A call has 10 seconds of wall-clock; a guest that runs
out reports "out of time", not "crashed". The store is limited to 64 MiB and
the binary to 64 MiB.

## Installing and consenting

```sh
mkdir -p stage
cp plugin.toml help.md stage/
cp target/wasm32-wasip2/release/<crate_name>.wasm stage/plugin.wasm
norte plugin install stage            # refuses to replace an installed id
norte plugin install stage --force    # replaces it, and withdraws its consent
norte plugin list
norte plugin uninstall org.example.mine
```

**Installing is not consenting.** The plugin arrives discovered and
unapproved. A human approves its capabilities and switches it on in the
extension manager (`F12` in the TUI, the extensions panel in the window),
and both facts are shown separately: you can approve and leave off.

The approval is anchored to the manifest **and** the binary. Editing either
after approval withdraws it: `--force` and `uninstall` withdraw it on
purpose, and a rebuilt `plugin.wasm` is a new binary and is approved again.
Do not fight this; it is what makes "I approved that plugin" mean something.

## Seeing what the host thinks

- `norte plugin list`: id, kind, approved, enabled, the badges, and a count
  of broken plugins.
- `norte doctor`: every plugin, with what is wrong — a manifest that does not
  parse, a digest that no longer matches the approval, a missing binary, a
  `config.toml` value outside its schema, a help page over the size cap, a
  binary built against another WIT.
- The manager shows a broken plugin as a row with its reason, never hides it.
- Your `host-log::log` lines land in norte's log (the log panel in both
  frontends, `RUST_LOG` on the CLI).

## Help pages

A `help.md` next to the manifest becomes a page in norte's help, in the
extensions group, marked as written by a plugin. Front matter, then Markdown:

```markdown
+++
id = "org.example.mine"
title = "Mine"
+++
What the plugin does, in a paragraph or two.
```

64 KiB at most; the text is treated as third-party throughout (bounded,
decoded, masked). A page may not reference norte's own commands as if they
were its own.

## WIT compatibility (ADR 0094)

The version of a WIT package is part of every interface name your component
imports or exports (`norte:plugin/previewer@0.9.0`). **When norte moves a
package, every binary built against the previous version stops loading.**
This is a fact of the component model; what norte promises is what it does
about it:

- The host serves exactly one version of each package. There is no
  compatibility window.
- The catalogue reads your binary's imports and exports. A version the host
  does not serve lists the plugin as broken — in the manager, in
  `norte plugin list`, and in `norte doctor` as `plugin-wit-mismatch` naming
  the package and both versions. It is never loaded and never silently
  skipped.
- The CHANGELOG entry that bumps a package says which one, its new version,
  and "plugins built against `norte:plugin@X` need a rebuild".
- Rebuild against the new `wit/`, reinstall with `--force`, approve again.

## Walk-through: from an empty directory to a running command

Done on a machine with the norte binary on `PATH` and the repository checked
out at `$NORTE`:

```sh
cp -r "$NORTE/plugins/template" mine && cd mine
rm wit && cp -r "$NORTE/crates/norte-plugin-host/wit" wit
sed -i 's/org.example.template/org.example.walk/' plugin.toml help.md
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
mkdir -p stage && cp plugin.toml help.md stage/
cp target/wasm32-wasip2/release/norte_plugin_template.wasm stage/plugin.wasm
norte plugin install stage
norte plugin list                       # org.example.walk  previewer  NOT approved  off  fs-read
norte plugin run org.example.walk hello world
# plugin run failed: not found         <- unapproved: the plugin does not exist to the runner
ntc                                     # F12, approve, switch on
norte plugin run org.example.walk hello world
# hello, world
```

Two things the first run of this taught: outside the repository the default
toolchain is whatever `rustup default` says, and `rustup target add` applies
to that one — the repository pins its own; and `norte plugin run` on an
unapproved plugin answers "not found", not "not approved", because to the
runner an unconsented plugin is indistinguishable from an absent one.
