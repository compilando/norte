# 0087 - The window is a supported frontend, and has a gate that runs

- Status: accepted
- Date: 2026-09-01
- Decision makers: Oscar González
- Related: ADR 0065 (retiring the GPUI window), ADR 0066 (`norte-ui-host` and
  the versioned bridge), ADR 0067 (the webview never gets a raw path), #256
  (packaging), #261 (accessibility and desktop integration).

## Context and problem statement

`norte-gui-tauri` was built as a spike: an experiment with a go/no-go at the
end. It has since acquired everything a frontend is supposed to have — the
semantic host behind it (`norte-ui-host`), a versioned bridge, a plain
TypeScript renderer with 125 tests, a restrictive CSP, a boundary test that
forbids it from reaching past `norte-client`, and packaging that ships it with
the `norte` and `ntc` binaries so a clean install has a daemon (#256).

And it was still labelled an experiment everywhere it was labelled at all: the
application identifier was `dev.norte.gui.spike`, `README.md` said in bold that
**there is no graphical interface right now**, the justfile and `Cargo.toml`
explained the exclusion by "until the spike closes its go/no-go", and CI never
ran its gate.

That last one is the part that stopped being a documentation problem.

## What running the gate found

`just gui-ci` existed and was run by hand. On 2026-09-01 it was **red on
`main`**, in two independent ways, and had been for weeks:

1. `startup.rs::boot` had grown to 104 lines and tripped `too_many_lines`,
   which the workspace denies.
2. `tests/celdas_locales.rs` did not compile: `UiHostOptions` gained a
   `profile` field (#307) and this test, in another crate, was never updated.

Neither is serious on its own. Together they are the argument: **a gate that
depends on someone remembering is not a gate.** The portable gate never sees
this crate, `just ci-fast` does not run `gui-ci`, and so the only thing
standing between `norte-ui-host` and a broken window was habit.

## Decision

### Go. The window is a supported frontend

The evidence is that it works, it is tested at the boundary that matters, and
it is the frontend the project decided to build when GPUI was retired (ADR
0065). Nothing in the go/no-go was still open except the labelling and the CI.

Supported does not mean finished, and the ADR says so rather than leaving it
implied: it is not exercised against screen readers, IME input or fractional
scaling (#261), and the packages are built against a current
glibc/WebKitGTK, so an older distribution needs a build from source. Those are
release-readiness questions on diverse desktops. They are not "is this a real
frontend".

### Its gate runs on every push — from a hook, not from a service

The gate is `just gui-ci`, and what runs it is `.githooks/pre-push`
(`just hooks`), on any push whose diff touches `norte-gui-tauri` or anything
that goes into it: `norte-ui-host`, `norte-client`, `norte-frontend`,
`norte-proto`, plus the shared configuration. Those four upstream crates are on
the list because of the failure mode that prompted this ADR: a change to
`norte-ui-host` can desynchronise the bridge or break a downstream test while
the portable gate stays green.

`.github/workflows/gui.yml` exists and does the same thing with WebKitGTK, GTK3
and libsoup3 installed **only in that job** — but **GitHub Actions is disabled
on this repository**, and has been since 2026-07-13. So it runs nothing today.

The first version of this ADR said "its own CI gate runs on every change". That
was false when it was written, and not because of the workflow: nothing at all
had run for seven weeks. The claim is recorded here rather than quietly fixed
because it is the same failure the ADR is about — a gate believed in rather
than observed — committed while writing the ADR against it.

The workflow file stays. It is correct, it costs nothing while Actions is off,
and re-enabling Actions is one setting. What must not stay is the belief that
it is running.

**So the floor is local, and it has to be one people keep.** A hook that makes
a push take twenty minutes gets `--no-verify`d into irrelevance, which is how
this rots the next time; see the stamp in `.githooks/pre-push`.

### The crate stays out of the portable gate, for the opposite reason

`core_pkgs` still excludes it, and the comment saying why is rewritten: not
"until the spike closes", but because requiring WebKitGTK to test `norte-vfs`
would make the portable gate unrunnable on a machine that has no business
having a browser engine installed. Staying out of that gate is now a
*property*, not a *stage*.

### The identifier drops `.spike`

`dev.norte.gui.spike` → `dev.norte.gui`. It names the webview's data
directory, the desktop entry and the package, so anyone inspecting an
installation read that this was an experiment.

Changing it has a price and that is why it was done now: an installation
carrying the old id is not upgraded in place, it sits beside the new one. In
alpha the price is zero. After a first stable release it would have been a
migration. A test now asserts the id and that it does not say `spike`.

### "The window finds its daemon" becomes testable

The packaging tests already asserted that the bundle carries `norte` and `ntc`
as sidecars. What they could not assert is the other half of the promise: that
`norte-gui` *resolves* the sibling binary at startup. That logic lived inside
`comando_de_daemon`, wrapped around `current_exe()`, and could only be checked
by installing — which is when it is too late.

It is now a pure function taking the directory, with three tests: the sibling
wins and travels by full path; with no sibling it falls back to `PATH`; and a
*directory* named `norte` is not a daemon — without that check the window
would launch a directory and report "could not connect", which says nothing
about what went wrong.

## Consequences

- Every push touching the window or its contracts runs `just gui-ci`, from the
  hook. The two rots above could not have reached `main` and stayed — but only
  as long as the hook is installed (`just hooks`, once per clone) and not
  skipped, which is a weaker guarantee than a service and is stated as such.
- No graphics dependency entered the portable gate; the CSP is untouched; the
  webview still receives no raw path (ADR 0067); the window still speaks only
  through `norte-client` and its boundary test still holds.
- `README.md` and `ARCHITECTURE.md` no longer contradict the repository about
  whether a graphical interface exists.
- Remaining, and named rather than implied: #261 (accessibility, IME,
  fractional scaling, compositor restart) needs a person in front of a screen;
  the smoke test on a clean machine and the old-glibc/WebKitGTK baseline are
  still open from phase 7.1; and `celdas_locales.rs` still polls with
  `tokio::time::sleep`, which ADR 0085 removed from `norte-ui-host` and has not
  yet been applied here.
