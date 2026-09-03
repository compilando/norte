+++
id = "org.norte.media-info"
title = "Media info"
+++
Two columns you can add to a listing: **Dims** (`1920×1080`) for PNG, JPEG,
GIF and WebP images, and **Length** (`3:41`) for WAV, MP3 and FLAC audio.

Both come from the file's header: the plugin reads at most 64 KiB of a file,
and only of files whose extension says they are an image or an audio track.
Everything else is left empty, as is a file whose header does not parse —
an empty cell means "cannot tell", never a guess.

It reads under the directory you are looking at, through a token the host
hands it for that listing. It never learns the path.
