#!/bin/bash
# Renders assets/icon/postden.svg, the app icon's source, into the files packaging ships:
# assets/macos/AppIcon.icns and assets/linux/icons/<size>.png. Run it after changing the SVG and
# commit the results. Needs macOS (iconutil) and resvg (from mise).
set -euo pipefail
cd "$(dirname "$0")/.."

[[ "$(uname -s)" == Darwin ]] || { echo 'Building the .icns needs macOS.' >&2; exit 1; }

source=assets/icon/postden.svg
render() { resvg --width "$1" --height "$1" "$source" "$2"; }

iconset="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  render "$size" "$iconset/icon_${size}x${size}.png"
  render "$((size * 2))" "$iconset/icon_${size}x${size}@2x.png"
done
iconutil --convert icns --output assets/macos/AppIcon.icns "$iconset"
rm -rf "$(dirname "$iconset")"

mkdir -p assets/linux/icons
for size in 32 48 64 128 256 512; do
  render "$size" "assets/linux/icons/$size.png"
done
echo 'Ready: assets/macos/AppIcon.icns, assets/linux/icons/'
