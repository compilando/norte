# Official plugins

Each directory here is a WASM plugin norte ships from source. They are **not**
workspace members: each has its own `Cargo.toml` with an empty `[workspace]`,
its own lockfile, and a `wit` symlink to `crates/norte-plugin-host/wit`, and
compiles to `wasm32-wasip2`. The gate builds and installs each one in a test
under `crates/norte-core/tests/`, so a plugin that stops building turns the
gate red.

| Directory | Id | Kind | What it does |
| --- | --- | --- | --- |
| `template/` | `org.example.template` | previewer + command | The smallest guest that builds. Copy it to start a plugin of your own — see [the author guide](../docs/plugins.md). |
| `git-status/` | `org.norte.git-status` | columns | A `git-status` column: which files changed against the index, read under the repository root the host confines it to (ADR 0057). |
| `file-icons/` | `org.norte.file-icons` | decorator | A badge per row saying what kind of file it is, from the name alone; `style = emoji` or `ascii`. |

The syntax-highlighting previewer (`org.norte.syntect`) lives with the host's
example guests, in `crates/norte-plugin-host/examples-wasm/previewer-syntect/`.

## Installing

```sh
just plugins            # build and install every official plugin
just plugins force      # replace ones already installed (withdraws their consent)
just plugin-git-status  # one of them
```

Installing does not approve: a plugin arrives discovered and unapproved, and
you approve its capabilities and switch it on in the extension manager (`F12`
in the TUI, the extensions panel in the window). `norte plugin list` shows the
same two facts; `norte plugin uninstall <id>` removes one and its approval.
