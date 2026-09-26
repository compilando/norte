# Single command interface: humans, Claude and CI all run exactly this.

default: ci

# The gate's ONLY set of features. A single source because cargo does not
# share artifacts between different sets: a bare `cargo nextest -p norte-tui`
# and `just test` compile TWO complete universes of norte-tui and everything
# that depends on it, and neither is ever deleted. Each workspace universe
# weighs ~30 G. Every recipe that compiles the workspace uses this variable;
# see `just prune` and `just disk`.
#
# EXPLICIT features and not `--all-features`: `it-openssh` (norte-vfs-sftp) is
# a nightly test against Docker (ADR 0013) and must not even compile here.
#
# `norte-core/testing` opens the doors the e2e tests need and the published
# library must NOT have (#241): without it, `columns_git_e2e` does not
# compile, which is exactly what is wanted from a test-only door.
features := "--features norte-tui/schema --features norte-config/watch --features norte-proto/schema --features norte-core/testing"

# The gate's packages: the whole workspace.
#
# Until 2026-08-20 this excluded `norte-gui`, and not for time reasons but
# for correctness: GPUI enabled `serde_json/preserve_order` and cargo's
# features are unified PER invocation, so putting it in the same `cargo` call
# as the core changed the JSON we publish (ordered maps → insertion order)
# and turned five tests red without anyone touching code. With the GPUI GUI
# retired (ADR 0065), the exclusion is no longer needed — but the lesson
# isn't: if a new member brings a feature that changes the core's behavior,
# it gets pulled out of here again. `norte-gui-tauri` stays OUT of THIS gate,
# and not because it is provisional: it is a frontend supported since
# 2026-09-01 (ADR 0087). It stays out because building it requires the
# system's WebKitGTK, GTK3 and libsoup3, and no other part of the tree needs
# them — a gate that does not start on a machine without them stops being a
# gate. It is in `members` on purpose: this way `Cargo.lock` pins Tauri's
# versions and `cargo fmt --all` covers it.
#
# Its gate is `just gui-ci`, run by `.github/workflows/gui.yml` on every
# change that reaches it. Running it by hand was not enough: on 2026-09-01 it
# had been red on `main` for weeks without anyone knowing.
core_pkgs := "--workspace --exclude norte-gui-tauri"

# Free-disk floor (GiB) below which `just ci` refuses to start. A workspace
# `cargo build` plus the coverage-instrumented target need on the order of
# 40 G; running out of disk midway does not give a clean error: it corrupts
# artifacts and leaves linker errors (`os error 28`) that look like bugs in
# the code.
disk_floor := "40"

fmt:
    cargo fmt --all

# Recompiles the ftp-provider guest to wasm32-wasip2 and updates the artifact
# EMBEDDED in norte-core (ADR 0033). Run after touching the ftp-provider guest
# or the `provider` WIT interface.
build-ftp-wasm:
    cargo build --release --target wasm32-wasip2 \
        --manifest-path crates/norte-plugin-host/examples-wasm/ftp-provider/Cargo.toml
    cp crates/norte-plugin-host/examples-wasm/ftp-provider/target/wasm32-wasip2/release/ftp_provider.wasm \
        crates/norte-core/resources/ftp-provider.wasm

fmt-check:
    cargo fmt --all -- --check

lint: fmt-check deny-guests
    CARGO_INCREMENTAL=0 cargo clippy {{core_pkgs}} --all-targets {{features}} -- -D warnings
    cargo deny check

# Advisories for the WASM guests, which are OUTSIDE the workspace and the
# lock (see deny-guests.toml). Only `advisories`: it compiles nothing, it
# just resolves the tree.
deny-guests:
    #!/usr/bin/env bash
    set -euo pipefail
    for m in crates/norte-plugin-host/examples-wasm/*/Cargo.toml plugins/*/Cargo.toml; do
        # `--config` goes BEFORE `check`: in cargo-deny 0.20 it went back to
        # being a binary option and the subcommand rejects it ("unexpected
        # argument '--config' found"). 0.19 wanted it the other way around —
        # and the gate went red TWICE as soon as someone updated it. If it
        # changes again, it's this line.
        cargo deny --manifest-path "$m" --config deny-guests.toml check -A advisory-not-detected advisories
    done

# --no-tests=pass: phase 1's skeleton has no tests yet; with real code the
# coverage gate (85%) makes it impossible for a workspace with no tests to
# pass CI. nextest does not run doctests: those run separately (rustdoc's
# convention requires them). Features come from `{{features}}`: a single
# source for the whole gate. CARGO_INCREMENTAL=0 because incremental
# compilation adds nothing to a full run (everything gets recompiled anyway)
# and its cache weighs ~8 G per universe.
test:
    CARGO_INCREMENTAL=0 cargo nextest run {{core_pkgs}} {{features}} --no-tests=pass --no-fail-fast
    CARGO_INCREMENTAL=0 cargo test {{core_pkgs}} {{features}} --doc

# Coverage gate (same threshold as CI): only logic crates (spec §12).
#
# `clean --profraw-only` FIRST: a previous run's `.profraw` files give FALSE
# percentages (58% and 76% spurious have been seen where the real figure was
# 88%). Only that — `--workspace` would also delete the instrumented target,
# which is a separate universe (proto/vfs/core and its tree) and recompiling
# it whole is most of `just ci`'s cost. That universe is thrown away by
# `just prune`, which does run `clean --workspace`: it is paid for when disk
# is actually needed, not on every gate run.
cov:
    cargo llvm-cov clean --profraw-only
    # `--features norte-core/testing` and not `{{features}}`: `cov` selects
    # with `-p`, and with a single package selected cargo rejects a feature
    # from another one ("the package does not contain these features") — the
    # same trap the `t` recipe documents. Without this feature,
    # `tests/columns_git_e2e.rs` cannot see
    # `plugins::run_column_values_for_test` (gated on
    # `cfg(any(test, feature = "testing"))`) and `cov` did not compile: an
    # integration test is a separate crate and does not inherit the lib's
    # `cfg(test)`. Broken since that test landed, and it went unnoticed
    # because `cov` is the last step of `just ci` and `ci-fast` does not
    # include it.
    CARGO_INCREMENTAL=0 cargo llvm-cov nextest -p norte-proto -p norte-vfs -p norte-core -p norte-vfs-local -p norte-client --features norte-core/testing --fail-under-lines 85

docs:
    RUSTDOCFLAGS="-D warnings" CARGO_INCREMENTAL=0 cargo doc {{core_pkgs}} --no-deps

# Public API breakage against the latest tag (ADR 0038, #13).
#
# Packages are named ONE BY ONE, and not with `--workspace`, for two
# different reasons that push in the same direction:
#
# 1. What matters is the PUBLISHABLE libraries (MIT/Apache): those are what a
#    third party consumes. The AGPL binaries —cli and tui— and the internal
#    AGPL libraries have no public API to break.
# 2. `--workspace` ABORTS, it does not just warn, when a member did not exist
#    at the baseline: against `v0.3.0-alpha.2` it stops at `norte-help` with
#    "package not found in <rev>" and checks nothing. `norte-help` was born
#    after that tag; it enters this list with the first baseline that
#    contains it.
#
# It is NOT in `ci`: wiring it in before there is a published release would
# break every `just ci` (a gate decision from ADR 0038). It runs in
# `just release-check`.
semver baseline="v0.3.0-alpha.2":
    #!/usr/bin/env bash
    # The workspace version has to have GONE UP compared to the baseline. If
    # they are equal, cargo-semver-checks decides "no change; assume major"
    # and skips each crate's 254 checks: it comes out green, in zero time,
    # without having looked at anything. That is worse than red — a gate that
    # can check nothing and say yes is not a gate. It fails here, with the
    # reason.
    set -euo pipefail
    actual=$(grep -m1 '^version = ' Cargo.toml | cut -d'"' -f2)
    previa=$(git show {{baseline}}:Cargo.toml | grep -m1 '^version = ' | cut -d'"' -f2)
    if [ "$actual" = "$previa" ]; then
        echo "semver: the workspace version ($actual) is {{baseline}}'s." >&2
        echo "cargo-semver-checks would skip ALL checks." >&2
        echo "Bump the version in Cargo.toml before running this." >&2
        exit 1
    fi
    cargo semver-checks --baseline-rev {{baseline}} \
        -p norte-proto -p norte-vfs -p norte-testkit \
        -p norte-vfs-local -p norte-vfs-sftp -p norte-vfs-object \
        -p norte-vfs-archive -p norte-config -p norte-encoding \
        -p norte-frontend -p norte-i18n -p norte-theme

# What CI runs. `_disk` first: running out of disk mid-build does not fail
# clean, it corrupts artifacts.
ci: _disk lint test cov docs
    @just _sellar

# Fast iteration: the whole gate MINUS coverage (cov recompiles proto/vfs/core
# instrumented in its own target and re-runs their tests: ~34s fixed cost even
# with no changes). The real pre-commit gate is still `just ci`.
ci-fast: _disk lint test docs
    @just _sellar

# Records that this CONTENT passed the gate, so `pre-push` does not repeat
# it. Lives in `target/`: not versioned and does not travel to another
# machine — the seal is only valid where it ran.
#
# Without this, whoever does the right thing (run the gate, then push) pays
# for it twice, and a floor that costs twenty minutes ends up as a habitual
# `--no-verify`.
#
# The WORKING TREE is hashed and not `HEAD^{tree}`. The first version did the
# latter and was useless in the normal flow: when you run the gate, your
# changes are not committed yet, so it sealed the PREVIOUS commit's tree and
# the hook paid for it all again. The first push that used it proved it. What
# the gate validates is the files on disk, so that is what gets sealed. Costs
# 0.2s over 1,248 files.
_sellar:
    @just _huella > target/.norte-gate-ok 2>/dev/null || true

_sellar-gui:
    @just _huella > target/.norte-gui-gate-ok 2>/dev/null || true

# The fingerprint of git-tracked content, as it is on disk.
_huella:
    @git ls-files -z | xargs -0 sha256sum 2>/dev/null | sha256sum | cut -d' ' -f1

# Installs the repository's hooks (`.githooks/`). Once per clone.
#
# Today there is one: `pre-push` runs `ci-fast`, and `gui-ci` if the push
# touches the window or something that feeds into it. It exists because a
# gate that depends on someone remembering rots — `gui-ci` had been red on
# `main` for weeks (ADR 0087) — and this is the floor that depends on no
# one's service.
#
# It is a floor, not a lock: `git push --no-verify` skips it.
#
# Installs the repository's hooks. Once per clone.
hooks:
    git config core.hooksPath .githooks
    @echo "hooks installed from .githooks/ (pre-push: ci-fast [+ gui-ci])"

# ---------- disk: why it fills up and how to reclaim it ----------

# Free-space guard. Fails BEFORE compiling instead of halfway through.
_disk:
    #!/usr/bin/env bash
    set -euo pipefail
    libre=$(df -BG --output=avail . | tail -1 | tr -dc '0-9')
    if [ "$libre" -lt {{disk_floor}} ]; then
        echo "disk: ${libre} GiB free, below the {{disk_floor}} GiB floor." >&2
        echo "run 'just prune' (or 'just disk' to see where the space is)." >&2
        exit 1
    fi

# Where the space is: this tree's target, its pieces, other worktrees'
# targets and cargo's registry.
disk:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "== free =="; df -h . | tail -1
    echo "== this tree's target =="
    # `|| true` on every du: a build in progress deletes files out from under
    # du and makes it exit with an error even though the total is correct.
    [ -d target ] && du -sh target 2>/dev/null || true
    for d in target/debug/deps target/debug/incremental target/debug/build target/llvm-cov-target target/tmp; do
        [ -d "$d" ] && du -sh "$d"
    done
    echo "== OTHER worktrees' targets ('just prune' does not touch them) =="
    git worktree list --porcelain | awk '/^worktree /{print $2}' | while read -r w; do
        [ "$w" = "$PWD" ] && continue
        [ -d "$w/target" ] && du -sh "$w/target"
    done
    echo "== cargo's registry (shared; not the project's) =="
    du -sh "${CARGO_HOME:-$HOME/.cargo}" 2>/dev/null || true

# Reclaims space WITHOUT throwing away the whole build: the incremental
# cache (useless between full runs) and the coverage-instrumented target (it
# regenerates on every `just cov`). Leaves intact the artifacts that make the
# next compile fast.
#
# Why a recipe is needed: cargo NEVER garbage-collects. Every feature set,
# every dependency version and every toolchain leaves its universe of
# artifacts there forever, and every workspace universe weighs ~30 G. If this
# is not enough, `just prune-all` throws away the whole target (the next
# compile is from scratch, several minutes).
prune days="2":
    #!/usr/bin/env bash
    set -euo pipefail
    antes=$(du -sk target 2>/dev/null | cut -f1 || echo 0)
    cargo llvm-cov clean --workspace 2>/dev/null || true
    rm -rf target/debug/incremental target/release/incremental target/tmp
    # cargo-semver-checks's target (`just release-check`): 17 G measured, and
    # it regenerates on its own. Nobody was touching it.
    rm -rf target/semver-checks
    # Dead test executables. THIS is the bulk: 184 GiB of the 288 G measured
    # were 2075 exes in debug/deps, of which 1262 (122 GiB) had gone untouched
    # for more than a day. Cargo deletes NONE of them: every relink leaves the
    # previous one there forever.
    #
    # ONLY the executables are swept (a file with no extension and +x), never
    # .rlib/.rmeta. This is deliberate: if the sweep takes one that was still
    # alive, cargo just RELINKS it (seconds with lld), not recompiles it. An
    # .rlib deleted by mistake would cost a full compile.
    # The `deps` directories that EXIST: without `release/` (normal on a
    # machine that only builds in debug) `find` exits with an error, and with
    # `pipefail` that killed the WHOLE recipe right before sweeping anything.
    dirs=()
    for d in target/debug/deps target/release/deps; do
        [ -d "$d" ] && dirs+=("$d")
    done
    barridos=0
    if [ ${#dirs[@]} -gt 0 ]; then
        barridos=$(find "${dirs[@]}" -maxdepth 1 -type f -executable \
            ! -name '*.*' -mtime +{{days}} -print -delete 2>/dev/null | wc -l)
    fi
    despues=$(du -sk target 2>/dev/null | cut -f1 || echo 0)
    echo "test exes swept (>{{days}} days): $barridos"
    echo "target: $((antes / 1024 / 1024)) GiB → $((despues / 1024 / 1024)) GiB"
    df -h . | tail -1

# The hammer: throws away this tree's WHOLE target.
prune-all:
    cargo clean
    @df -h . | tail -1

# ---------- development: run and try things by hand ----------

# The TUI (release: <50ms cold start is spec §12's budget).
#
# `{{features}}` is NOT decorative here: cargo unifies features per
# invocation and keys artifacts by the resulting set. Without them, this
# `cargo run` compiled a COMPLETE, separate universe of norte-tui and
# everything hanging off it (~30 G) that no other recipe ever reused.
run:
    cargo run --release -p norte-tui {{features}}

# The TUI in debug (compiles faster; for iterating). Same features as the
# gate → reuses what `just test` already compiled, normally zero cost.
dev:
    cargo run -p norte-tui {{features}}

# The FIRST-TIME-on-a-machine recipe: puts `ntc`, `norte` and `ntc-gui` on
# the PATH pointing at this tree, and there is nothing else to do afterward.
# The links are symlinks to this tree's `target/`, so from then on any build
# (yours or the gate's) updates all three commands on its own.
#
# It is not called `setup` because `make setup` is already something else —
# the toolchain bootstrap (rustup, just, nextest) — and two `setup`s doing
# different things is exactly the kind of detail that gets mistyped at two
# in the morning.
#
# Three things this recipe does that `just link` + `just link-gui` on their
# own do not:
#
# - Checks that `~/.local/bin` is on the PATH and, if not, says how to add it
#   in fish. Linking into a directory nobody looks at is the classic silent
#   failure: the recipe says "done" and the command does not exist.
# - The window is OPTIONAL. If WebKitGTK/GTK3/libsoup3/npm is missing, the
#   graphical part warns and continues, instead of leaving the machine
#   without `ntc` — the same reason `core_pkgs` leaves the GUI out of the
#   gate.
# - `--gui`/`--no-gui` forces the decision when you do not want it guessed.
#
# `dir` picks the profile the same way as in `just link`:
# `just link-all release`.
link-all dir="debug" gui="auto":
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p ~/.local/bin
    case ":$PATH:" in
        *":$HOME/.local/bin:"*) ;;
        *)
            echo "warning: ~/.local/bin is not on the PATH. In fish:" >&2
            echo "  fish_add_path ~/.local/bin" >&2
            ;;
    esac
    just link {{dir}}
    quiero_gui={{gui}}
    if [ "$quiero_gui" = "auto" ]; then
        if command -v npm >/dev/null && pkg-config --exists webkit2gtk-4.1 2>/dev/null; then
            quiero_gui=yes
        else
            quiero_gui=no
            echo "warning: no npm or no WebKitGTK 4.1; skipping ntc-gui ('just gui-deps' and 'just link-all {{dir}} yes' once you have them)" >&2
        fi
    fi
    if [ "$quiero_gui" = "yes" ]; then
        if [ ! -d {{gui_dir}}/ui/node_modules ]; then
            just gui-deps
        fi
        if ! just link-gui {{dir}}; then
            echo "warning: the window could not be linked; ntc and norte are still there" >&2
        fi
    fi
    echo
    echo "on the PATH now:"
    for b in ntc norte ntc-gui; do
        if [ -L ~/.local/bin/$b ]; then
            printf '  %-8s → %s\n' "$b" "$(readlink ~/.local/bin/$b)"
        fi
    done

# Removes from the PATH the links `just link-all` put there. Does not touch
# what `cargo install` installed (that is what `just uninstall` is for) nor
# delete anything from the tree: it only unlinks, and only if the link points
# at THIS tree — this way a session in a worktree does not sweep away
# another one's links.
unlink:
    #!/usr/bin/env bash
    set -euo pipefail
    for b in ntc norte ntc-gui norte-gui; do
        dest=$(readlink ~/.local/bin/$b 2>/dev/null || true)
        case "$dest" in
            "$PWD"/*) rm -f ~/.local/bin/$b; echo "removed: $b" ;;
            "") ;;
            *) echo "untouched: $b (points at $dest, another tree)" ;;
        esac
    done

# Puts `ntc` on the PATH pointing at THIS tree's binary. `~/.local/bin` comes
# before cargo's bin on the PATH, so it beats `cargo install`.
#
# Why a symlink and not `just install`: `cargo install --path` compiles in
# its OWN temporary target, i.e. a whole cold build (~4-5 min and another
# universe of disk) every time you want to try a change. The symlink points
# at the binary the gate already built: zero cost and never stale as long as
# you run the tests. `just install` is still there for installing for real.
#
# `dir` (debug by default) picks the profile: `just link release` to measure
# startup, which is the one thing debug cannot tell you.
#
# The graphical window does NOT come in here: it has its own recipe
# (`just link-gui`), for the same reason `core_pkgs` excludes it from the
# gate — building it drags in WebKitGTK, GTK3, libsoup3 and npm, and putting
# it in this recipe would leave any machine without them with no `ntc`.
link dir="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "{{dir}}" = "release" ]; then
        cargo build --release -p norte-tui -p norte-cli {{features}}
    else
        cargo build -p norte-tui -p norte-cli {{features}}
    fi
    mkdir -p ~/.local/bin
    for b in ntc norte; do
        ln -sfn "$PWD/target/{{dir}}/$b" ~/.local/bin/$b
        printf '%-6s → %s\n' "$b" "$(readlink ~/.local/bin/$b)"
    done
    echo "remember: the symlink points at THIS tree; a 'just prune-all' leaves it dangling"

# Like `just link`, but `ntc`/`norte` RECOMPILE before starting.
#
# `just link`'s symlink points at the binary the last compile produced, not
# at the code that is there now: if you edit and run without having run the
# tests, you are using the old one without anything warning you. This closes
# that gap by putting a wrapper on the PATH that compiles and then runs.
#
# Three decisions inside the wrapper, and all three matter:
#
# - It compiles with the SAME `features` as the gate. Without them cargo keys
#   the artifacts by another set and builds a COMPLETE, separate universe of
#   norte-tui and everything hanging off it (~30 G) that no other recipe
#   reuses. It is the disk-budget trap, and by hand it is very easy to step
#   on.
# - If the build FAILS, it starts the previous binary with a warning instead
#   of leaving you without a file manager. A half-edited tree should not cost
#   you the tool.
# - If another session is compiling, cargo waits on `target/`'s lock. The
#   wrapper SAYS so before blocking, because a silent ten-second start looks
#   hung.
#
# `just link` is still there for when you want zero-cost startup.
link-fresh:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p ~/.local/bin
    for b in ntc norte; do
        {
            echo '#!/usr/bin/env bash'
            echo "# Generated by 'just link-fresh' in $PWD. Do not edit by hand."
            echo 'set -uo pipefail'
            echo "tree=\"$PWD\""
            echo "bin=\"\$tree/target/debug/$b\""
            echo 'if [ -e "$tree/target/.cargo-lock" ]; then'
            echo "  printf 'norte: another build holds the target/ lock, waiting…\\n' >&2"
            echo 'fi'
            echo "if ! cargo build --quiet --manifest-path \"\$tree/Cargo.toml\" -p norte-tui -p norte-cli {{features}}; then"
            echo '  if [ -x "$bin" ]; then'
            echo "    printf 'norte: the tree does not build; starting the last good build\\n' >&2"
            echo '  else'
            echo "    printf 'norte: the tree does not build and there is no previous build\\n' >&2"
            echo '    exit 1'
            echo '  fi'
            echo 'fi'
            echo 'exec "$bin" "$@"'
        } > ~/.local/bin/$b
        chmod +x ~/.local/bin/$b
        printf '%-6s → wrapper that recompiles before starting\n' "$b"
    done

# The smoke CLI (NATIVE paths): `just cli ls /tmp`, `just cli cp a b`…
# With `{{features}}` like everything the core compiles: without them it
# built its own universe of artifacts that no other recipe reused.
cli *args:
    cargo run -p norte-cli {{features}} -- {{args}}

# Tests for a specific crate: `just t norte-vfs`, `just t norte-tui`.
#
# With the gate's SAME features on purpose. A bare `cargo nextest run -p
# norte-tui` is not cheaper: it compiles a DIFFERENT artifact universe
# (another feature set = another fingerprint) for that crate and its whole
# tree, which also stays on disk forever. Iterating with this recipe reuses
# what `just ci` already compiled, and vice versa.
#
# The crate is chosen by FILTERING (`-E package(...)`), not with `-p`: `-p
# norte-vfs` alongside `--features norte-tui/schema` is a cargo error —"the
# package does not contain these features"— because with a single package
# selected there is no longer a workspace to resolve the rest against. The
# recipe was broken by this for a while. Filtering selects the same tests
# WITHOUT changing the package set, which is exactly what makes the gate's
# compile get reused.
t crate:
    CARGO_INCREMENTAL=0 cargo nextest run {{core_pkgs}} {{features}} -E 'package({{crate}})'

# Clippy with the gate's features. WITHOUT a crate argument, and not by
# oversight: clippy lacks the filter `nextest` has, and trimming the
# packages changes the feature unification —i.e. the artifact universe—
# which would lose exactly what makes this recipe cheap. Warm it costs
# whatever it costs to check what you touched; the rest comes from the
# cache.
c:
    CARGO_INCREMENTAL=0 cargo clippy {{core_pkgs}} --all-targets {{features}} -- -D warnings

# Development loop: workspace tests on every save (requires cargo-watch).
# Same features as the gate: without them `cargo watch` recompiled the whole
# workspace in its own universe on every file save.
watch:
    cargo watch -x "nextest run {{core_pkgs}} {{features}}"

# NIGHTLY integration tests against REAL servers via Docker (ADR 0013/0016):
# sftp against real OpenSSH (atmoz/sftp) and real S3. They REQUIRE Docker;
# outside the PR gate (the nightly workflow runs it, not `just ci`). (Real
# FTP: the provider is now a WASM plugin — ADR 0033 — whose contract runs
# in-process against libunftp in `just ci`; there is no dedicated nightly
# Docker job.)
it-remote:
    cargo nextest run -p norte-vfs-sftp --features it-openssh
    cargo nextest run -p norte-vfs-object --features it-s3 -E 'binary(reals3)'

# Benchmarks for spec §12's budgets (manual: they take a while).
bench:
    cargo bench -p norte-tui --bench budgets
    cargo bench -p norte-core --bench copy_remoto
    # ADR 0002 / #12's: the system's floor against the provider's path. It is
    # the yardstick that expires the decision not to bring in `tokio-uring`.
    cargo bench -p norte-vfs-local --bench local_io

# ---------- installation ----------

# Installs into $CARGO_HOME/bin —~/.cargo/bin by default— (release):
# `ntc` (the manager) and `norte` (the CLI).
# --locked: exactly the Cargo.lock versions that passed CI.
install:
    cargo install --path crates/norte-tui --locked
    cargo install --path crates/norte-cli --locked
    @echo "installed: $(command -v ntc) and $(command -v norte)"

# `cargo uninstall` takes the CRATE's name, not the binary's: the package is
# still called `norte-tui` even though it installs an `ntc`. It is not a slip
# in the line above.
uninstall:
    cargo uninstall norte-tui
    cargo uninstall norte-cli


# Builds and INSTALLS the syntect previewer: the first real plugin that can
# be kept installed, instead of only existing as a test fixture.
#
# Staged in `target/plugin-stage/` and installed from there: `plugin.wasm` is
# a build artifact and has no reason to sit next to `plugin.toml` in the
# source tree.
#
# Installing does NOT approve: the plugin ends up discovered and unconsented,
# and is approved and enabled in the extension manager (F12 in the TUI).
#
# `just plugin-syntect force` replaces one already installed (revokes its
# consent). The word, not `--force`: `just` takes any argument starting with
# `-` as another recipe, and there is no `--` to prevent that.
plugin-syntect *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=crates/norte-plugin-host/examples-wasm/previewer-syntect
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/previewer-syntect
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    cp $origen/target/wasm32-wasip2/release/previewer_syntect.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# Builds and INSTALLS the official git columns plugin (`plugins/git-status`,
# ADR 0057). Same staging as `plugin-syntect`: stage in `target/plugin-stage/`
# and `norte plugin install` from there. Installing does NOT approve. `force`
# replaces.
plugin-git-status *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/git-status
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/git-status
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/git_status.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# The official git panel (`plugins/git-panel`, phase 3): a whole slot
# painted by a plugin. Same staging as the rest: stage in
# `target/plugin-stage/` and `norte plugin install` from there. Installing
# does NOT approve, and until it is approved its panel does not exist for
# layout.
plugin-git-panel *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/git-panel
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/git-panel
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/git_panel.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# The file-type icon decorator (`plugins/file-icons`, demo D1).
plugin-file-icons *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/file-icons
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/file-icons
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/file_icons.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# The size-as-a-bar column (`plugins/size-bar`, spec 2026-09-11 V4).
plugin-size-bar *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/size-bar
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/size-bar
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/size_bar.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# The column for each entry's age (`plugins/age`, spec 2026-09-11 V4).
plugin-age *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/age
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/age
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/age.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# Image thumbnails for the window's viewer (`plugins/image-thumb`, ADR 0107).
plugin-image-thumb *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/image-thumb
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/image-thumb
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/image_thumb.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# The dimensions and duration columns (`plugins/media-info`, demo D2).
plugin-media-info *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/media-info
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/media-info
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/media_info.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# The Markdown previewer (`plugins/markdown`, demo D3).
plugin-markdown *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/markdown
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/markdown
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/markdown_preview.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# The image previewer (`plugins/image-ansi`, demo D4).
plugin-image-ansi *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/image-ansi
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/image-ansi
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/image_ansi_preview.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# The date renamer (`plugins/date-prefix`, demo C3).
plugin-date-prefix *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/date-prefix
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/date-prefix
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/date_prefix.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# The hook that counts renames (`plugins/rename-log`, demo H1 / ADR 0100).
plugin-rename-log *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    flag=""; [ "{{ARGS}}" = "force" ] && flag="--force"
    origen=plugins/rename-log
    cargo build --release --target wasm32-wasip2 --manifest-path $origen/Cargo.toml
    stage=target/plugin-stage/rename-log
    rm -rf "$stage" && mkdir -p "$stage"
    cp $origen/plugin.toml "$stage/plugin.toml"
    [ -f $origen/help.md ] && cp $origen/help.md "$stage/help.md" || true
    cp $origen/target/wasm32-wasip2/release/rename_log.wasm "$stage/plugin.wasm"
    cargo run --quiet -p norte-cli -- plugin install "$stage" $flag

# All the official plugins, at once. `just plugins force` replaces the ones
# already installed (and revokes their consent, as `plugin install` states).
[positional-arguments]
plugins *ARGS:
    #!/usr/bin/env bash
    set -euo pipefail
    just plugin-syntect "$@"
    just plugin-git-status "$@"
    just plugin-file-icons "$@"
    just plugin-media-info "$@"
    just plugin-size-bar "$@"
    just plugin-age "$@"
    just plugin-image-thumb "$@"
    just plugin-markdown "$@"
    just plugin-image-ansi "$@"
    just plugin-date-prefix "$@"
    just plugin-rename-log "$@"

# ---------- native platform lab ----------

# Fast tests for the VM orchestration's pure safety and path logic. No VM,
# network or elevated privileges required.
platform-selftest:
    shellcheck -S warning infra/vm/common/*.sh infra/vm/windows/*.sh
    ./infra/vm/common/selftest.sh

# Windows' local native builder (ADR 0157). Definitions are in the tree;
# ISOs, disks, generated answer media and artifacts live in ../norte-lab by
# default. Copy infra/vm/windows/config.example.env to config.env first.
windows-vm-preflight:
    ./infra/vm/windows/preflight.sh

windows-vm-host-network:
    ./infra/vm/windows/host-network.sh --install

windows-vm-attach-bootstrap:
    ./infra/vm/windows/attach-bootstrap.sh

windows-vm-create:
    ./infra/vm/windows/create.sh

windows-vm-start:
    ./infra/vm/windows/start.sh

windows-vm-stop:
    ./infra/vm/windows/stop.sh

windows-vm-snapshot name="provisioned":
    ./infra/vm/windows/snapshot.sh {{name}}

# Deliberately leaves the qcow2 in the external state root: undefining a VM
# is recoverable; silently deleting a 160 GiB development disk is not.
windows-vm-destroy:
    ./infra/vm/windows/destroy.sh

# ---------- distribution ----------

# Release artifacts are NOT built on this machine: its glibc is newer than
# almost any installed Linux's, and a `norte` from here required GLIBC_2.39
# and did not start on Ubuntu 22.04 or Debian 12 — with a green smoke test,
# because the smoke test also ran here. They are built, tested and verified
# with `just baseline <ref>` (ADR 0112, `baseline*` recipes below). `dist` is
# still the tool; it runs inside the image.
#
# Explicit `--target` inside `build.sh` for the reason already known: without
# it dist tries the five targets in `dist-workspace.toml` and stops at the
# first macOS crossing, and the installers would promise a file the release
# does not contain. Cross-compiling aws-lc-rs is what ADR 0021 deemed
# fragile; macOS and Windows need their own machines.

# ---------------------------------------------------------------------------
# The window: the Tauri renderer (ADR 0087).
#
# Outside the portable gate on purpose (see `core_pkgs`): building it
# requires the system's WebKitGTK, GTK3 and libsoup3. Its gate is this one,
# run by `.github/workflows/gui.yml`; by hand, `just gui-ci`.
# ---------------------------------------------------------------------------

gui_dir := "crates/norte-gui-tauri"

# The window gate's features. Exists for the same reason as `features`
# above: a bare `cargo -p norte-gui-tauri` resolves a DIFFERENT set than
# `core_pkgs`'s for the shared crates (norte-core and norte-testkit come in
# via dev-dependencies), so they got compiled and left on disk TWICE. It
# cannot take `{{features}}`: that set names `norte-tui/schema`, which is not
# in this graph.
gui_features := "--features norte-core/testing"

# The JS dependencies, from the lockfile and without touching it (`npm ci`).
gui-deps:
    cd {{gui_dir}}/ui && npm ci

# The webview bundle: typecheck + Vite. Local assets, nothing remote.
gui-build:
    cd {{gui_dir}}/ui && npm run build

# The renderer's tests (vitest, jsdom): no window, no WebKitGTK.
gui-test-ui:
    cd {{gui_dir}}/ui && npm run test

# The window's RUST tests, alone: this crate's RED→GREEN loop.
#
# Exists because `just t norte-gui-tauri` runs NOTHING —`core_pkgs` excludes
# this package from the portable gate— and the alternative was `just
# gui-ci`, which drags in npm and the Vite bundle just to see whether a Rust
# test passes. It uses the SAME `gui_features` as `gui-ci`, which is what
# makes it share its artifacts instead of compiling a separate universe (see
# that variable's comment).
# No filter, on purpose: compiling dominates the clock and this crate's whole
# suite takes seconds, so `-E 'test(...)'` would buy nothing.
gui-test:
    CARGO_INCREMENTAL=0 cargo nextest run -p norte-gui-tauri {{gui_features}} --no-tests=pass

# Format and lint the renderer.
gui-lint-ui:
    cd {{gui_dir}}/ui && npm run fmt:check && npm run lint && npm run typecheck

# The window's whole gate. `gui-build` goes BEFORE the Rust tests because one
# of them audits the packaged bundle (`el_bundle_no_llama_a_casa`).
#
# Seals SEPARATELY (`_sellar-gui`), and that is not symmetry: `just ci`
# excludes `norte-gui-tauri`, so its seal says nothing about this one. With a
# single seal, the pre-push hook took the portable gate's shortcut and
# silently skipped this gate over window changes.
gui-ci: gui-lint-ui gui-test-ui gui-build
    CARGO_INCREMENTAL=0 cargo clippy -p norte-gui-tauri --all-targets {{gui_features}} -- -D warnings
    CARGO_INCREMENTAL=0 cargo nextest run -p norte-gui-tauri {{gui_features}} --no-tests=pass
    CARGO_INCREMENTAL=0 cargo test -p norte-gui-tauri {{gui_features}} --doc
    @just _sellar-gui

# Starts the renderer against the daemon. Needs a live daemon.
gui-run *args: gui-build
    cargo run -p norte-gui-tauri --bin norte-gui -- {{args}}

# The same in release: it is the ONLY one worth measuring with (3.6).
gui-run-release *args: gui-build
    cargo run --release -p norte-gui-tauri --bin norte-gui -- {{args}}

# Puts `ntc-gui` (and its historic alias `norte-gui`) on the PATH pointing at
# THIS tree's binary, the same way `just link` does with `ntc` and `norte`.
#
# Two names for a single binary on purpose: `ntc-gui` is the one typed, and
# pairs with `ntc`; `norte-gui` is what the executable inside the crate is
# called and how the packages name it, so removing it would break the
# scripts that already use it.
#
# Depends on `gui-build` and it is not optional: `frontendDist` is `ui/dist`,
# i.e. Tauri EMBEDS the webview into the binary at compile time. Without
# rebuilding the bundle, the link would point at a binary carrying a stale
# webview inside — and that is invisible, because the executable exists and
# starts.
#
# It being embedded is also what makes the symlink work: the binary is
# self-contained and does not look for `ui/dist` in the cwd.
#
# It also assembles the `externalBin` (`binaries/norte-<triple>`,
# `ntc-<triple>`) and that is NOT a packaging concern: Tauri's build script
# requires them for ANY compile of the crate, so on a clean tree this recipe
# used to die with "resource path `binaries/norte-x86_64-…` doesn't exist"
# and only worked if someone had run `just gui-package` before.
#
# They are COPIED, not linked: `just gui-package` does a `cp` on top with
# the release binaries, and a `cp` onto a symlink writes THROUGH it — i.e. a
# link here would leave the release binary inside `target/debug/`, with
# nothing saying so.
#
# Separate from `just link` on purpose: see that recipe's comment.
link-gui dir="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    just gui-build
    perfil=()
    if [ "{{dir}}" = "release" ]; then perfil=(--release); fi
    triple=$(rustc -vV | sed -n 's/^host: //p')
    cargo build "${perfil[@]}" -p norte-cli -p norte-tui {{features}}
    mkdir -p {{gui_dir}}/binaries
    for b in norte ntc; do
        rm -f "{{gui_dir}}/binaries/$b-$triple"
        cp "target/{{dir}}/$b" "{{gui_dir}}/binaries/$b-$triple"
    done
    cargo build "${perfil[@]}" -p norte-gui-tauri --bin norte-gui {{gui_features}}
    mkdir -p ~/.local/bin
    for b in ntc-gui norte-gui; do
        ln -sfn "$PWD/target/{{dir}}/norte-gui" ~/.local/bin/$b
        printf '%-9s → %s\n' "$b" "$(readlink ~/.local/bin/$b)"
    done
    echo "remember: the symlink points at THIS tree; a 'just prune-all' leaves it dangling"

# The PRODUCTION build, unpackaged. Needs the lockfile's Tauri CLI.
#
# It runs from the CRATE's directory and not from `ui/` (#256): the CLI
# looks for `tauri.conf.json` in the current directory and its
# subdirectories, and the file lives here, not under `ui/`. Running it from
# `ui/` aborts with "Couldn't recognize the current folder as a Tauri
# project" — which is what this recipe did since it was written, and why it
# never produced anything.
#
# The lockfile's binary is invoked by its path instead of with `npx`: `npx`
# resolves against the directory it is called from, and there is no
# `node_modules` from the crate.
gui-build-release: gui-build
    cd {{gui_dir}} && ./ui/node_modules/.bin/tauri build --no-bundle

# The real package: `.deb` and AppImage in `target/release/bundle/`.
#
# **`NO_STRIP=1` is not optional on a modern system** (#256). `linuxdeploy`'s
# AppImage carries its own `strip`, from an old binutils that does not
# recognize the `.relr.dyn` section an up-to-date distribution's libraries
# use. Without the variable, it fails with `failed to run linuxdeploy` after
# a wall of "Unable to recognise the format of the input file" — which never
# says anywhere that the problem is the strip.
#
# The correct fix in the medium term is building on the OLDEST
# glibc/WebKitGTK baseline, which is what the plan's task 7.1 asks for
# anyway; this is what makes the package come out today, on the reference
# machine. And the package carries all THREE binaries (#256): `norte-gui`,
# the `norte` daemon and the `ntc` TUI. A package with only the window does
# not start on a clean install — since #300 the window starts its own
# daemon, and for that it has to be there. They go in as `externalBin`,
# which is how Tauri bundles a sidecar executable —next to it: in the
# `.deb` they end up in `/usr/bin`, which is where the window looks for them
# (next to its own executable, and otherwise on the `PATH`).
#
# Tauri requires the source file to carry the target's TRIPLE in its name and
# strips it when packaging, so they are copied with that suffix to
# `binaries/`.
gui-package: gui-build
    #!/usr/bin/env bash
    set -euo pipefail
    triple=$(rustc -vV | sed -n 's/^host: //p')
    # WITHOUT `{{features}}`, and one package per invocation: what gets
    # packaged is the product `dist` publishes (`precise-builds`), not the
    # gate's universe. The gate's four features are for tests or docs
    # —`schema` is "outside the final binary" (ADR 0007) and
    # `norte-core/testing` would put an unpoliced minting path into the
    # package—; `watch` is already required by `norte-tui`.
    cargo build --release -p norte-cli
    cargo build --release -p norte-tui
    mkdir -p {{gui_dir}}/binaries
    for b in norte ntc; do
        rm -f "{{gui_dir}}/binaries/$b-$triple"
        cp "target/release/$b" "{{gui_dir}}/binaries/$b-$triple"
    done
    cd {{gui_dir}} && NO_STRIP=1 ./ui/node_modules/.bin/tauri build

# Installs the PACKAGE in a clean container and checks that it works in
# there: the three binaries, the initial listing, and the window starting
# under Xvfb without dying.
#
# The window's sibling to `dist-smoke`. That one unpacks the portable
# tarballs; this one does what neither did: install for real on a
# distribution that has not seen this tree. `empaquetado.rs` checks what the
# package PROMISES (reads `tauri.conf.json`); this, what it DOES.
#
# Needs Docker and a prior `just gui-package`. It does not need CI — that is
# the point: a clean-install failure should not depend on who presses the
# button.
#
# Installs the package in a clean container and starts it.
gui-smoke imagen="debian:trixie":
    ./scripts/gui-smoke.sh {{imagen}}

# The baseline system's tests (`scripts/baseline/lib.sh`) and shellcheck over
# all its scripts. Seconds, no Docker: that folder's RED→GREEN loop.
baseline-selftest:
    shellcheck -S warning scripts/baseline/*.sh scripts/gui-smoke.sh
    ./scripts/baseline/selftest.sh

# The baseline's build image (Ubuntu 22.04 pinned, Node 22.23.2, the
# `rust-toolchain.toml` toolchain, `dist-workspace.toml`'s cargo-dist). Built
# once; its tag only changes if one of those entries changes.
baseline-image:
    ./scripts/baseline/image.sh

# Builds everything publishable from a ref in the baseline image and leaves
# `target/baseline/<revision>/` with dist/, gui/, MANIFEST and SHA256SUMS.
# Fails if a binary requires glibc above the floor (2.35) or reports another
# revision. Slow cold; the `norte-baseline-*` volumes make it cheaper.
baseline-build ref="HEAD":
    ./scripts/baseline/build.sh {{ref}}

# Smoke test of a baseline build over the `scripts/baseline/matrix.txt`
# matrix. `artefacto` limits it to one (tarball, installer, deb, rpm,
# appimage).
baseline-smoke dir artefacto="":
    ./scripts/baseline/smoke.sh {{dir}} {{artefacto}}

# A ref end to end: build on the baseline, smoke test over the matrix and
# verification. Green = this build could be published.
baseline ref="HEAD":
    ./scripts/baseline/all.sh {{ref}}

# Can this build be published? Checksums, floor, revisions and the full
# matrix.
baseline-verify dir:
    ./scripts/baseline/verify.sh {{dir}}

# Uploads a VERIFIED build of that same tag to the tag's release. It only
# publishes: the release has to exist. Rejects a build from another commit
# (its revision has to be `<tag>-0-g…`). The schemas go with the binaries on
# purpose (#13): a third party wanting to write a client should not have to
# clone the repository to learn the protocol's shape.
baseline-publish tag dir:
    #!/usr/bin/env bash
    set -euo pipefail
    ./scripts/baseline/verify.sh {{dir}}
    rev="$(awk '$1 == "revision" { print $2 }' {{dir}}/MANIFEST)"
    case "$rev" in
        {{tag}}-0-g*) ;;
        *) echo "the build is from $rev, not from {{tag}}" >&2; exit 1 ;;
    esac
    gh release upload {{tag}} \
        $(find {{dir}}/dist {{dir}}/gui -maxdepth 1 -type f) \
        {{dir}}/SHA256SUMS \
        docs/schema/proto.schema.json docs/schema/norte.schema.json docs/schema/keymap.schema.json \
        --clobber

# What the baseline occupies outside `target/`: Docker volumes and images.
# `-cargo`, `-rustup` and `-node` are the retired `gui-baseline`'s volumes.
baseline-prune:
    docker volume rm -f norte-baseline-registry norte-baseline-target norte-baseline-cargo norte-baseline-rustup norte-baseline-node
    docker image ls -q norte-builder | xargs -r docker image rm -f
    rm -rf target/baseline
