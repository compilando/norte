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

Five kinds, one world each, chosen by `category` in the manifest:

| Kind | World | You export | It gives the user |
| --- | --- | --- | --- |
| `previewer` | `norte-plugin` | `previewer` (and `command`) | a rendering of a file in the viewer, plain or styled |
| `command` | `norte-plugin` | `command` (and `previewer`) | a verb in the palette, run with an argument |
| `decorator` | `norte-decorator` | `decorator` | a badge and a theme role on each row of a listing |
| `columns` | `norte-columns` | `columns` | a value per entry for a column the user adds |
| `provider` | `norte-provider` | `provider` | a backend behind a URL scheme of your own (`webdav://…`) |

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
category = "previewer"         # one of the five kinds
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

# decorator: no fields; the host asks you about every visible page
[[contributions.decorator]]

# columns: each column the user can add, by id, with its header
[[contributions.columns]]
id = "dims"
header = "Dims"

# provider: the scheme you serve, and the port the host grants when the URL
# has none. `file`, `sftp`, `ftp`, `s3` and archive formats are the core's.
[[contributions.provider]]
scheme = "webdav"
default-port = 8443
```

Contributions are part of the approval digest: they say *when* and *how*
the plugin fires, which is as much a part of what the human approves as the
capabilities are.

### Capabilities, and what approving shows

Everything in `[capabilities]` shows as a badge when the human approves.
Absent means denied.

| Key | Values | What it grants |
| --- | --- | --- |
| `fs-read` | `"scoped"` | receive the bytes of the file the host already read (previewers) |
| `fs-write` | `"scoped"` | reserved; no host door uses it yet |
| `net` | `{ hosts = ["203.0.113.5", "198.51.100.7:8443"] }` | outbound TCP to exactly those addresses (bare IP = any port). A provider also gets the connection's own `ip:port`, resolved by the host. No DNS: the guest connects by IP. |
| `location` | `"read"` | read, stat and list under an opaque token for the directory being listed (columns). The guest never learns the path. |
| `location-root-marker` | `".git"` | with `location`: the host opens the nearest ancestor containing that name instead of the listed directory, and tells you the prefix |
| `exec` | `"none"` or absent | there is no other value. A plugin never runs a program. |

Two declarations are rejected at parse time because nothing honours them
yet: `ai`, and the `hook` category. A manifest with either does not install,
and the error says so.

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

The host caps what a guest may return: 4 MiB per call, 10 000 styled lines,
64 spans per line, 4 KiB of text per span, 8 cells per decorator badge,
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
imports or exports (`norte:plugin/previewer@0.8.0`). **When norte moves a
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
