+++
id = "org.norte.size-bar"
title = "Size bar"
+++
Adds a column with each file's size drawn as a small bar, `█` for the filled
part and `░` for the rest, so the big files stand out without reading a
number. Directories get no bar: their size is not what is on disk.

It asks the size of each entry under the directory being listed and reads
nothing else.

Three settings shape the bar. `scale` is `log` by default, which spreads
sizes from bytes to gigabytes over the bar; `linear` is proportional, so
everything but the biggest file is a stub. `width` is the bar's length in
cells, 3 to 8. `relative-to` says what fills the bar: `page`, the biggest
file on the visible page, or `absolute`, one gibibyte, so a bar means the
same in every directory.
