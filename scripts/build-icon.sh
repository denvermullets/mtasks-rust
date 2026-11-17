#!/usr/bin/env bash
# Regenerate assets/AppIcon.icns from assets/appicon-default.svg.
#
#   ./scripts/build-icon.sh
#
# The SVG is full-bleed square art. macOS app icons sit on Apple's 1024 grid:
# an 824x824 rounded rect (corner radius 185) centered with 100px of clear
# margin, so the art is scaled into that shape before rasterizing. Without the
# margin and rounding the Dock shows a hard-edged square larger than its
# neighbours.
#
# Needs rsvg-convert (librsvg) and iconutil (ships with macOS). Commit the
# resulting .icns; `cargo bundle` reads it via `icon` in
# [package.metadata.bundle] in Cargo.toml.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/assets/appicon-default.svg"
OUT="$ROOT/assets/AppIcon.icns"

command -v rsvg-convert >/dev/null || { echo "rsvg-convert not found (librsvg)" >&2; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Wrap the art in the macOS icon shape. The source viewBox is 0 0 256 256.
ART="$(awk 'opened { sub("</svg>", ""); print; next } /<svg[^>]*>/ { opened = 1 }' "$SRC")"
[[ -n "$ART" ]] || { echo "no drawing found inside <svg> in $SRC" >&2; exit 1; }
cat >"$WORK/icon.svg" <<EOF
<svg width="1024" height="1024" viewBox="0 0 1024 1024" xmlns="http://www.w3.org/2000/svg">
  <defs><clipPath id="shape"><rect x="100" y="100" width="824" height="824" rx="185" ry="185"/></clipPath></defs>
  <g clip-path="url(#shape)">
    <g transform="translate(100 100) scale($(echo "scale=6; 824/256" | bc))">
$ART
    </g>
  </g>
</svg>
EOF

ICONSET="$WORK/AppIcon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    rsvg-convert -w "$size" -h "$size" "$WORK/icon.svg" -o "$ICONSET/icon_${size}x${size}.png"
    rsvg-convert -w $((size * 2)) -h $((size * 2)) "$WORK/icon.svg" -o "$ICONSET/icon_${size}x${size}@2x.png"
done

iconutil -c icns "$ICONSET" -o "$OUT"
echo "wrote $OUT"
