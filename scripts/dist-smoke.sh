#!/usr/bin/env bash
# Desempaqueta los artefactos recién construidos y arranca lo que llevan dentro.
#
# Una release que no arranca es el único fallo de empaquetado que el usuario
# descubre ANTES que nosotros. Se comprueba sobre los archivos, no sobre
# `target/release/`: lo que se publica son los archivos, y el binario de dentro
# puede llevar otro perfil, otro strip u otro nombre.
#
# dist produce UN archivo POR PAQUETE, no uno con todo: `norte-tui` trae `ntc`
# y `norte-cli` trae `norte`, cada uno con su instalador. Por eso se comprueba
# pareja a pareja — un archivo que existe pero trae el binario del otro pasaría
# desapercibido con una búsqueda global.
set -euo pipefail

fallos=0

comprobar() {
    local paquete="$1" binario="$2" archivo ruta
    archivo=$(find target/distrib -name "${paquete}-*.tar.xz" -print -quit)
    if [ -z "$archivo" ]; then
        echo "no hay artefacto de ${paquete}: corre \`just dist\` antes"
        fallos=$((fallos + 1))
        return
    fi

    local tmp
    tmp=$(mktemp -d)
    # shellcheck disable=SC2064  # se expande AHORA a propósito: un trap por llamada.
    trap "rm -rf '$tmp'" RETURN
    tar -xf "$archivo" -C "$tmp"

    ruta=$(find "$tmp" -name "$binario" -type f -print -quit)
    if [ -z "$ruta" ]; then
        echo "FALTA en $(basename "$archivo"): $binario"
        fallos=$((fallos + 1))
        return
    fi
    if ! "$ruta" --version >/dev/null 2>&1; then
        echo "NO ARRANCA desde $(basename "$archivo"): $binario"
        fallos=$((fallos + 1))
        return
    fi
    echo "ok: $binario $("$ruta" --version)"
}

comprobar norte-tui ntc
comprobar norte-cli norte

exit "$fallos"
