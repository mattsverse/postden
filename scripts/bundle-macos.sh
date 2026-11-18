#!/bin/bash
# Builds Postden.app (desktop) into dist/.
set -euo pipefail
cd "$(dirname "$0")/.."

[[ "$(uname -s)" == Darwin ]] || { echo 'Bundling the macOS app needs macOS.' >&2; exit 1; }

# By default this builds for the current Mac. `--universal` builds for both Apple
# Silicon and Intel Macs, as releases ship. `--dev` builds an unoptimized bundle for the current
# Mac into the host target directory, so it reuses what `cargo test` already compiled (CI does this).
dev=
case "${1:-}" in
  '') targets=("$(rustc -vV | sed -n 's/^host: //p')") ;;
  --universal) targets=(aarch64-apple-darwin x86_64-apple-darwin) ;;
  --dev) dev=1 ;;
  *) echo "Usage: $0 [--universal | --dev]" >&2; exit 2 ;;
esac

# Keep artifact paths deterministic even with a user-configured target directory.
desktop=()
if [[ -n "$dev" ]]; then
  # No deployment target here: setting it would rerun build scripts that `cargo test` already ran.
  cargo build --locked --target-dir "$PWD/target" -p postden
  desktop+=(target/debug/postden)
else
  export MACOSX_DEPLOYMENT_TARGET=13.0
  for target in "${targets[@]}"; do
    cargo build --release --locked --target "$target" --target-dir "$PWD/target" -p postden
    desktop+=("target/$target/release/postden")
  done
fi

bundle="$PWD/dist/Postden.app"
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
lipo -create "${desktop[@]}" -output "$bundle/Contents/MacOS/postden"
# A release build passes RELEASE_TAG, which may be a nightly's.
python3 scripts/release.py plist ${RELEASE_TAG:+--tag "$RELEASE_TAG"} --output "$bundle/Contents/Info.plist"
cp LICENSE assets/macos/AppIcon.icns "$bundle/Contents/Resources/"
codesign --force --sign - "$bundle"
echo "Ready: $bundle"
