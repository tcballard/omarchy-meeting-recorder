#!/bin/bash
# Packs the release build into the tarball for this machine's architecture, in dist/:
#   cargo build --release --locked && packaging/release-tarball.sh
set -euo pipefail
cd "$(dirname "$0")/.."

name=omarchy-meeting-recorder
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
arch=$(uname -m)
dir=dist/$name-$version
tarball=$name-$version-$arch-linux.tar.gz

rm -rf dist
mkdir -p "$dir/data" "$dir/plugin" "$dir/examples/actions"
install -m755 "target/release/$name" "$dir/$name"
strip "$dir/$name"
cp README.md LICENSE "$dir/"
sed "s/@ARCH@/$arch/" packaging/INSTALL.md > "$dir/INSTALL.md"
cp "data/$name.desktop" "data/$name.xml" "$dir/data/"
cp plugin/manifest.json plugin/Widget.qml "$dir/plugin/"
for action in examples/actions/*; do
  if [[ -f "$action" ]]; then cp "$action" "$dir/examples/actions/"; fi
done
mkdir -p "$dir/docs"
cp docs/live-assistant.md "$dir/docs/"
cp examples/actions/local-assistant/THIRD_PARTY_NOTICES.md "$dir/"

tar -C dist -czf "dist/$tarball" "$name-$version"
(cd dist && sha256sum "$tarball" | tee "$tarball.sha256")
