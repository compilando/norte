# Fuentes empaquetadas

JetBrains Mono e Inter llegan por npm (`@fontsource/*`, OFL-1.1) y no viven
aquí. Este directorio tiene lo que no está en un registro:

## `symbols-nerd-mono-subset.woff2`

Symbols Nerd Font Mono (MIT, `LICENSE-nerd-fonts.txt`), recortada a los
glifos que el plugin `file-icons` usa en sus estilos `nerd` y `seti`. La
fuente entera son 2,6 MB; el recorte, 16 KB (73 glifos). Los codepoints son
TODOS los de uso privado que aparecen en `plugins/file-icons/src/icons.rs`:
**añadir un glifo al plugin es regenerar este fichero**, o la ventana pinta
una caja en esa fila.

Cómo se regenera (fonttools y woff2 en la máquina; el script saca la lista
del propio `icons.rs`, así no hay una segunda lista que se desincronice):

```sh
curl -sL -o nf.zip https://github.com/ryanoasis/nerd-fonts/releases/latest/download/NerdFontsSymbolsOnly.zip
unzip -o nf.zip SymbolsNerdFontMono-Regular.ttf LICENSE
CPS=$(python3 -c '
import re
src = open("plugins/file-icons/src/icons.rs").read()
cps = {int(h, 16) for h in re.findall(r"\\u\{([0-9a-fA-F]{4,5})\}", src)}
print(",".join("U+%04X" % c for c in sorted(cps) if 0xE000 <= c <= 0xF8FF))')
pyftsubset SymbolsNerdFontMono-Regular.ttf --no-hinting \
  --unicodes="$CPS" --output-file=subset.ttf
woff2_compress subset.ttf   # → subset.woff2
```

Comprueba que no falta ninguno antes de copiarlo:
`python3 -c 'from fontTools.ttLib import TTFont; print(len(TTFont("subset.woff2").getBestCmap()))'`
debe dar el mismo número que la lista.

La `@font-face` está en `style.css`, acotada con `unicode-range` a los tres
rangos que usa el plugin (`nf-seti` U+E5FA–E6B7, `nf-dev` U+E700–E7C5 y
`nf-fa` U+F000–F2E0): fuera de ellos la fuente ni se consulta.
