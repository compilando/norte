#!/usr/bin/env bash
# Installs the official plugins the shots show into ada's home. Installing is
# not consenting (docs/plugins.md): the approval is given afterwards, through
# the extension manager, by scenes/approve.scene.
#
# usage: plugins.sh <home> <bin-dir> <repo>
set -euo pipefail

home=${1:?home} bin=${2:?bin dir} repo=${3:?repo}
here=$(cd "$(dirname "$0")" && pwd)

# id-less name → where its wasm is built
declare -A wasm=(
	[image-ansi]=plugins/image-ansi/target/wasm32-wasip2/release/image_ansi_preview.wasm
	[markdown]=plugins/markdown/target/wasm32-wasip2/release/markdown_preview.wasm
	[file-icons]=plugins/file-icons/target/wasm32-wasip2/release/file_icons.wasm
	[size-bar]=plugins/size-bar/target/wasm32-wasip2/release/size_bar.wasm
	[media-info]=plugins/media-info/target/wasm32-wasip2/release/media_info.wasm
	[image-thumb]=plugins/image-thumb/target/wasm32-wasip2/release/image_thumb.wasm
	[previewer-syntect]=crates/norte-plugin-host/examples-wasm/previewer-syntect/target/wasm32-wasip2/release/previewer_syntect.wasm
)

for name in "${!wasm[@]}"; do
	src=$repo/plugins/$name
	[ -d "$src" ] || src=$repo/crates/norte-plugin-host/examples-wasm/$name
	if [ ! -f "$repo/${wasm[$name]}" ]; then
		echo "plugins.sh: $name is not built (just plugins)" >&2
		exit 1
	fi
	stage=$home/.stage/$name
	mkdir -p "$stage"
	cp "$src/plugin.toml" "$stage/"
	[ -f "$src/help.md" ] && cp "$src/help.md" "$stage/"
	cp "$repo/${wasm[$name]}" "$stage/plugin.wasm"
	"$here/sandbox.sh" "$home" "$bin" norte plugin install "/home/ada/.stage/$name" --force >/dev/null
done
rm -rf "$home/.stage"
"$here/sandbox.sh" "$home" "$bin" norte plugin list
