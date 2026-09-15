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
#
# `escritorio` es lo que un AppImage da por hecho: la *excludelist* de
# AppImage/linuxdeploy deja FUERA del paquete las bibliotecas que todo
# escritorio trae (GL/EGL, X11/xcb, fontconfig, freetype, harfbuzz, gbm…),
# porque llevar las suyas rompe con los drivers del sistema. Un contenedor
# mínimo no es un escritorio, y sin ellas la ventana muere con
# «libfontconfig.so.1: cannot open shared object file». La lista es la que
# `ldd` dio por ausente sobre el AppImage extraído en Debian 13 y Fedora 41,
# más lo que WebKit abre con `dlopen` y `ldd` no ve: `libGLESv2.so.2`, y el
# EGL/DRI de Mesa con el que pinta bajo Xvfb. En Fedora esos llegan como
# dependencias de mesa-libEGL; en la familia Debian hay que pedirlos.
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

# Que cada /usr/bin/<b> lo haya puesto ESTE paquete: estar en el PATH no basta.
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
    if [ -z "$norte" ] || [ -z "$ntc" ]; then
      echo "faltan norte o ntc en los tarballs" >&2
      exit 1
    fi
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
    echo "artefacto desconocido: $ARTIFACT" >&2
    exit 2
    ;;
esac
echo "humo OK: $ARTIFACT"
