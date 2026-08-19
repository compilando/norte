# 0061 - A configuration name that becomes a filename is bytes, and resolves byte-exactly

- Status: accepted
- Date: 2026-08-19
- Decision makers: Oscar González
- Related: ADR 0051 (the shared fold key), ADR 0058 (a screen is a tree),
  hard rule 1 (filenames are bytes), issues #245, #246, #242.

## Context and problem statement

A layout is chosen by NAME — on the command line (`--layout mio`), in the
config (`[ui] layout = "mio"`), and by pressing Enter on a row of the picker.
That name is then turned into a path: `<config>/layouts/<name>.toml`. So the
name is not a label the program shows; it is the argument to an `open`.

It was handled as text anyway, and both halves of that produced a bug that no
green test could see, because both are invisible on Linux:

- `--layout` went through `Cli::text`, which is `to_string_lossy`. `$'\xff'`
  opened `layouts/\u{FFFD}.toml`: the file the user meant was unreachable, and
  two different invalid bytes landed on the same file. Its own rustdoc already
  said the method was "only for values that are text by contract".
- The composed path was handed to the OS to resolve. On APFS or NTFS, a saved
  `Orthodox.toml` is what `load("orthodox")` opens. The picker row that said
  *factory* then applied the user's tree, after previewing the preset's.

The guard on the name was `Path::components().count() == 1`. On Windows
`Path::new("C:")` is exactly one component — a `Prefix` — and `Path::join`
with a prefix replaces the whole base.

The same question arrives with every future name-shaped setting: a theme file,
a keymap file, a plugin id that becomes a directory.

## Options

### A. Keep `String`, and fold before comparing

Compare the picker's factory names against the user's with the shared fold key
(ADR 0051), so `Orthodox` shadows `orthodox`, and normalise NFC/NFD.

- **Good**: no signature changes; it matches how the pairing engine already
  thinks about two names being one name.
- **Bad**: it answers the wrong question. The fold key says whether two names
  COLLIDE on some filesystem, and the fold mode that would be correct depends
  on the filesystem the config directory is on — which nobody probes (#145).
  On ext4 `Orthodox` and `orthodox` are two real files, and folding them into
  one row hides one of the user's files.
- **Bad**: it does nothing for `--layout $'\xff'`, which is the other half.

### B. Bytes end to end, and resolve against the directory listing

The name is an `OsString` from the command line to the filesystem. `load` lists
`layouts/` and requires an entry whose filename equals `<name>.toml` byte for
byte; if there is none, the answer is `NotFound` — the same answer on every
filesystem. The guard inspects the NAME rather than its shape as a path.

- **Good**: identical behaviour on ext4, APFS and NTFS, without probing
  anything. The factory row can no longer load the user's file, because the
  file it names does not exist under those bytes.
- **Good**: a non-UTF-8 layout file stops being unreachable, and stops being
  silently dropped from the picker.
- **Bad**: one `read_dir` per load. The directory holds a handful of files and
  the read already happens to list the picker; it is not a cost that shows.
- **Bad**: on a case-insensitive volume, `--layout Orthodox` for a file saved
  as `orthodox.toml` now says "not found" where the OS would have opened it.
  That is the price of the same answer everywhere, and the picker shows the
  real name, so the discoverable path stays correct.

## Decision

**B. A configuration value that becomes a filename travels as bytes, and the
file is resolved byte-exactly against the directory listing.**

Three rules follow, and they are the reusable part:

1. **The type says what it is.** `OsString`/`OsStr`, never `String`, from the
   argument parser (`Cli::os_text`) to `config::load(dir, name: &OsStr)`. Only
   the PAINTED form goes through text, via `display_os_name`, which marks the
   lossy conversion and masks terminal hazards like any other name.
2. **The filesystem does not get to choose.** The candidate is looked up in the
   listing and compared byte for byte. Case-insensitivity, NFD normalisation
   and 8.3 aliases are then somebody else's business, not a silent redirect.
3. **The guard is about the name, not about the path shape.** Rejected: empty,
   `.`, `..`, anything containing `/`, `\`, `:` or NUL, anything ending in a
   dot or a space, and the Win32 device names (`CON`, `NUL`, `COM1`…) — on
   every platform, because the file syncs even when the rule does not.

The picker keeps a factory row and a same-name-different-case user row as TWO
rows. They are two files, and on a case-insensitive volume the user's is the
one that exists; what mattered was that the row labelled *factory* stops
loading it.

## Consequences

### Positive

- `--layout`, `[ui] layout` and the picker agree about which file they open,
  on all three platforms, and a hostile name reaches the loader intact.
- `[ui] layout` is honoured from every config layer including Project, so a
  cloned repository could name `CON` — a device Win32 opens — from inside a TUI
  holding the terminal in raw mode. The name guard closes that without taking
  the key away from the layer.
- The corpus grew the plain-ASCII case pair (`Orthodox`/`orthodox`) it did not
  have: its eight case twins were all non-ASCII exotica, so a path that only
  breaks under ordinary case folding had no fixture at all.

### Negative

- Two APIs changed shape (`config::load`, `config::list`, `LayoutPicker::open`,
  `App::apply_loaded_layout`), and every caller with them.
- A user on APFS who types the wrong case now gets a diagnostic where the OS
  used to be forgiving. Deliberate: the alternative is a name that means one
  file here and another one there.
- A name that is NOT valid UTF-8 cannot match a factory preset, because those
  are named by ASCII strings. It falls through to `NotFound`, which is correct
  and worth saying out loud.

### Neutral

- The `U+FFFD` name — a legal filename that IS the replacement character — was
  deliberately NOT added to the shared corpus. The SFTP provider rejects a
  listing containing one, because `russh-sftp` decodes names lossily and it
  cannot tell a real one from damage (#37); adding the fixture would make five
  provider and bridge tests assert a guarantee that issue has not delivered.
