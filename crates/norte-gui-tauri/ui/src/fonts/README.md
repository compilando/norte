# Fuentes empaquetadas

JetBrains Mono e Inter llegan por npm (`@fontsource/*`, OFL-1.1) y no viven
aquí. Este directorio tiene lo que no está en un registro:

## `symbols-nerd-mono-subset.woff2`

Symbols Nerd Font Mono (MIT, `LICENSE-nerd-fonts.txt`), recortada a los
glifos que el plugin `file-icons` usa en su estilo `nerd`. La fuente entera
son 2,6 MB; el recorte, 3,8 KB. Los codepoints son los de
`plugins/file-icons/src/icons.rs` (`Style::Nerd`): **añadir un glifo al
plugin es regenerar este fichero**, o la ventana pinta una caja en esa fila.

Cómo se regenera (fonttools y woff2 en la máquina):

```sh
curl -sL -o nf.zip https://github.com/ryanoasis/nerd-fonts/releases/latest/download/NerdFontsSymbolsOnly.zip
unzip -o nf.zip SymbolsNerdFontMono-Regular.ttf LICENSE
pyftsubset SymbolsNerdFontMono-Regular.ttf --no-hinting \
  --unicodes="U+F07B,U+F0C1,U+E7A8,U+F121,U+F120,U+F0F6,U+F0CE,U+F1C4,U+F02D,U+F1C5,U+F001,U+F008,U+F1C6,U+F013,U+F0AD,U+E702,U+E7B0,U+F24E" \
  --output-file=subset.ttf
woff2_compress subset.ttf   # → subset.woff2
```

La `@font-face` está en `style.css`, acotada con `unicode-range` a los dos
rangos que Nerd Fonts v3 no movió (`nf-dev` U+E700–E7C5 y `nf-fa`
U+F000–F2E0): fuera de ellos la fuente ni se consulta.
