+++
id = "org.norte.image-thumb"
title = "Image thumbnails"
+++
Gives the window's viewer a small picture of an image file when the viewer
has none of its own: a photo too big for the viewer's cap, or a format the
window cannot decode (TIFF). It reads the bytes the host hands it — never
the disk — decodes PNG, JPEG, GIF, WebP, BMP and TIFF, and answers a raster
no larger than the edge the viewer asked for.

Two settings. `format` picks the encoding of the thumbnail: `jpeg` (the
default, a tenth the size for a photo) or `png` (exact, with transparency).
`quality` is the JPEG quality, 30 to 95, 80 by default.

The terminal ignores this extension: it has no pixels to put a picture on.
