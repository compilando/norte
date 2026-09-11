+++
id = "org.norte.age"
title = "Age"
+++
Adds a column saying how long ago each entry changed: a glyph for the
bucket it falls in — today, this week, this month, older — and a short
figure such as `3h`, `2d`, `5mo`. The freshest rows catch the eye first,
which is what a date column is usually for.

It asks the modification time of each entry under the directory being
listed and reads nothing else.

Three settings shape it. `thresholds` are the bucket edges in days,
ascending, `1,7,30` by default; anything past the last edge is the last
bucket. `glyphs` is one glyph per bucket, freshest first (`●◐○·` by
default); with fewer glyphs than buckets the last one repeats. `format` is
`both` by default, or `glyph` or `text` for one of them alone.
