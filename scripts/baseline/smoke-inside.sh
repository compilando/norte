# shellcheck shell=bash
# Runs INSIDE a smoke container (read by `bash -s`). Receives
# ARTIFACT (tarball|installer|deb|rpm|appimage), REV (empty = do not check
# the revision) and the artifacts under /a, mounted read-only. What it
# tests: that it installs with what the distribution ships, that
# `--version` reports the expected revision, that a listing comes back,
# and —if there is a window— that it does not die on start under Xvfb.
set -euo pipefail
: "${ARTIFACT:?}"
REV="${REV:-}"

# Logical names → each family's packages.
#
# `desktop` is what an AppImage takes for granted: AppImage/linuxdeploy's
# *excludelist* leaves OUT of the package the libraries every desktop ships
# (GL/EGL, X11/xcb, fontconfig, freetype, harfbuzz, gbm…), because carrying
# its own breaks against the system's drivers. A minimal container is not a
# desktop, and without them the window dies with
# "libfontconfig.so.1: cannot open shared object file". The list is what
# `ldd` reported as absent on the AppImage extracted on Debian 13 and
# Fedora 41, plus what WebKit opens with `dlopen` and `ldd` does not see:
# `libGLESv2.so.2`, and the Mesa EGL/DRI it paints with under Xvfb. On
# Fedora those arrive as mesa-libEGL's dependencies; on the Debian family
# they have to be requested.
instalar() {
  local pkgs=() p
  if command -v apt-get >/dev/null; then
    for p in "$@"; do
      case "$p" in
        xz) pkgs+=(xz-utils) ;;
        escritorio)
          pkgs+=(libegl1 libgl1 libgles2 libegl-mesa0 libgl1-mesa-dri
            libx11-6 libx11-xcb1 libxcb1 libdrm2 libgbm1 libexpat1
            libfontconfig1 libfreetype6 libharfbuzz0b libfribidi0 libgpg-error0 libcom-err2)
          ;;
        *) pkgs+=("$p") ;;
      esac
    done
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends ca-certificates "${pkgs[@]}" >/dev/null
  else
    for p in "$@"; do
      case "$p" in
        xvfb) pkgs+=(xorg-x11-server-Xvfb) ;;
        escritorio)
          pkgs+=(mesa-libEGL mesa-libGL libX11 libX11-xcb libxcb libdrm mesa-libgbm expat
            fontconfig freetype harfbuzz fribidi libgpg-error libcom_err)
          ;;
        *) pkgs+=("$p") ;;
      esac
    done
    dnf install -y -q "${pkgs[@]}" >/dev/null
  fi
}

version_ok() {
  local out
  out="$("$1" --version)"
  echo "$out"
  if [ -n "$REV" ] && [[ "$out" != *"($REV)" ]]; then
    echo "unexpected revision: expected ($REV)" >&2
    return 1
  fi
}

listado() {
  local salida
  mkdir -p /tmp/casa && : >/tmp/casa/hola.txt
  # `norte ls` starts the daemon if there is none: the window's path.
  salida="$("$1" ls /tmp/casa)"
  grep -q 'hola.txt' <<<"$salida" || { echo "the listing did not bring the file: $salida" >&2; return 1; }
  echo "the listing comes through"
}

# It does not check that it PAINTS —that needs a person—, only that it does
# not fall over on start: a missing WebKitGTK dependency dies here.
ventana() {
  local pid
  export DISPLAY=:99
  Xvfb :99 -screen 0 1280x800x24 &
  sleep 2
  "$@" &
  pid=$!
  sleep 8
  if kill -0 "$pid" 2>/dev/null; then
    echo "the window is still alive after 8 seconds"
    kill "$pid" 2>/dev/null || true
  else
    wait "$pid" 2>/dev/null || true
    echo "the window DIED on start" >&2
    return 1
  fi
}

# That each /usr/bin/<b> was put there by THIS package: being on the PATH is
# not enough.
dueno_de() {
  local b
  for b in norte-gui norte ntc; do
    "$@" "/usr/bin/$b" >/dev/null 2>&1 || { echo "the package does not install /usr/bin/$b" >&2; return 1; }
  done
  echo "the package provides all three binaries"
}

case "$ARTIFACT" in
  tarball)
    instalar xz
    mkdir -p /opt/t
    for t in /a/*.tar.xz; do tar -xJf "$t" -C /opt/t; done
    norte="$(find /opt/t -type f -name norte -print -quit)"
    ntc="$(find /opt/t -type f -name ntc -print -quit)"
    if [ -z "$norte" ] || [ -z "$ntc" ]; then
      echo "norte or ntc missing from the tarballs" >&2
      exit 1
    fi
    version_ok "$norte"
    version_ok "$ntc"
    listado "$norte"
    ;;
  installer)
    # The installer downloads from GitHub; `INSTALLER_DOWNLOAD_URL` points it
    # at the local artifacts, and `curl -sSfL` reads `file://`.
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
    # A container has no FUSE: it is extracted and the extraction is run.
    instalar xvfb escritorio
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
    echo "unknown artifact: $ARTIFACT" >&2
    exit 2
    ;;
esac
echo "smoke OK: $ARTIFACT"
