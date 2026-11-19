#!/usr/bin/env bash
# Build, notarize and publish JAIT in one go.
#
#   mise exec -- ./scripts/release.sh [--skip-tests]
#
# Runs scripts/build-release.sh, then scripts/publish-release.sh on the export
# folder it just made. If the publish step fails, the build is kept and the
# exact retry command is printed, so you don't need to rebuild.
#
# Before the slow build it checks the things publish would refuse anyway (a dirty
# tree, a missing CHANGELOG section), so you find out in seconds, not minutes.
set -euo pipefail

BUILD_ARGS=()
for arg in "$@"; do
    case "$arg" in
        --skip-tests) BUILD_ARGS+=("$arg") ;;
        *)
            echo "usage: $0 [--skip-tests]" >&2
            exit 2
            ;;
    esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# ── Fast preflight ──

if [[ -n "$(git status --porcelain)" ]]; then
    echo "Working tree has uncommitted changes; publish would refuse this build. Commit first." >&2
    exit 1
fi

VERSION="$(awk -F'"' '/^\[/ { section = $0 } section == "[package]" && /^version *=/ { print $2; exit }' Cargo.toml)"
if ! grep -qF "## $VERSION · " CHANGELOG.md; then
    echo "CHANGELOG.md has no '## $VERSION · <date>' section. Run /changelog first." >&2
    exit 1
fi

# ── Build ──

LOG="$(mktemp)"
trap 'rm -f "$LOG"' EXIT

# tee so the build output still streams; pipefail keeps the build's exit status.
"$ROOT/scripts/build-release.sh" ${BUILD_ARGS[@]+"${BUILD_ARGS[@]}"} 2>&1 | tee "$LOG"

EXPORT_DIR="$(sed -n 's/^Done: //p' "$LOG" | tail -n 1)"
if [[ -z "$EXPORT_DIR" || ! -d "$EXPORT_DIR" ]]; then
    echo "Couldn't find the export folder in the build output." >&2
    exit 1
fi

# ── Publish ──

echo
echo "==> publishing $EXPORT_DIR"
if ! "$ROOT/scripts/publish-release.sh" "$EXPORT_DIR"; then
    echo >&2
    echo "Publish failed; the build is kept. Retry without rebuilding:" >&2
    echo "  mise exec -- ./scripts/publish-release.sh $EXPORT_DIR" >&2
    exit 1
fi
