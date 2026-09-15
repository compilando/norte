# Baseline Release System Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One local, Docker-based system that builds EVERY release artefact of a
given git ref on a pinned Ubuntu 22.04 builder, refuses a binary above the glibc
floor, smoke-tests each artefact on each distribution of a pinned matrix, and
leaves a manifest that a later publish step can verify.

**Architecture:** Pure shell functions in `scripts/baseline/lib.sh` (tested by
`selftest.sh`, no Docker), a builder image keyed by the hash of its inputs, a
build script that clones the ref inside that image and runs `dist` plus the
Tauri bundler, a smoke runner that fans a matrix file out to containers in
parallel, and a verifier that reads only the output directory. Everything lives
under `scripts/baseline/` and is driven from `just`.

**Tech Stack:** bash, Docker, cargo-dist 0.32.0 (from `dist-workspace.toml`),
Tauri CLI (from `crates/norte-gui-tauri/ui/package-lock.json`), Node 22.23.2,
binutils `objdump`, libarchive `bsdtar` on the host.

**Spec:** the scope agreed with Oscar on 2026-09-15, restated in *Context*
below; ADR 0111 is the decision this plan extends and partly supersedes.

## Context — what is broken today

1. `just dist` builds `norte`/`ntc` tarballs on the reference machine (Arch,
   glibc 2.44). Measured: its `norte` needs **GLIBC_2.39**, so the tarballs and
   `installer.sh` do not start on Ubuntu 22.04 (2.35) or Debian 12 (2.36).
2. `just dist-smoke` runs those tarballs on the machine that built them, so it
   cannot see (1).
3. Builds take HEAD, not a ref: after tagging `v0.3.0-alpha.4` on `3d69a6fc`
   and merging, binaries said `v0.3.0-alpha.4-1-gaecbc039`.
4. `scripts/gui-baseline.sh` reinstalls apt packages, rustup and "latest Node
   22" on every run: slow, and two runs are not the same build.
5. The glibc floor is written to `glibc.txt` and enforced by nobody.
6. The AppImage is never smoke-tested.

## Global Constraints

- glibc floor: **2.35** (Ubuntu 22.04). A shipped binary needing more fails the build.
- Builder base: `ubuntu@sha256:829f6df217bcbae2b371026e81711d1a787c61b2967ad09d015063663ebafbf7` (ubuntu:22.04).
- Node: **22.23.2**, checked against nodejs.org `SHASUMS256.txt`.
- Rust toolchain: whatever `rust-toolchain.toml` says (today 1.96.1). cargo-dist: whatever `dist-workspace.toml` `cargo-dist-version` says (today 0.32.0). Never duplicate those numbers in scripts.
- Smoke matrix (image digests pinned):
  - `ubuntu:22.04@sha256:829f6df217bcbae2b371026e81711d1a787c61b2967ad09d015063663ebafbf7` (glibc 2.35)
  - `ubuntu:24.04@sha256:224a1869083a311ef3f13648a154ba79832fbef6364d31493642ca03082da254` (glibc 2.39)
  - `debian:bookworm@sha256:6ebd97fa83deb272194a2cf015b3d26a4d538e9ad3a7a79d544c8af5b0a01443` (glibc 2.36)
  - `debian:trixie@sha256:f324c7ff54321e8d9c588493a20244965938ce0aa50bbd1022d38010e9ffc4b1`
  - `fedora:41@sha256:f1a3fab47bcb3c3ddf3135d5ee7ba8b7b25f2e809a47440936212a3a50957f3d` (glibc 2.40)
- NOTHING is published or pushed by this plan. `baseline-publish` is written and never run.
- All scripts pass `shellcheck -S warning`. Comments in Spanish, like the rest of `scripts/`.
- Agents: no `sleep`/`tail -f` to wait; the session's Bash hook blocks redirections to `$VARIABLE` paths and `<` in commands — use literal paths. Long builds (> 5 min) run under `setsid nohup … & disown` with a log in the scratchpad and a `Monitor` that exits when the pid dies.
- No Rust code changes: the cargo gate is not part of this plan's loop; `just baseline-selftest` is.

## File Structure

| file | responsibility |
| --- | --- |
| `scripts/baseline/lib.sh` | pure functions: glibc parsing, version order, image slug, manifest sums/problems, smoke bookkeeping, builder tag |
| `scripts/baseline/selftest.sh` | tests for `lib.sh`, fixtures in a temp dir, no Docker |
| `scripts/baseline/Dockerfile` | the pinned builder image |
| `scripts/baseline/image.sh` | builds the image if its tag is missing; prints the tag |
| `scripts/baseline/build.sh` | `build.sh [ref]`: clone ref in the builder, `dist` + Tauri bundle, MANIFEST, SHA256SUMS; prints the output dir |
| `scripts/baseline/matrix.txt` | `artefact image@digest` lines |
| `scripts/baseline/smoke-inside.sh` | the script that runs INSIDE a smoke container, per artefact |
| `scripts/baseline/smoke.sh` | fans `matrix.txt` out (`xargs -P`), records `SMOKE`, logs per run |
| `scripts/baseline/verify.sh` | sums, floor, versions, full matrix: exit 0 only if all hold |
| `scripts/baseline/all.sh` | build → smoke → verify for one ref |
| `scripts/gui-smoke.sh` | becomes a thin wrapper: host `gui-package` output through `smoke.sh --one` |
| `scripts/gui-baseline.sh`, `scripts/dist-smoke.sh` | deleted |
| `justfile` | `baseline*` recipes; `dist`, `dist-smoke`, `dist-publish`, `gui-baseline`, `gui-publish` removed |
| `docs/adr/0112-…`, `docs/adr/0111-…`, `docs/adr/README.md`, `CLAUDE.md`, `.claude/commands/release-check.md` | docs |

Output layout, one directory per build: `target/baseline/<revision>/`
with `dist/` (tarballs, installers, `sha256.sum`, `source.tar.gz`), `gui/`
(deb, rpm, AppImage), `MANIFEST`, `SHA256SUMS`, `SMOKE`, `smoke/*.log`.

`MANIFEST` is line-oriented, first word is the kind:

```
ref v0.3.0-alpha.4
commit 3d69a6fc…
revision v0.3.0-alpha.4-0-g3d69a6fc
glibc-floor 2.35
builder norte-builder:0123456789ab
glibc tar/norte-cli-x86_64-unknown-linux-gnu/norte 2.34
version tar/norte-cli-x86_64-unknown-linux-gnu/norte norte 0.3.0-alpha.4 (v0.3.0-alpha.4-0-g3d69a6fc)
```

`SMOKE` has one `ok <artefact> <image>` line per passed run.

---

### Task 1: Pure functions and their self-test

**Files:**
- Create: `scripts/baseline/lib.sh`
- Create: `scripts/baseline/selftest.sh`
- Modify: `justfile` (add `baseline-selftest` after the `gui-smoke` recipe)

**Interfaces:**
- Produces (sourced by every later script):
  - `glibc_max` — stdin: `objdump -T` text; stdout: highest `GLIBC_x.y[.z]` without prefix, or nothing; always exit 0
  - `version_le A B` — exit 0 iff A ≤ B in version order
  - `version_matches OUTPUT REVISION` — exit 0 iff OUTPUT ends with `(REVISION)`
  - `image_slug IMAGE` — `debian:trixie@sha256:…` → `debian-trixie`
  - `manifest_sums DIR` — writes `DIR/SHA256SUMS` over every file except `SHA256SUMS`, `MANIFEST`, `SMOKE`, `smoke/*`
  - `manifest_check_sums DIR` — exit 0 iff every sum matches
  - `manifest_problems MANIFEST_FILE` — prints one line per problem, nothing if none
  - `smoke_missing MATRIX_FILE SMOKE_FILE` — prints `artefact image` for each matrix line without an `ok` record
  - `builder_tag ROOT` — `norte-builder:<12 hex>` from Dockerfile + rust-toolchain.toml + dist-workspace.toml

- [ ] **Step 1: Write the failing self-test**

`scripts/baseline/selftest.sh`:

```bash
#!/usr/bin/env bash
# Pruebas de `lib.sh`: sin Docker ni red, en milisegundos. Lo que se puede
# equivocar en silencio —el orden de versiones, un suelo que deja pasar un
# binario, una suma que no se comprueba— se prueba aquí y no en una build de
# veinte minutos.
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

fallos=0
igual() {
  if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FALLA $1: se esperaba '$3', fue '$2'"; fallos=$((fallos + 1)); fi
}
cierto() { if "${@:2}"; then igual "$1" si si; else igual "$1" no si; fi; }
falso() { if "${@:2}"; then igual "$1" si no; else igual "$1" no no; fi; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# glibc_max
igual "glibc_max toma la más alta" \
  "$(printf '0 GLIBC_2.17 x\n0 GLIBC_2.34 y\n0 GLIBC_2.3.4 z\n' | glibc_max)" "2.34"
igual "glibc_max sin símbolos no dice nada" "$(printf 'nada\n' | glibc_max)" ""

# version_le
cierto "2.34 <= 2.35" version_le 2.34 2.35
cierto "2.35 <= 2.35" version_le 2.35 2.35
cierto "2.3.4 <= 2.35" version_le 2.3.4 2.35
falso "2.39 <= 2.35" version_le 2.39 2.35

# version_matches
cierto "versión con la revisión" version_matches "norte 0.3.0-alpha.4 (v0.3.0-alpha.4-0-gabc)" "v0.3.0-alpha.4-0-gabc"
falso "versión de otra revisión" version_matches "norte 0.3.0-alpha.4 (v0.3.0-alpha.4-1-gdef)" "v0.3.0-alpha.4-0-gabc"
falso "revisión unknown" version_matches "norte 0.3.0-alpha.4 (unknown)" "v0.3.0-alpha.4-0-gabc"

# image_slug
igual "slug con tag" "$(image_slug ubuntu:22.04)" "ubuntu-22.04"
igual "slug con tag y digest" "$(image_slug debian:trixie@sha256:f324)" "debian-trixie"
igual "slug solo digest" "$(image_slug debian@sha256:f324)" "debian"

# manifest_sums / manifest_check_sums
mkdir -p "$tmp/out/dist" "$tmp/out/smoke"
echo a >"$tmp/out/dist/a.tar.xz"
echo b >"$tmp/out/b.deb"
echo m >"$tmp/out/MANIFEST"
echo s >"$tmp/out/smoke/x.log"
manifest_sums "$tmp/out"
igual "SHA256SUMS lista dos ficheros" "$(wc -l <"$tmp/out/SHA256SUMS" | tr -d ' ')" "2"
igual "SHA256SUMS en formato dist" "$(grep -c ' \*dist/a.tar.xz$' "$tmp/out/SHA256SUMS")" "1"
cierto "sumas intactas cuadran" manifest_check_sums "$tmp/out"
echo cambiado >"$tmp/out/b.deb"
falso "una suma tocada no cuadra" manifest_check_sums "$tmp/out"

# manifest_problems
cat >"$tmp/bueno" <<'EOF'
revision v1-0-gabc
glibc-floor 2.35
glibc deb/usr/bin/norte-gui 2.34
glibc deb/usr/bin/norte 2.34
glibc tar/norte-tui/ntc none
version deb/usr/bin/norte norte 1 (v1-0-gabc)
EOF
igual "manifest bueno sin problemas" "$(manifest_problems "$tmp/bueno")" ""
cat >"$tmp/malo" <<'EOF'
revision v1-0-gabc
glibc-floor 2.35
glibc tar/x/norte 2.39
version tar/x/norte norte 1 (v1-1-gdef)
EOF
# suelo superado, revisión ajena, y faltan ntc y norte-gui: cuatro.
igual "manifest malo: cuatro problemas" "$(manifest_problems "$tmp/malo" | wc -l | tr -d ' ')" "4"
igual "sin suelo es un problema" "$(printf 'revision v1\n' >"$tmp/sinsuelo"; manifest_problems "$tmp/sinsuelo" | grep -c 'glibc-floor')" "1"

# smoke_missing
printf '# comentario\n\ntarball ubuntu:22.04@sha256:1\ndeb debian:bookworm@sha256:2\n' >"$tmp/matrix"
printf 'ok tarball ubuntu:22.04@sha256:1\n' >"$tmp/smoke"
igual "falta un humo" "$(smoke_missing "$tmp/matrix" "$tmp/smoke")" "deb debian:bookworm@sha256:2"
igual "sin fichero de humos faltan todos" "$(smoke_missing "$tmp/matrix" "$tmp/no-existe" | wc -l | tr -d ' ')" "2"

# builder_tag
mkdir -p "$tmp/r/scripts/baseline"
echo 'FROM x' >"$tmp/r/scripts/baseline/Dockerfile"
echo 'channel = "1"' >"$tmp/r/rust-toolchain.toml"
echo 'cargo-dist-version = "0"' >"$tmp/r/dist-workspace.toml"
t1="$(builder_tag "$tmp/r")"
echo 'FROM y' >"$tmp/r/scripts/baseline/Dockerfile"
t2="$(builder_tag "$tmp/r")"
cierto "tag con forma norte-builder:hex12" bash -c "[[ '$t1' =~ ^norte-builder:[0-9a-f]{12}$ ]]"
falso "cambiar el Dockerfile cambia el tag" test "$t1" = "$t2"

if [ "$fallos" -gt 0 ]; then echo "$fallos fallos"; exit 1; fi
echo "selftest: todo en orden"
```

`manifest_problems` also requires one `glibc` line for each of `norte`, `ntc`
and `norte-gui`, which is why the bad manifest yields four problems.

Make it executable: `chmod +x scripts/baseline/selftest.sh`.

- [ ] **Step 2: Run it to verify it fails**

Run: `scripts/baseline/selftest.sh`
Expected: exits non-zero — `lib.sh: No such file or directory`.

- [ ] **Step 3: Write `lib.sh`**

`scripts/baseline/lib.sh`:

```bash
# shellcheck shell=bash
# Funciones puras del sistema de base: sin Docker, sin red, sin estado. Las
# prueba `selftest.sh`; las usan `build.sh`, `smoke.sh` y `verify.sh`.

# La versión GLIBC_ más alta de una salida de `objdump -T` (stdin), sin
# prefijo. Nada si no hay ninguna. Siempre sale 0: un binario sin símbolos de
# glibc no es un error de esta función.
glibc_max() {
  { grep -oE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' || true; } |
    sed 's/^GLIBC_//' | sort -Vu | tail -n 1
}

# 0 si $1 <= $2 en orden de versiones (2.3.4 < 2.35).
version_le() {
  [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n 1)" = "$1" ]
}

# 0 si la salida de `--version` ($1) termina en «($2)».
version_matches() {
  [[ "$1" == *"($2)" ]]
}

# `debian:trixie@sha256:…` → `debian-trixie`: un nombre de fichero por imagen.
image_slug() {
  local s="${1%%@*}"
  printf '%s\n' "${s//[:\/]/-}"
}

# SHA256SUMS de todo fichero bajo $1 salvo los que se escriben DESPUÉS (las
# sumas mismas, el MANIFEST, los humos). Formato de `dist`: `<hash> *<ruta>`.
manifest_sums() {
  local dir="$1"
  (
    cd "$dir" || exit 1
    find . -type f ! -name SHA256SUMS ! -name MANIFEST ! -name SMOKE ! -path './smoke/*' -printf '%P\n' |
      LC_ALL=C sort |
      while IFS= read -r f; do sha256sum --binary -- "$f"; done
  ) >"$dir/SHA256SUMS"
}

manifest_check_sums() {
  (cd "$1" && sha256sum --quiet --strict -c SHA256SUMS)
}

# Un problema por línea; nada si el MANIFEST es aceptable.
manifest_problems() {
  local file="$1" floor revision kind path rest b
  floor="$(awk '$1 == "glibc-floor" { print $2 }' "$file")"
  revision="$(awk '$1 == "revision" { print $2 }' "$file")"
  [ -n "$floor" ] || echo "MANIFEST sin glibc-floor"
  [ -n "$revision" ] || echo "MANIFEST sin revision"
  for b in norte ntc norte-gui; do
    grep -qE "^glibc [^ ]*/$b " "$file" || echo "MANIFEST sin glibc de $b"
  done
  while read -r kind path rest; do
    case "$kind" in
      glibc)
        if [ "$rest" != none ] && [ -n "$floor" ] && ! version_le "$rest" "$floor"; then
          echo "$path pide glibc $rest, por encima del suelo $floor"
        fi
        ;;
      version)
        version_matches "$rest" "$revision" || echo "$path dice «$rest», se esperaba ($revision)"
        ;;
    esac
  done <"$file"
}

# Las líneas `artefacto imagen` de la matriz sin su `ok artefacto imagen`.
smoke_missing() {
  local matrix="$1" record="$2" artifact image
  while read -r artifact image; do
    case "${artifact:-}" in '' | \#*) continue ;; esac
    grep -qxF "ok $artifact $image" "$record" 2>/dev/null || printf '%s %s\n' "$artifact" "$image"
  done <"$matrix"
}

# El tag de la imagen de construcción: cambia si cambia cualquiera de sus
# entradas, así que una imagen vieja no se reusa por error.
builder_tag() {
  local root="$1"
  printf 'norte-builder:%s\n' "$(cat "$root/scripts/baseline/Dockerfile" \
    "$root/rust-toolchain.toml" "$root/dist-workspace.toml" | sha256sum | cut -c1-12)"
}
```

- [ ] **Step 4: Run the self-test to verify it passes**

Run: `scripts/baseline/selftest.sh`
Expected: every line `ok`, last line `selftest: todo en orden`, exit 0.

- [ ] **Step 5: Add the recipe and lint**

In `justfile`, after the `gui-smoke` recipe:

```just
# Las pruebas del sistema de base (`scripts/baseline/lib.sh`) y shellcheck de
# todos sus scripts. Segundos, sin Docker: el bucle RED→GREEN de esa carpeta.
baseline-selftest:
    shellcheck -S warning scripts/baseline/*.sh scripts/gui-smoke.sh
    ./scripts/baseline/selftest.sh
```

Run: `just baseline-selftest`
Expected: no shellcheck output, `selftest: todo en orden`.

- [ ] **Step 6: Commit**

```bash
git add scripts/baseline/lib.sh scripts/baseline/selftest.sh justfile
git diff --cached --stat
git commit -F <message file>   # build(baseline): pure functions and their self-test
```

---

### Task 2: The pinned builder image

**Files:**
- Create: `scripts/baseline/Dockerfile`
- Create: `scripts/baseline/image.sh`
- Modify: `justfile` (add `baseline-image`)

**Interfaces:**
- Consumes: `builder_tag` (Task 1)
- Produces: `scripts/baseline/image.sh` — builds `$(builder_tag ROOT)` if absent, progress on stderr, prints the tag on stdout. Image contents: `rustc`/`cargo` per `rust-toolchain.toml` (with its components), `dist`, `node`/`npm` 22.23.2 at `/opt/node/bin`, `git`, `objdump`, the WebKitGTK/GTK/libsoup dev packages, `lld`, `clang`, `cmake`, `patchelf`. `CARGO_HOME=/opt/cargo`, `RUSTUP_HOME=/opt/rustup`. `git` trusts every directory.

- [ ] **Step 1: Write the Dockerfile**

`scripts/baseline/Dockerfile`:

```dockerfile
# La imagen de construcción de todo lo que se publica (ADR 0112). Ubuntu 22.04
# es la base más vieja con WebKitGTK 4.1, y su glibc (2.35) es el suelo.
# El digest está fijado: la misma imagen hoy y dentro de un año.
FROM ubuntu@sha256:829f6df217bcbae2b371026e81711d1a787c61b2967ad09d015063663ebafbf7

ARG NODE_VERSION=22.23.2
ENV DEBIAN_FRONTEND=noninteractive

# `.github/workflows/gui.yml` más lo que el host da por supuesto: `lld` porque
# `.cargo/config.toml` fuerza `-fuse-ld=lld`, `clang`/`cmake` para aws-lc-sys,
# `file`/`xz-utils` para los bundlers, `binutils` para `objdump`.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential clang cmake lld pkg-config curl ca-certificates git file \
      xz-utils binutils patchelf \
      libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
      libjavascriptcoregtk-4.1-dev librsvg2-dev \
 && rm -rf /var/lib/apt/lists/*

# Node exacto, con la suma que publica nodejs.org.
RUN set -eu; t="node-v${NODE_VERSION}-linux-x64.tar.xz"; \
    base="https://nodejs.org/dist/v${NODE_VERSION}"; cd /tmp; \
    curl -fsSLO "$base/$t"; \
    curl -fsSL "$base/SHASUMS256.txt" | grep " $t\$" | sha256sum -c -; \
    mkdir -p /opt/node; tar -xJf "$t" -C /opt/node --strip-components=1; rm "$t"

ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/node/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

# Toolchain y cargo-dist salen de los ficheros del repo, no de números
# escritos aquí: cambiar cualquiera de los dos cambia el tag de la imagen.
COPY rust-toolchain.toml dist-workspace.toml /opt/norte/
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal --default-toolchain none \
 && cd /opt/norte && rustup toolchain install \
 && rustc --version
RUN v="$(grep -E '^cargo-dist-version' /opt/norte/dist-workspace.toml | cut -d'"' -f2)" \
 && cd /opt/norte && cargo install cargo-dist --version "$v" --locked \
 && rm -rf /opt/cargo/registry /opt/cargo/git

# El repo llega por un volumen de otro dueño.
RUN git config --system --add safe.directory '*'
```

- [ ] **Step 2: Write `image.sh`**

`scripts/baseline/image.sh`:

```bash
#!/usr/bin/env bash
# Construye la imagen de construcción si su tag no existe y escribe el tag.
# El contexto de Docker son TRES ficheros, no el repo: el repo tiene un
# `target/` de decenas de GB que Docker copiaría entero.
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

tag="$(builder_tag "$RAIZ")"
if ! docker image inspect "$tag" >/dev/null 2>&1; then
  echo "construyendo $tag" >&2
  tar -c -C "$RAIZ/scripts/baseline" Dockerfile -C "$RAIZ" rust-toolchain.toml dist-workspace.toml |
    docker build -t "$tag" - >&2
fi
printf '%s\n' "$tag"
```

`chmod +x scripts/baseline/image.sh`

- [ ] **Step 3: Add the recipe**

```just
# La imagen de construcción de la base (Ubuntu 22.04 fijada, Node 22.23.2, el
# toolchain de `rust-toolchain.toml`, cargo-dist de `dist-workspace.toml`). Se
# construye una vez; su tag cambia solo si cambia alguna de esas entradas.
baseline-image:
    ./scripts/baseline/image.sh
```

- [ ] **Step 4: Build it and check what is inside**

Run (> 5 min cold: detach it, log to the scratchpad, Monitor the pid):
`just baseline-image`
Expected: last stdout line `norte-builder:<12 hex>`.

Then, with that tag:

```bash
docker run --rm norte-builder:<tag> bash -c 'rustc --version; cargo clippy --version; dist --version; node --version; ldd --version | head -1; objdump --version | head -1'
```

Expected: `rustc 1.96.1`, a clippy line, `cargo-dist 0.32.0`, `v22.23.2`, `GLIBC 2.35`, a GNU objdump line.
If `rustup toolchain install` without arguments is rejected by the rustup the
installer fetched, replace that line with
`ch="$(grep -E '^channel' rust-toolchain.toml | cut -d'"' -f2)" && rustup toolchain install "$ch" --profile minimal -c rustfmt -c clippy -c llvm-tools-preview`
and rebuild — the component list is the one in `rust-toolchain.toml`.

Run `just baseline-image` again. Expected: prints the same tag at once, no build output.

- [ ] **Step 5: Lint and commit**

Run: `just baseline-selftest` — expected clean.

```bash
git add scripts/baseline/Dockerfile scripts/baseline/image.sh justfile
git diff --cached --stat
git commit -F <message file>   # build(baseline): a pinned builder image keyed by its inputs
```

---

### Task 3: Build every artefact of a ref

**Files:**
- Create: `scripts/baseline/build.sh`
- Modify: `justfile` (add `baseline-build`)

**Interfaces:**
- Consumes: `image.sh` (Task 2); `glibc_max`, `manifest_sums`, `manifest_problems` (Task 1)
- Produces: `scripts/baseline/build.sh [REF]` — all progress on stderr; on success prints ONE stdout line, the absolute output dir `target/baseline/<revision>`; exits non-zero if `manifest_problems` reports anything. Docker volumes: `norte-baseline-registry` (cargo registry), `norte-baseline-target` (cargo target).

- [ ] **Step 1: Write `build.sh`**

`scripts/baseline/build.sh`:

```bash
#!/usr/bin/env bash
# Construye TODO lo que se publicaría de una referencia —tarballs e
# instaladores de `dist`, y el deb/rpm/AppImage de la ventana— dentro de la
# imagen de construcción, y deja un MANIFEST que dice qué glibc pide cada
# binario y qué revisión dice ser (ADR 0112).
#
#   scripts/baseline/build.sh [ref]      # por defecto HEAD
#
# El repo entra por un clon DENTRO del contenedor desde el `.git` montado en
# solo lectura: la revisión (`git describe`) sale del ref pedido y no de
# HEAD, y `dist` puede hacer su `source.tar.gz`. El `target/` del host no se
# toca.
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

REF="${1:-HEAD}"
FLOOR="${NORTE_GLIBC_FLOOR:-2.35}"

for c in docker git bsdtar objdump; do
  command -v "$c" >/dev/null || { echo "hace falta $c" >&2; exit 1; }
done

commit="$(git -C "$RAIZ" rev-parse --verify "$REF^{commit}")"
revision="$(git -C "$RAIZ" describe --tags --always --long "$commit")"
gitdir="$(git -C "$RAIZ" rev-parse --path-format=absolute --git-common-dir)"
SALIDA="$RAIZ/target/baseline/$revision"
imagen="$("$RAIZ/scripts/baseline/image.sh")"

echo "ref $REF · commit ${commit:0:12} · revisión $revision · $imagen" >&2
rm -rf "$SALIDA"
mkdir -p "$SALIDA"

# shellcheck disable=SC2016  # se expande DENTRO del contenedor
DENTRO='
set -euo pipefail
export CARGO_TARGET_DIR=/target
git clone -q /repo.git /src
cd /src
git checkout -q "$COMMIT"
triple="$(rustc -vV | grep "^host:" | cut -d" " -f2)"
rm -rf /target/distrib /src/target/distrib /target/release/bundle

echo "--- dist: norte y ntc"
dist build --artifacts=local --target="$triple"
dist build --artifacts=global --target="$triple"

echo "--- webview"
(cd crates/norte-gui-tauri/ui && npm ci --no-audit --no-fund && npm run build)

echo "--- paquete de la ventana, con los MISMOS norte y ntc que los tarballs"
mkdir -p crates/norte-gui-tauri/binaries
for b in norte ntc; do
  src="$(find /target -type f -perm -u+x -path "*/dist/$b" -print -quit)"
  [ -n "$src" ] || { echo "dist no dejó el binario $b bajo /target" >&2; exit 1; }
  cp "$src" "crates/norte-gui-tauri/binaries/$b-$triple"
done
(cd crates/norte-gui-tauri && NO_STRIP=1 APPIMAGE_EXTRACT_AND_RUN=1 ./ui/node_modules/.bin/tauri build)

mkdir -p /out/dist /out/gui
distrib=/target/distrib
[ -d "$distrib" ] || distrib=/src/target/distrib
find "$distrib" -maxdepth 1 -type f -exec cp {} /out/dist/ \;
find /target/release/bundle -maxdepth 2 -type f \( -name "*.deb" -o -name "*.rpm" -o -name "*.AppImage" \) -exec cp {} /out/gui/ \;
chown -R "$HOST_UID:$HOST_GID" /out
'

docker run --rm \
  -v "$gitdir:/repo.git:ro" \
  -v norte-baseline-registry:/opt/cargo/registry \
  -v norte-baseline-target:/target \
  -v "$SALIDA:/out" \
  -e COMMIT="$commit" \
  -e HOST_UID="$(id -u)" \
  -e HOST_GID="$(id -g)" \
  "$imagen" bash -c "$DENTRO" >&2

# --- MANIFEST: se abre lo que se publicaría y se mira DENTRO -------------
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/tar" "$tmp/deb"
for t in "$SALIDA"/dist/*.tar.xz; do tar -xf "$t" -C "$tmp/tar"; done
deb="$(find "$SALIDA/gui" -name '*.deb' -print -quit)"
[ -n "$deb" ] || { echo "la build no dejó .deb" >&2; exit 1; }
bsdtar -xOf "$deb" 'data.tar.*' | bsdtar -x -C "$tmp/deb"

{
  echo "ref $REF"
  echo "commit $commit"
  echo "revision $revision"
  echo "glibc-floor $FLOOR"
  echo "builder $imagen"
  while IFS= read -r bin; do
    rel="${bin#"$tmp"/}"
    v="$(objdump -T "$bin" | glibc_max)"
    echo "glibc $rel ${v:-none}"
    case "${bin##*/}" in
      norte | ntc) echo "version $rel $("$bin" --version)" ;;
    esac
  done < <(find "$tmp" -type f \( -name norte -o -name ntc -o -name norte-gui \) | LC_ALL=C sort)
} >"$SALIDA/MANIFEST"

manifest_sums "$SALIDA"

problemas="$(manifest_problems "$SALIDA/MANIFEST")"
if [ -n "$problemas" ]; then
  printf 'la build NO vale:\n%s\n' "$problemas" >&2
  exit 1
fi
printf '%s\n' "$SALIDA"
```

`chmod +x scripts/baseline/build.sh`

- [ ] **Step 2: Add the recipe**

```just
# Construye todo lo publicable de una referencia en la imagen de la base y
# deja `target/baseline/<revisión>/` con dist/, gui/, MANIFEST y SHA256SUMS.
# Falla si un binario pide glibc por encima del suelo (2.35) o dice otra
# revisión. Lento en frío; los volúmenes `norte-baseline-*` lo abaratan.
baseline-build ref="HEAD":
    ./scripts/baseline/build.sh {{ref}}
```

- [ ] **Step 3: Lint**

Run: `just baseline-selftest` — expected clean.

- [ ] **Step 4: Run it on HEAD and read the result**

Detached (it takes long cold), then:

Run: `just baseline-build`
Expected: last stdout line `…/target/baseline/<revision>`. Then:

```bash
ls <dir>/dist <dir>/gui
cat <dir>/MANIFEST
```

Expected: `dist/` has both `.tar.xz` with `.sha256`, both `-installer.sh`, `sha256.sum`, `source.tar.gz`; `gui/` has `.deb`, `.rpm`, `.AppImage`; MANIFEST has `glibc` lines for `tar/…/norte`, `tar/…/ntc`, `deb/usr/bin/norte`, `deb/usr/bin/ntc`, `deb/usr/bin/norte-gui`, all ≤ 2.35, and `version` lines ending in `(<revision>)`.

If `dist build` fails inside the clone, read its error before changing anything:
the likely causes are a missing system tool (add it to the Dockerfile's apt
list, which re-tags the image) or `dist` wanting the host triple spelled
differently. Do not work around a failure by skipping `--artifacts=global`.

- [ ] **Step 5: Prove the gate bites**

Run: `NORTE_GLIBC_FLOOR=2.30 just baseline-build`
Expected: exit non-zero, stderr `la build NO vale:` listing each binary `pide glibc 2.34, por encima del suelo 2.30`. (It rebuilds warm; the point is that the gate is not decorative.)

- [ ] **Step 6: Commit**

```bash
git add scripts/baseline/build.sh justfile
git diff --cached --stat
git commit -F <message file>   # build(baseline): build every artefact of a ref and refuse one above the glibc floor
```

---

### Task 4: The smoke matrix

**Files:**
- Create: `scripts/baseline/matrix.txt`
- Create: `scripts/baseline/smoke-inside.sh`
- Create: `scripts/baseline/smoke.sh`
- Modify: `justfile` (add `baseline-smoke`)

**Interfaces:**
- Consumes: `image_slug` (Task 1); an output dir from Task 3 (`dist/`, `gui/`, `MANIFEST`)
- Produces:
  - `smoke.sh DIR [ARTEFACT]` — runs every matrix line (or those of ARTEFACT), `NORTE_SMOKE_JOBS` in parallel (default 4); appends `ok <artefact> <image>` to `DIR/SMOKE`; writes `DIR/smoke/<artefact>-<slug>.log`; exit non-zero if any run failed
  - `smoke.sh --one DIR REV ARTEFACT IMAGE` — one run; `REV` may be empty (no revision check)
  - Artefact names: `tarball`, `installer`, `deb`, `rpm`, `appimage`

- [ ] **Step 1: Write the matrix**

`scripts/baseline/matrix.txt`:

```
# artefacto imagen@digest — una ejecución de humo por línea (ADR 0112).
# Los digests fijan la imagen: una distribución que se actualiza no cambia
# el resultado de un humo sin que cambie este fichero.

tarball ubuntu:22.04@sha256:829f6df217bcbae2b371026e81711d1a787c61b2967ad09d015063663ebafbf7
tarball ubuntu:24.04@sha256:224a1869083a311ef3f13648a154ba79832fbef6364d31493642ca03082da254
tarball debian:bookworm@sha256:6ebd97fa83deb272194a2cf015b3d26a4d538e9ad3a7a79d544c8af5b0a01443
tarball debian:trixie@sha256:f324c7ff54321e8d9c588493a20244965938ce0aa50bbd1022d38010e9ffc4b1
tarball fedora:41@sha256:f1a3fab47bcb3c3ddf3135d5ee7ba8b7b25f2e809a47440936212a3a50957f3d

installer ubuntu:22.04@sha256:829f6df217bcbae2b371026e81711d1a787c61b2967ad09d015063663ebafbf7
installer ubuntu:24.04@sha256:224a1869083a311ef3f13648a154ba79832fbef6364d31493642ca03082da254
installer debian:bookworm@sha256:6ebd97fa83deb272194a2cf015b3d26a4d538e9ad3a7a79d544c8af5b0a01443
installer debian:trixie@sha256:f324c7ff54321e8d9c588493a20244965938ce0aa50bbd1022d38010e9ffc4b1
installer fedora:41@sha256:f1a3fab47bcb3c3ddf3135d5ee7ba8b7b25f2e809a47440936212a3a50957f3d

deb ubuntu:22.04@sha256:829f6df217bcbae2b371026e81711d1a787c61b2967ad09d015063663ebafbf7
deb ubuntu:24.04@sha256:224a1869083a311ef3f13648a154ba79832fbef6364d31493642ca03082da254
deb debian:bookworm@sha256:6ebd97fa83deb272194a2cf015b3d26a4d538e9ad3a7a79d544c8af5b0a01443
deb debian:trixie@sha256:f324c7ff54321e8d9c588493a20244965938ce0aa50bbd1022d38010e9ffc4b1

rpm fedora:41@sha256:f1a3fab47bcb3c3ddf3135d5ee7ba8b7b25f2e809a47440936212a3a50957f3d

appimage ubuntu:22.04@sha256:829f6df217bcbae2b371026e81711d1a787c61b2967ad09d015063663ebafbf7
appimage debian:trixie@sha256:f324c7ff54321e8d9c588493a20244965938ce0aa50bbd1022d38010e9ffc4b1
appimage fedora:41@sha256:f1a3fab47bcb3c3ddf3135d5ee7ba8b7b25f2e809a47440936212a3a50957f3d
```

- [ ] **Step 2: Write the in-container script**

`scripts/baseline/smoke-inside.sh`:

```bash
# shellcheck shell=bash
# Corre DENTRO de un contenedor de humo (lo lee `bash -s`). Recibe
# ARTIFACT (tarball|installer|deb|rpm|appimage), REV (vacío = no comprobar la
# revisión) y los artefactos en /a, montado en solo lectura. Lo que prueba:
# que se instala con lo que la distribución trae, que `--version` dice la
# revisión esperada, que llega un listado, y —si hay ventana— que no muere al
# arrancar bajo Xvfb.
set -euo pipefail
: "${ARTIFACT:?}"
REV="${REV:-}"

# Nombres lógicos → paquetes de cada familia.
instalar() {
  local pkgs=() p
  if command -v apt-get >/dev/null; then
    for p in "$@"; do
      case "$p" in xz) pkgs+=(xz-utils) ;; *) pkgs+=("$p") ;; esac
    done
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends ca-certificates "${pkgs[@]}" >/dev/null
  else
    for p in "$@"; do
      case "$p" in xvfb) pkgs+=(xorg-x11-server-Xvfb) ;; *) pkgs+=("$p") ;; esac
    done
    dnf install -y -q "${pkgs[@]}" >/dev/null
  fi
}

version_ok() {
  local out
  out="$("$1" --version)"
  echo "$out"
  if [ -n "$REV" ] && [[ "$out" != *"($REV)" ]]; then
    echo "revisión inesperada: se esperaba ($REV)" >&2
    return 1
  fi
}

listado() {
  local salida
  mkdir -p /tmp/casa && : >/tmp/casa/hola.txt
  # `norte ls` levanta el daemon si no hay ninguno: el camino de la ventana.
  salida="$("$1" ls /tmp/casa)"
  grep -q 'hola.txt' <<<"$salida" || { echo "el listado no trajo el fichero: $salida" >&2; return 1; }
  echo "llega el listado"
}

# No se comprueba que PINTE —eso pide una persona—, sino que no se cae al
# arrancar: una dependencia de WebKitGTK que falte muere aquí.
ventana() {
  local pid
  export DISPLAY=:99
  Xvfb :99 -screen 0 1280x800x24 &
  sleep 2
  "$@" &
  pid=$!
  sleep 8
  if kill -0 "$pid" 2>/dev/null; then
    echo "la ventana sigue viva a los 8 segundos"
    kill "$pid" 2>/dev/null || true
  else
    wait "$pid" 2>/dev/null || true
    echo "la ventana MURIÓ al arrancar" >&2
    return 1
  fi
}

dueno_de() {
  local b
  for b in norte-gui norte ntc; do
    "$@" "/usr/bin/$b" >/dev/null 2>&1 || { echo "el paquete no instala /usr/bin/$b" >&2; return 1; }
  done
  echo "los tres binarios los pone el paquete"
}

case "$ARTIFACT" in
  tarball)
    instalar xz
    mkdir -p /opt/t
    for t in /a/*.tar.xz; do tar -xJf "$t" -C /opt/t; done
    norte="$(find /opt/t -type f -name norte -print -quit)"
    ntc="$(find /opt/t -type f -name ntc -print -quit)"
    [ -n "$norte" ] && [ -n "$ntc" ] || { echo "faltan norte o ntc en los tarballs" >&2; exit 1; }
    version_ok "$norte"
    version_ok "$ntc"
    listado "$norte"
    ;;
  installer)
    # El instalador descarga de GitHub; `INSTALLER_DOWNLOAD_URL` lo apunta a
    # los artefactos locales, y `curl -sSfL` lee `file://`.
    instalar curl xz
    for s in /a/*-installer.sh; do
      INSTALLER_DOWNLOAD_URL=file:///a INSTALLER_NO_MODIFY_PATH=1 sh "$s"
    done
    version_ok "$HOME/.cargo/bin/norte"
    version_ok "$HOME/.cargo/bin/ntc"
    listado "$HOME/.cargo/bin/norte"
    ;;
  deb)
    instalar xvfb
    dpkg -i /a/*.deb >/dev/null 2>&1 || apt-get -f install -y -qq >/dev/null
    dueno_de dpkg -S
    version_ok /usr/bin/norte
    version_ok /usr/bin/ntc
    listado /usr/bin/norte
    ventana /usr/bin/norte-gui
    ;;
  rpm)
    instalar xvfb
    dnf install -y -q /a/*.rpm >/dev/null
    dueno_de rpm -qf
    version_ok /usr/bin/norte
    version_ok /usr/bin/ntc
    listado /usr/bin/norte
    ventana /usr/bin/norte-gui
    ;;
  appimage)
    # Un contenedor no tiene FUSE: se extrae y se corre lo extraído.
    instalar xvfb
    cd /tmp
    cp /a/*.AppImage app.AppImage
    chmod +x app.AppImage
    ./app.AppImage --appimage-extract >/dev/null
    version_ok squashfs-root/usr/bin/norte
    version_ok squashfs-root/usr/bin/ntc
    listado squashfs-root/usr/bin/norte
    ventana squashfs-root/AppRun
    ;;
  *)
    echo "artefacto desconocido: $ARTIFACT" >&2
    exit 2
    ;;
esac
echo "humo OK: $ARTIFACT"
```

- [ ] **Step 3: Write the runner**

`scripts/baseline/smoke.sh`:

```bash
#!/usr/bin/env bash
# Humo de los artefactos de una build de la base, en contenedores limpios,
# según `matrix.txt` (ADR 0112).
#
#   scripts/baseline/smoke.sh DIR [artefacto]
#   scripts/baseline/smoke.sh --one DIR REV ARTEFACTO IMAGEN
#
# Cada ejecución deja su log en DIR/smoke/ y, si pasa, una línea
# `ok artefacto imagen` en DIR/SMOKE. Un fallo no para a los demás: al final
# se sabe TODO lo que falla. NORTE_SMOKE_JOBS ejecuciones a la vez (4).
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"
MATRIX="$RAIZ/scripts/baseline/matrix.txt"

rojo() { printf '\033[31m%s\033[0m\n' "$*" >&2; }
verde() { printf '\033[32m%s\033[0m\n' "$*"; }

una() {
  local dir="$1" rev="$2" artifact="$3" image="$4" sub log
  case "$artifact" in
    tarball | installer) sub=dist ;;
    deb | rpm | appimage) sub=gui ;;
    *) rojo "artefacto desconocido: $artifact"; return 2 ;;
  esac
  mkdir -p "$dir/smoke"
  log="$dir/smoke/$artifact-$(image_slug "$image").log"
  if docker run --rm -i \
    -v "$dir/$sub:/a:ro" \
    -e ARTIFACT="$artifact" \
    -e REV="$rev" \
    -e DEBIAN_FRONTEND=noninteractive \
    "$image" bash -s <"$RAIZ/scripts/baseline/smoke-inside.sh" >"$log" 2>&1; then
    echo "ok $artifact $image" >>"$dir/SMOKE"
    verde "ok     $artifact $(image_slug "$image")"
  else
    rojo "FALLA  $artifact $(image_slug "$image") — $log"
    return 1
  fi
}

if [ "${1:-}" = "--one" ]; then
  shift
  una "$@"
  exit
fi

DIR="$(realpath "${1:?uso: smoke.sh DIR [artefacto]}")"
SOLO="${2:-}"
[ -f "$DIR/MANIFEST" ] || { rojo "no hay $DIR/MANIFEST: ¿es una salida de build.sh?"; exit 1; }
REV="$(awk '$1 == "revision" { print $2 }' "$DIR/MANIFEST")"

lineas="$(grep -vE '^[[:space:]]*(#|$)' "$MATRIX" | awk -v solo="$SOLO" 'solo == "" || $1 == solo')"
[ -n "$lineas" ] || { rojo "ninguna línea de la matriz para «$SOLO»"; exit 1; }
[ -n "$SOLO" ] || : >"$DIR/SMOKE"

printf '%s\n' "$lineas" |
  xargs -P "${NORTE_SMOKE_JOBS:-4}" -L 1 "$0" --one "$DIR" "$REV"
```

`chmod +x scripts/baseline/smoke.sh`

(`xargs` exits 123 if any run failed, which `set -e` propagates.)

- [ ] **Step 4: Add the recipe and lint**

```just
# Humo de una build de la base en la matriz de `scripts/baseline/matrix.txt`.
# `artefacto` limita a uno (tarball, installer, deb, rpm, appimage).
baseline-smoke dir artefacto="":
    ./scripts/baseline/smoke.sh {{dir}} {{artefacto}}
```

Run: `just baseline-selftest` — expected clean.

- [ ] **Step 5: Run one artefact first, then all**

Run: `just baseline-smoke <dir from Task 3> tarball`
Expected: five `ok tarball …` lines.

Run: `just baseline-smoke <dir>`
Expected: 18 `ok` lines, exit 0, `SMOKE` has 18 lines.

A red run is a finding, not noise: read `<dir>/smoke/<artefacto>-<imagen>.log`,
reproduce with `scripts/baseline/smoke.sh --one <dir> <rev> <artefacto> <imagen>`,
and fix the cause (a package the smoke container lacks, a real missing
dependency of the package, an installer that does not accept `file://`). If the
AppImage genuinely cannot start in a container, say so in the ADR and remove
those lines from the matrix with the reason written in a comment there — do not
silently drop them.

- [ ] **Step 6: Commit**

```bash
git add scripts/baseline/matrix.txt scripts/baseline/smoke-inside.sh scripts/baseline/smoke.sh justfile
git diff --cached --stat
git commit -F <message file>   # build(baseline): smoke every artefact on a pinned matrix
```

---

### Task 5: Verify, one entry point, and retire the old recipes

**Files:**
- Create: `scripts/baseline/verify.sh`
- Create: `scripts/baseline/all.sh`
- Modify: `scripts/gui-smoke.sh` (becomes a wrapper)
- Delete: `scripts/gui-baseline.sh`, `scripts/dist-smoke.sh`
- Modify: `justfile` — add `baseline`, `baseline-verify`, `baseline-publish`, `baseline-prune`; remove `dist`, `dist-smoke`, `dist-publish`, `gui-baseline`, `gui-publish`

**Interfaces:**
- Consumes: `manifest_check_sums`, `manifest_problems`, `smoke_missing` (Task 1); `build.sh` (Task 3); `smoke.sh` (Task 4)
- Produces: `verify.sh DIR` (exit 0 iff sums, floor, versions and full matrix hold); `all.sh [REF]`

- [ ] **Step 1: Write `verify.sh`**

```bash
#!/usr/bin/env bash
# ¿Se puede publicar esta build? Solo lee DIR: sumas, suelo de glibc,
# revisión de cada binario, y un humo verde por cada línea de la matriz.
#
#   scripts/baseline/verify.sh DIR
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

DIR="$(realpath "${1:?uso: verify.sh DIR}")"
[ -f "$DIR/MANIFEST" ] || { echo "no hay $DIR/MANIFEST" >&2; exit 1; }

mal=0
if ! manifest_check_sums "$DIR"; then
  echo "SHA256SUMS no cuadra: algo cambió después de la build" >&2
  mal=1
fi
p="$(manifest_problems "$DIR/MANIFEST")"
if [ -n "$p" ]; then
  printf '%s\n' "$p" >&2
  mal=1
fi
m="$(smoke_missing "$RAIZ/scripts/baseline/matrix.txt" "$DIR/SMOKE")"
if [ -n "$m" ]; then
  printf '%s\n' "$m" | sed 's/^/humo sin pasar: /' >&2
  mal=1
fi
if [ "$mal" -eq 0 ]; then
  printf '\033[32mverificado: %s\033[0m\n' "$DIR"
fi
exit "$mal"
```

- [ ] **Step 2: Write `all.sh`**

```bash
#!/usr/bin/env bash
# Una referencia, de principio a fin: build, humo y verificación.
#
#   scripts/baseline/all.sh [ref]
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
dir="$("$RAIZ/scripts/baseline/build.sh" "${1:-HEAD}")"
# El humo puede fallar; quien dice si la build vale es verify.
"$RAIZ/scripts/baseline/smoke.sh" "$dir" || true
exec "$RAIZ/scripts/baseline/verify.sh" "$dir"
```

`chmod +x scripts/baseline/verify.sh scripts/baseline/all.sh`

- [ ] **Step 3: Turn `gui-smoke.sh` into a wrapper**

Replace the whole file:

```bash
#!/usr/bin/env bash
# Humo del paquete que `just gui-package` dejó en ESTA máquina, por el mismo
# cuerpo que la matriz de la base (`scripts/baseline/smoke-inside.sh`).
#
#   scripts/gui-smoke.sh [imagen]
#
# Es el bucle de desarrollo: no comprueba revisión ni suelo de glibc, y un
# paquete construido aquí no arranca en una distribución vieja. Lo que se
# publicaría pasa por `just baseline` (ADR 0112).
set -euo pipefail
IMAGEN="${1:-debian:trixie}"
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "${IMAGEN##*/}" in
  fedora* | rockylinux* | almalinux* | centos*) artefacto=rpm ;;
  *) artefacto=deb ;;
esac
paquete="$(find "$RAIZ/target/release/bundle/$artefacto" -name "*.$artefacto" -print -quit 2>/dev/null || true)"
[ -n "$paquete" ] || { echo "no hay .$artefacto en target/release/bundle — corre \`just gui-package\`" >&2; exit 1; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/gui"
cp "$paquete" "$tmp/gui/"
if ! "$RAIZ/scripts/baseline/smoke.sh" --one "$tmp" "" "$artefacto" "$IMAGEN"; then
  cat "$tmp"/smoke/*.log >&2
  exit 1
fi
```

- [ ] **Step 4: Edit the justfile**

Remove the recipes `dist`, `dist-smoke`, `dist-publish`, `gui-baseline` and `gui-publish`, each with its comment block. Add, next to `baseline-smoke`:

```just
# Una referencia de principio a fin: build en la base, humo en la matriz y
# verificación. Verde = esta build se podría publicar.
baseline ref="HEAD":
    ./scripts/baseline/all.sh {{ref}}

# ¿Se puede publicar esta build? Sumas, suelo, revisiones y matriz completa.
baseline-verify dir:
    ./scripts/baseline/verify.sh {{dir}}

# Sube a la release del tag una build VERIFICADA de ese mismo tag. Solo
# publica: la release tiene que existir. Rechaza una build de otro commit
# (su revisión tiene que ser `<tag>-0-g…`).
baseline-publish tag dir:
    #!/usr/bin/env bash
    set -euo pipefail
    ./scripts/baseline/verify.sh {{dir}}
    rev="$(awk '$1 == "revision" { print $2 }' {{dir}}/MANIFEST)"
    case "$rev" in
        {{tag}}-0-g*) ;;
        *) echo "la build es de $rev, no de {{tag}}" >&2; exit 1 ;;
    esac
    gh release upload {{tag}} \
        $(find {{dir}}/dist {{dir}}/gui -maxdepth 1 -type f) \
        {{dir}}/SHA256SUMS \
        docs/schema/proto.schema.json docs/schema/norte.schema.json docs/schema/keymap.schema.json \
        --clobber

# Lo que ocupa la base fuera de `target/`: volúmenes e imágenes de Docker.
baseline-prune:
    docker volume rm -f norte-baseline-registry norte-baseline-target norte-baseline-cargo norte-baseline-rustup norte-baseline-node
    docker image ls -q norte-builder | xargs -r docker image rm -f
    rm -rf target/baseline
```

(`norte-baseline-cargo`, `-rustup`, `-node` are the volumes the retired `gui-baseline.sh` created.)

Delete the scripts: `git rm scripts/gui-baseline.sh scripts/dist-smoke.sh`.

Check nothing else calls the removed names:

Run: `grep -rn -E 'dist-smoke|dist-publish|gui-baseline|gui-publish|just dist\b' --include='*.md' --include=justfile --include='*.sh' --include='*.yml' . | grep -v '^./target' | grep -v 'docs/superpowers/plans/2026-09-15-baseline-release-system.md'`
Expected: only lines in `CLAUDE.md`, `.claude/commands/release-check.md`, `docs/adr/0111-*` and `CHANGELOG.md` history — Task 6 handles the first three; CHANGELOG history is not rewritten.

- [ ] **Step 5: Verify**

Run: `just baseline-selftest` — clean.
Run: `just --list | grep baseline` — shows `baseline`, `baseline-build`, `baseline-image`, `baseline-prune`, `baseline-publish`, `baseline-selftest`, `baseline-smoke`, `baseline-verify`.
Run: `just baseline-verify <dir from Task 4>` — `verificado: …`, exit 0.
Corrupt a copy to prove verify bites: `cp -r <dir> <scratchpad>/copia && echo x >> <scratchpad>/copia/gui/*.deb && just baseline-verify <scratchpad>/copia` — exit 1, `SHA256SUMS no cuadra`.
Run: `just gui-smoke debian:bookworm` against the host bundle in `target/release/bundle` (exists from earlier `gui-package` runs) — exit 0.

- [ ] **Step 6: Commit**

```bash
git add scripts/baseline scripts/gui-smoke.sh justfile   # the two deletions were staged by `git rm` in Step 4
git diff --cached --stat
git commit -F <message file>   # build(baseline): verify, one entry point, and retire host-built release recipes
```

---

### Task 6: Documentation

**Files:**
- Create: `docs/adr/0112-release-artefacts-are-built-in-one-pinned-image-and-smoked-per-distribution.md`
- Modify: `docs/adr/0111-the-window-package-is-built-on-ubuntu-22-04.md` (status line)
- Modify: `docs/adr/README.md` (row 0112; 0111 status)
- Modify: `CLAUDE.md` (packaging paragraph)
- Modify: `.claude/commands/release-check.md` (step 7)

- [ ] **Step 1: ADR 0112**

MADR, like 0111. Must contain:
- **Context:** the six problems of this plan's *Context*, with the measured numbers (host glibc 2.44, dist `norte` GLIBC_2.39, Ubuntu 22.04 2.35, Debian 12 2.36, the `-1-gaecbc039` revision).
- **Options:** (A) one pinned builder image, all artefacts, container smoke matrix — chosen; (B) keep host `dist` and add container smoke only — detects, does not fix; (C) `cargo-zigbuild` to a glibc target on the host — ADR 0021 already calls cross-building aws-lc-rs fragile; (D) CI runners — Actions has not run since 2026-07-13 and publishing is local.
- **Decision:** the recipes and files of this plan; floor 2.35 enforced by `manifest_problems`; matrix pinned by digest in `matrix.txt`; the build is a clone of the ref, so the revision is the ref's; `dist`, `dist-smoke`, `dist-publish`, `gui-baseline`, `gui-publish` removed; `baseline-publish` accepts only a verified build of exactly that tag.
- **Consequences:** positive (one floor for everything, tarballs start on 22.04, AppImage smoked, reproducible image); negative (Docker required to release; volumes + image on disk, `baseline-prune`; ~18 container runs per release; the image moves when Ubuntu 22.04 leaves support in 2027; still unsigned; `cargo-semver-checks` skips prerelease bumps, unchanged).
- Status accepted; related ADR 0021, 0087, 0111.

- [ ] **Step 2: ADR 0111 and the index**

In ADR 0111 change `- Status: accepted` to `- Status: accepted; its recipes (`gui-baseline`, `gui-publish`) superseded by ADR 0112`. In `docs/adr/README.md` add the 0112 row after 0111 and set 0111's status cell to `superseded in part by 0112`.

- [ ] **Step 3: CLAUDE.md**

In the "Disk budget" section, after the paragraph that ends with "…and deliberately not CI: a clean-install failure should not depend on who pressed the button.", add:

```markdown
**What would be released is built by `just baseline [ref]`, never on this
machine** (ADR 0112). This machine's glibc is newer than most installed Linux
systems; a `norte` built here needed GLIBC_2.39 and did not start on Ubuntu
22.04 or Debian 12, and the smoke that ran it here was green. `baseline` clones
the ref inside a pinned Ubuntu 22.04 image, builds the `dist` tarballs and
installers and the window's deb/rpm/AppImage there, fails if any binary needs
glibc above 2.35 or reports another revision, smoke-tests every artefact on
the digest-pinned matrix in `scripts/baseline/matrix.txt`, and verifies.
`baseline-publish <tag> <dir>` uploads only a verified build of exactly that
tag. The loop for its scripts is `just baseline-selftest` (seconds, no Docker).
`just gui-smoke` stays for a package `gui-package` built here, and says
nothing about old distributions. `just baseline-prune` removes its Docker
volumes and images.
```

- [ ] **Step 4: release-check**

In `.claude/commands/release-check.md` replace step 7:

Old: ``7. Confirm the artefacts: `just dist && just dist-smoke`. A release that does not start is the one packaging failure the user finds before we do.``

New: ``7. Confirm the artefacts: `just baseline <tag>` must end with `verificado:`. It builds every artefact of the tag on the pinned Ubuntu 22.04 image, refuses a binary above glibc 2.35 or with another revision, and smoke-tests each artefact on the pinned distribution matrix. A release that does not start is the one packaging failure the user finds before we do; a smoke run on the build machine cannot see it.``

- [ ] **Step 5: Commit**

```bash
git add docs/adr/0112-*.md docs/adr/0111-*.md docs/adr/README.md CLAUDE.md .claude/commands/release-check.md
git diff --cached --stat
git commit -F <message file>   # docs: ADR 0112, release artefacts come from one pinned image
```

---

### Task 7: End to end on the tag

**Files:** none (verification and memory only)

- [ ] **Step 1: Full run on the local tag**

`v0.3.0-alpha.4` exists locally on `3d69a6fc` (not pushed). Detached, with a Monitor:

Run: `just baseline v0.3.0-alpha.4`
Expected: build dir `target/baseline/v0.3.0-alpha.4-0-g3d69a6fc`, 18 `ok` smoke lines, final `verificado: …`, exit 0.

Note: the tagged commit predates this branch, but the scripts run from the
working tree and only the SOURCE comes from the tag — that is the design.

- [ ] **Step 2: Record cost**

Run: `docker system df -v | grep -E 'norte-baseline|norte-builder'` and `du -sh target/baseline`
Write the build wall time, smoke wall time and the disk figures into ADR 0112's Consequences (amend commit on the branch, or a follow-up `docs:` commit).

- [ ] **Step 3: Memory**

Update `fase7-base-vieja-0111.md` (or a new `sistema-de-base-0112.md` linked from it) and `MEMORY.md`: the recipes, the floor, the matrix, the measured costs, and the traps met during execution.

- [ ] **Step 4: Report**

Tell Oscar: what `just baseline v0.3.0-alpha.4` produced, what failed on the way and why, and that nothing was pushed or published.
