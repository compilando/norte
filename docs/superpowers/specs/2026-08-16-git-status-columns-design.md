# Git status as the official columns plugin: design

> Roadmap post-alpha item **6**; specification §17, "ship status as an official
> columns plugin, not a Git client in the core". M4's exit criterion is that a
> third party can ship a plugin without changing the core. This is the proof —
> and the proof fails on the first try, which is the point of running it here
> rather than learning it from a third party.

## The interface does not reach

A columns plugin today exports one function
(`crates/norte-plugin-host/wit/norte-plugin.wit`, `norte:plugin@0.7.0`):

```wit
column-values: func(id: string, entries: list<list<u8>>) -> list<option<string>>;
```

Four gaps, in order of severity:

1. **`entries` are basenames, never paths.** `paths_to_basenames`
   (`crates/norte-core/src/plugins.rs:194`) takes the file name and drops the
   rest — an explicit privacy decision: a columns plugin sees *what* is visible,
   not *where* you are. A git plugin cannot find a repository it cannot locate.
2. **No filesystem at all.** The `norte-columns` world imports only `host-log`
   and `host-config`. `read-scoped` exists but reads a token the host seeded,
   and the columns path (`crates/norte-core/src/daemon/server.rs:2930`) seeds
   none.
3. **`exec` is permanently `none`** (`capability.rs:3`, ADR 0022 D4). The plugin
   cannot run `git`. This is not negotiable and not worked around.
4. **A fresh instance per call**, with a 50 ms epoch deadline
   (`crates/norte-plugin-host/src/runtime.rs`). No cache survives a page turn,
   and parsing a large `.git/index` does not fit in 50 ms.

So item 6 is two pieces of work: **grow the interface**, then **write the
plugin**. The first is the valuable one.

## Scope

**In:** a location capability that lets an approved plugin read *under* the
directory being listed without learning where that is; instance reuse with a
category deadline; a `org.norte.git-status` plugin computing worktree-versus-index
status; installation the way a third-party plugin would be installed.

**Deliberately out:**

- **Staged status (index versus HEAD).** It needs an object-database reader —
  zlib for loose objects, `.idx` lookups, offset and reference delta chains —
  inside a wasm guest. Worktree-versus-index needs none of that, and covers the
  question the column is actually asked ("did I touch this?").
- **Anything but `file://`.** A location token is minted only for local paths.
  Elsewhere the cells are empty, which is what an inapplicable column already
  means.
- **Git operations.** No staging, no commits, no branches switched. A column.
- **Submodules, worktrees-of-worktrees, `GIT_DIR` indirection beyond a plain
  `.git` directory or a `gitdir:` file.** Unrecognised shapes yield empty cells.
- **Index version 4.** Path prefix compression, refused by name.

## Approaches considered

**Host resolves the repository and hands over the bytes** — rejected. It puts
"what a git repository is" in the core, which is the one sentence §17 writes.

**Give the plugin the directory path plus a scoped read token** — rejected,
narrowly. It works and it is simpler to debug, but it reverses the privacy
decision at `plugins.rs:194` for every columns plugin forever, so that a plugin
that wants to know your directory layout only has to ask for a column.

**An opaque location token** — chosen. The host mints a token bound to the
directory; the guest reads, stats and lists *relative* to it and never learns
the absolute path. The privacy decision survives, and rule 9 survives too: the
host opens every byte.

## Architecture

### A new WIT package, not a change to `norte:host`

The package version travels inside every interface name, so a bump to
`norte:host` invalidates every compiled guest — documented twice in the WIT
header and verified empirically both times, which is why ADR 0041 decision 4
split the packages in the first place. The location surface is new and will
move; it gets its own package.

```wit
package norte:location@0.1.0;

interface location {
    record meta {
        kind: entry-kind,          // file, dir, symlink, other
        size: u64,
        mtime-sec: s64, mtime-nsec: u32,
        ctime-sec: s64, ctime-nsec: u32,
        ino: u64, dev: u64,
        mode: u32,
    }
    record dirent { name: list<u8>, kind: entry-kind }

    read: func(token: string, rel: list<u8>) -> result<list<u8>, string>;
    stat: func(token: string, rel: list<u8>) -> result<meta, string>;
    list: func(token: string, rel: list<u8>) -> result<list<dirent>, string>;
}
```

`rel` is bytes, not `string`: hard rule 1. `meta` carries exactly the stat
fields git's own index stores, so the guest can do git's comparison rather than
an approximation of it.

`columns::column-values` gains the token:

```wit
column-values: func(id: string, location: option<string>,
                    entries: list<list<u8>>) -> list<option<string>>;
```

`norte:plugin` 0.7.0 → **0.8.0**. This breaks every `.wasm` compiled against
0.7.0, by both export and import shape. That is the known, twice-verified cost
of any bump to this package; the in-tree guests are recompiled in the same
change, as before.

### Enforcement in the host

- The token is **minted per call and dies with it.** It is a random opaque
  string in a map on the host side; nothing about the path is derivable from it.
- Resolution goes through `norte-vfs-local`'s confined opener
  (`crates/norte-vfs-local/src/confined.rs`, the `openat2(RESOLVE_BENEATH)` path
  that closed #164). A `..`, an absolute `rel`, or a symlink pointing out of the
  directory is refused **by the kernel**, not by a check someone can forget.
- Bounds, all configurable, all fail-closed: bytes per `read`, calls per
  invocation, total bytes per invocation, entries per `list`.
- Only for a plugin whose manifest declares the new capability
  `location = "read"`. It joins the approval digest, so an already-approved
  plugin that adds it needs approving again, and it shows as its own badge in
  the extension manager. `fs-read` is untouched and unrelated.
- The location root is **read-gated like any other path** (`read_gate_all`
  already runs over the page's paths) and obeys protected roots (ADR 0052): the
  state directory is not readable through a plugin any more than through search.
- A location that is not `file://` mints no token; the guest receives `none` and
  answers `none` per entry.

### Performance

- **Instance pool** keyed by (plugin id, location), LRU with a TTL, replacing
  the instantiate-per-call at `server.rs:2930`. Settings are applied once.
  There are **two** call sites, not one — the daemon handler and the embedded
  backend (`crates/norte-core/src/backend.rs:1984`) instantiate the same way, so
  minting, bounds and pooling live in `norte-core::plugins` and both call it. A
  capability enforced on one path and not the other is the failure mode this
  repository has already written down three times.
- **Freshness is the guest's problem, and that is deliberate.** The host cannot
  know what a plugin's answer depends on. The guest keeps its parsed index in
  memory and re-`stat`s `.git/index` to decide whether to reparse — so the host
  stays ignorant of git, which is the constraint the whole design is under.
- **A columns-category epoch deadline**, configurable and far above 50 ms.
  On expiry: empty cells and a warning, exactly today's fail-closed behaviour.
- The listing is not blocked either way: values are fetched by a separate RPC
  after the page.

### Wire

The token never reaches the wire — the host mints it from paths it already has,
so `plugin.column_values` is unchanged. The only wire surface is the new
capability appearing in `PluginInfo` for the extension manager's badge, which is
additive: a minor bump, goldens updated, `protocol-guardian` on the diff.

## The plugin

`plugins/git-status/`, a crate outside the workspace (like `norte-gui`), built to
`wasm32-wasip2` by a `just` recipe, **installed into `config_dir/plugins/` the
way a third party's plugin would be**. Not embedded in the binary: the install
path is the thing being proven. `manifest`:

```toml
[plugin]
id = "org.norte.git-status"
category = "columns"
[[contributions.columns]]
id = "status"
header = "S"
[capabilities]
location = "read"
```

What it does, in order:

1. **Find the repository**: `stat` `.git` upward from the location. A `.git`
   file with `gitdir:` is followed once; anything else unrecognised → empty
   cells.
2. **Parse `.git/index`** (versions 2 and 3; 4 refused by name): path, stat
   data, mode, blob object id per entry. The index is sorted by path, which is
   what makes both lookups and directory aggregation a prefix scan.
3. **Compare each visible entry**: `stat` it, compare size, mtime and the rest
   against the index's stat data; when the stat comparison is ambiguous — the
   "racy git" case, same mtime as the index — `read` the file and compute its
   blob SHA-1, bounded by the per-call byte budget. Over budget: `none` for that
   entry, never a guess.
4. **Untracked and ignored**: a visible name absent from the index is untracked,
   unless a minimal `.gitignore` matcher (the directory's file, its parents' up
   to the repository root, and `.git/info/exclude`) says ignored. Without the
   matcher the column marks `target/` and is pure noise, so the matcher is in
   scope; it is text parsing, no objects.
5. **Directories aggregate**: a directory shows the strongest state found under
   its prefix.

Cell vocabulary: empty = clean, `M` modified, `D` deleted, `?` untracked,
`!` ignored.

## Tests

**Host side** — these are the ones that matter, because they are the interface:

- the token refuses `..`, an absolute `rel`, and a symlink escaping the
  directory (the last one with a real symlink, not a mocked resolver);
- a token from a previous call is dead;
- each bound (bytes per read, calls, total bytes, list entries) refuses at its
  edge;
- a plugin without `location = "read"` gets an error and never a byte;
- the state directory stays unreadable through the capability (ADR 0052);
- pool reuse across pages, and eviction by TTL;
- deadline expiry yields empty cells for the page, not an error to the client.

**Guest side**: index v2/v3 parsing against fixtures, v4 refusal, gitignore
matching, aggregation, the racy-mtime path.

**End to end** with a real built `.wasm`, the way
`decorator_wit_e2e_positional_roundtrip_wasm_real` already does it.

**Under load**: a synthetic repository with a large index, asserting the column
answers within the category deadline and that a page turn hits the guest's cache
rather than reparsing. This is the test the roadmap actually asked for — the
performance story of the interface has never been exercised.

## ADRs

1. **A plugin may be given a location it cannot name.** The opaque token, why
   the basename privacy decision is preserved rather than reversed, kernel-level
   confinement instead of a path check, the bounds, and the capability that makes
   it visible at approval.
2. Amend or reference ADR 0037 where it describes the columns contract, since
   `column-values` changes shape.

## Definition of done

`norte:location@0.1.0`, `norte:plugin@0.8.0` with every in-tree guest recompiled,
host enforcement with the tests above, instance pool and category deadline,
`org.norte.git-status` building and installing, the load test, both ADRs, a
changelog entry, and `just ci` green.
