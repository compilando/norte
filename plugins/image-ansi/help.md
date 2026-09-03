+++
id = "org.norte.image-ansi"
title = "Image preview"
+++
Shows a PNG, JPEG or GIF (first frame) in the viewer as coloured cells.
Each cell is a `▀` half block carrying two pixels, one in the foreground
colour and one in the background, so a picture takes half as many rows as
it has pixels. The picture is shrunk to the viewer's width, never enlarged;
transparent pixels are blended over black.

Files over 1 MiB are not rendered: the host hands the previewer at most that
much, and a truncated picture decodes to garbage. The viewer says so and the
raw view is one key away, as always.
