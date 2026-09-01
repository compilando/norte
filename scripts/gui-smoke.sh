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
#
# `imagen` es la base limpia (por defecto `debian:trixie`). Exige que
# `just gui-package` se haya corrido antes: construirlo aquí dentro obligaría
# a meter el toolchain entero en el contenedor, que es lo contrario de una
# máquina limpia.
set -euo pipefail

IMAGEN="${1:-debian:trixie}"
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE="$RAIZ/target/release/bundle"

rojo() { printf '\033[31m%s\033[0m\n' "$*" >&2; }
verde() { printf '\033[32m%s\033[0m\n' "$*"; }

# --- 1. El paquete existe -----------------------------------------------
DEB="$(find "$BUNDLE/deb" -name '*.deb' -print -quit 2>/dev/null || true)"
if [[ -z "$DEB" ]]; then
  rojo "no hay .deb en $BUNDLE/deb — corre \`just gui-package\` primero"
  exit 1
fi
echo "paquete: $(basename "$DEB")"

# --- 2. Lleva los TRES binarios -----------------------------------------
#
# La ventana busca a `norte` como HERMANO de su propio ejecutable (#256). Si
# el bundler se deja un sidecar, en una instalación limpia no hay daemon que
# arrancar y la ventana no tiene a quién pedirle un listado.
contenido="$(dpkg-deb -c "$DEB")"
falta=0
for b in norte-gui norte ntc; do
  # Sin barra inicial: `dpkg-deb -c` lista `usr/bin/norte-gui`, no
  # `./usr/bin/…`. El `$` final es lo que impide que `norte` case con
  # `norte-gui`.
  if ! grep -qE "(^| )usr/bin/$b\$" <<<"$contenido"; then
    rojo "el .deb no lleva /usr/bin/$b"
    falta=1
  fi
done
[[ $falta -eq 0 ]] || exit 1
verde "los tres binarios están en el paquete"

# --- 3. Se instala y funciona en una máquina que no conoce este árbol ----
#
# `apt-get -f install` resuelve las dependencias que el paquete DECLARA: si
# el bundler se dejó una, aquí falla, que es justo lo que no se veía. El
# contenedor no comparte nada con el host salvo el .deb.
docker run --rm -i \
  -v "$DEB:/tmp/norte.deb:ro" \
  -e DEBIAN_FRONTEND=noninteractive \
  "$IMAGEN" bash -euo pipefail -s <<'DENTRO'
apt-get update -qq
apt-get install -y -qq --no-install-recommends xvfb ca-certificates >/dev/null

echo "--- instalando"
dpkg -i /tmp/norte.deb 2>/dev/null || apt-get -f install -y -qq

for b in norte-gui norte ntc; do
  command -v "$b" >/dev/null || { echo "FALTA $b tras instalar" >&2; exit 1; }
done
echo "los tres binarios están en el PATH"

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
