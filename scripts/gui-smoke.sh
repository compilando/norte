#!/usr/bin/env bash
# Humo del PAQUETE de la ventana, en una máquina limpia.
#
# Lo que los tests de `empaquetado.rs` NO pueden decir: ésos leen
# `tauri.conf.json` y la entrada de escritorio, o sea que comprueban lo que el
# paquete PROMETE. Esto comprueba lo que el paquete HACE — que el bundler
# metió los tres binarios, que sus dependencias declaradas bastan en una
# distribución que no ha visto nunca este árbol, y que ahí dentro la ventana
# encuentra su daemon y llega un listado.
#
# Ese es el fallo que ninguna suite verde ve: se ve al instalar, y hasta ahora
# eso era en la máquina de alguien.
#
# No necesita CI. Necesita un contenedor.
#
#   scripts/gui-smoke.sh [imagen]
#   NORTE_DEB=target/baseline/ubuntu-22.04/norte_….deb scripts/gui-smoke.sh [imagen]
#   NORTE_RPM=target/baseline/ubuntu-22.04/norte-….rpm scripts/gui-smoke.sh fedora:41
#
# `imagen` es la base limpia (por defecto `debian:trixie`). Una imagen de la
# familia Fedora (fedora, rockylinux, almalinux, centos) prueba el `.rpm` con
# dnf; cualquier otra, el `.deb` con apt. Exige que `just gui-package` se haya
# corrido antes: construirlo aquí dentro obligaría a meter el toolchain entero
# en el contenedor, que es lo contrario de una máquina limpia. `NORTE_DEB` y
# `NORTE_RPM` prueban otro paquete —el de `gui-baseline`, que es el que se
# publicaría— en vez del de `target/release/bundle`.
set -euo pipefail

IMAGEN="${1:-debian:trixie}"
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE="$RAIZ/target/release/bundle"

rojo() { printf '\033[31m%s\033[0m\n' "$*" >&2; }
verde() { printf '\033[32m%s\033[0m\n' "$*"; }

case "${IMAGEN##*/}" in
  fedora* | rockylinux* | almalinux* | centos*) FORMATO=rpm ;;
  *) FORMATO=deb ;;
esac

# --- 1. El paquete existe -----------------------------------------------
if [[ $FORMATO == deb ]]; then
  ELEGIDO="${NORTE_DEB:-}"
else
  ELEGIDO="${NORTE_RPM:-}"
fi
if [[ -n "$ELEGIDO" ]]; then
  PAQUETE="$(realpath "$ELEGIDO")"
else
  PAQUETE="$(find "$BUNDLE/$FORMATO" -name "*.$FORMATO" -print -quit 2>/dev/null || true)"
fi
if [[ -z "$PAQUETE" || ! -f "$PAQUETE" ]]; then
  rojo "no hay .$FORMATO (${ELEGIDO:-$BUNDLE/$FORMATO}) — corre \`just gui-package\` primero"
  exit 1
fi
echo "paquete: $(basename "$PAQUETE") en $IMAGEN"

# --- 2. Se instala, lleva los TRES binarios, y funciona ------------------
#
# El gestor de paquetes resuelve las dependencias que el paquete DECLARA: si
# el bundler se dejó una, aquí falla, que es justo lo que no se veía. El
# contenedor no comparte nada con el host salvo el paquete.
#
# La ventana busca a `norte` como HERMANO de su propio ejecutable (#256). Si
# el bundler se deja un sidecar, en una instalación limpia no hay daemon que
# arrancar y la ventana no tiene a quién pedirle un listado. Por eso se
# pregunta al gestor de quién es cada `/usr/bin/<b>`: que esté en el PATH no
# basta, tiene que haberlo puesto ESTE paquete. Y se pregunta dentro, donde
# está el gestor: el host no tiene por qué tener `rpm`.
docker run --rm -i \
  -v "$PAQUETE:/tmp/norte.$FORMATO:ro" \
  -e FORMATO="$FORMATO" \
  -e DEBIAN_FRONTEND=noninteractive \
  "$IMAGEN" bash -euo pipefail -s <<'DENTRO'
echo "--- instalando"
if [[ $FORMATO == deb ]]; then
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends xvfb ca-certificates >/dev/null
  dpkg -i /tmp/norte.deb 2>/dev/null || apt-get -f install -y -qq
  dueno() { dpkg -S "$1" >/dev/null 2>&1; }
else
  dnf install -y -q xorg-x11-server-Xvfb /tmp/norte.rpm >/dev/null
  dueno() { rpm -qf "$1" >/dev/null 2>&1; }
fi

for b in norte-gui norte ntc; do
  dueno "/usr/bin/$b" || { echo "el paquete no instala /usr/bin/$b" >&2; exit 1; }
done
echo "los tres binarios los pone el paquete"

# El CLI arranca: si faltara una biblioteca del sistema, esto ya no enlaza.
norte --version

echo "--- el listado inicial, por los binarios del paquete"
mkdir -p /tmp/casa && : >/tmp/casa/hola.txt
# `norte ls` levanta el daemon por su cuenta si no hay ninguno, que es el
# mismo camino que usa la ventana.
salida="$(norte ls /tmp/casa)"
grep -q 'hola.txt' <<<"$salida" || {
  echo "el listado no trajo el fichero:" >&2; echo "$salida" >&2; exit 1; }
echo "llega el listado"

echo "--- la ventana arranca bajo Xvfb"
# No se comprueba que PINTE —eso pide una persona o una captura—, sino que no
# se cae al arrancar: una dependencia de WebKitGTK que falte muere aquí, y es
# el fallo de instalación limpia más común.
export DISPLAY=:99
Xvfb :99 -screen 0 1280x800x24 &
sleep 2
norte-gui &
GUI=$!
sleep 8
if kill -0 "$GUI" 2>/dev/null; then
  echo "la ventana sigue viva a los 8 segundos"
  kill "$GUI" 2>/dev/null || true
else
  wait "$GUI" 2>/dev/null || true
  echo "la ventana MURIÓ al arrancar" >&2
  exit 1
fi
DENTRO

verde "humo del paquete: OK en $IMAGEN"
