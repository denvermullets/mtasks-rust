#!/usr/bin/env bash
# Publish a finished, notarized export folder as a JAIT Release.
#
#   ./scripts/publish-release.sh exports/<yymmdd>/jait-<version>
#
# The folder comes from scripts/build-release.sh, so a failed publish can be
# retried without rebuilding. This script only reads: it checks the build and
# refuses to go on if anything is off. There is deliberately no --force; a bad
# Release is fixed by bumping the version, never by overwriting one.
#
# After the guards: a draft release is created, the zip, DMG, JAIT.dmg and
# latest.json are uploaded, the manifest and zip are re-downloaded and checked
# against their sha256, and only then does the release go live as latest. gh is
# the mise-pinned one, with a Keychain token (see scripts/lib/publish-steps.sh).
#
# Guards, each exits non-zero with its own message:
#   - the folder has build-info.json and the notarized zip (an unnotarized build
#     makes no zip)
#   - the build is clean and its commit is HEAD
#   - the version has a section in CHANGELOG.md
#   - no v<version> source tag exists, locally or on origin
#   - the zip's app is stapled, passes Gatekeeper, has bundle id com.jait.desktop,
#     our Team ID, and the version build-info.json claims
#   - the live Release manifest doesn't already have this version or a newer one
#
# TEAM_ID overrides the Team ID to expect (default: read from the only Developer
# ID Application identity in the keychain). See docs/desktop-run-notes.md.
set -euo pipefail

BUNDLE_ID="com.jait.desktop"
APP_NAME="JAIT"
RELEASES_REPO="denvermullets/jait-releases"
MANIFEST_URL="https://github.com/$RELEASES_REPO/releases/latest/download/latest.json"

usage() {
    echo "usage: $0 <export-folder>" >&2
    exit 2
}

# One refusal path: say why, exit non-zero.
refuse() {
    echo "Refusing to publish: $*" >&2
    exit 1
}

[[ $# -eq 1 && "$1" != -* ]] || usage
EXPORT_DIR="$1"
[[ -d "$EXPORT_DIR" ]] || refuse "'$EXPORT_DIR' is not a folder."
EXPORT_DIR="$(cd "$EXPORT_DIR" && pwd)"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Value of a top-level key in a JSON or plist file; empty if missing.
json_get() {
    plutil -extract "$2" raw -o - "$1" 2>/dev/null || true
}

# ── Guards that only need local files ──

INFO="$EXPORT_DIR/build-info.json"
[[ -f "$INFO" ]] || refuse "$INFO is missing. Only notarized builds from scripts/build-release.sh write one."

VERSION="$(json_get "$INFO" version)"
COMMIT="$(json_get "$INFO" commit)"
DIRTY="$(json_get "$INFO" dirty)"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || refuse "build-info.json has no valid version ('$VERSION')."
[[ -n "$COMMIT" ]] || refuse "build-info.json has no commit."

ZIP="$EXPORT_DIR/$APP_NAME-$VERSION-arm64.zip"
[[ -f "$ZIP" ]] || refuse "$(basename "$ZIP") is missing. An unnotarized build makes no zip; rebuild without --no-notarize."

if [[ "$DIRTY" != "false" ]]; then
    refuse "this build was made from a dirty working tree (dirty: '$DIRTY'). Commit, then rebuild."
fi

HEAD_COMMIT="$(git rev-parse HEAD)"
if [[ "$COMMIT" != "$HEAD_COMMIT" ]]; then
    refuse "the build is from commit ${COMMIT:0:12} but HEAD is ${HEAD_COMMIT:0:12}. Check out the build's commit or rebuild."
fi

if ! grep -qF "## $VERSION · " CHANGELOG.md; then
    refuse "CHANGELOG.md has no '## $VERSION · <date>' section. Run /changelog."
fi

TAG="v$VERSION"
if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
    refuse "the source tag $TAG already exists locally, so $VERSION is already published. Bump the version."
fi

# ── Guards on the bundle inside the zip ──

EXTRACT="$TMP/extract"
mkdir -p "$EXTRACT"
ditto -x -k "$ZIP" "$EXTRACT" 2>/dev/null || refuse "$(basename "$ZIP") isn't a readable zip."
APP="$EXTRACT/$APP_NAME.app"
[[ -d "$APP" ]] || refuse "$(basename "$ZIP") doesn't contain $APP_NAME.app at its top level."

xcrun stapler validate "$APP" >/dev/null 2>&1 || refuse "the app in the zip has no valid stapled notarization ticket."
spctl -a -t exec "$APP" >/dev/null 2>&1 || refuse "Gatekeeper rejects the app in the zip (spctl -a -t exec)."

PLIST="$APP/Contents/Info.plist"
APP_ID="$(json_get "$PLIST" CFBundleIdentifier)"
[[ "$APP_ID" == "$BUNDLE_ID" ]] || refuse "bundle id is '$APP_ID', expected $BUNDLE_ID."

APP_VERSION="$(json_get "$PLIST" CFBundleShortVersionString)"
[[ "$APP_VERSION" == "$VERSION" ]] || refuse "the app reports version '$APP_VERSION' but build-info.json says $VERSION."

if [[ -z "${TEAM_ID:-}" ]]; then
    IDENTITIES="$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application:.*\)"$/\1/p')"
    [[ "$(printf '%s' "$IDENTITIES" | grep -c . || true)" == "1" ]] || refuse "expected one Developer ID Application identity in the keychain. Set TEAM_ID."
    TEAM_ID="$(printf '%s' "$IDENTITIES" | sed -n 's/.*(\([A-Z0-9]*\))$/\1/p')"
fi
[[ -n "$TEAM_ID" ]] || refuse "couldn't work out the Team ID. Set TEAM_ID."
# The same requirement the app checks on every Update it stages.
REQUIREMENT="anchor apple generic and identifier \"$BUNDLE_ID\" and certificate leaf[subject.OU] = \"$TEAM_ID\""
codesign --verify --deep --strict -R="$REQUIREMENT" "$APP" >/dev/null 2>&1 \
    || refuse "the app's code signature fails the check ($REQUIREMENT)."

# ── Guards that touch the network (read-only) ──

if ! REMOTE_TAGS="$(git ls-remote --tags origin "refs/tags/$TAG")"; then
    refuse "couldn't ask origin whether $TAG exists."
fi
[[ -z "$REMOTE_TAGS" ]] || refuse "the source tag $TAG already exists on origin, so $VERSION is already published. Bump the version."

# The live manifest: 404 means nothing has been published yet, which is fine.
# Any other failure means we can't tell, so we stop.
LIVE="$TMP/latest.json"
STATUS="$(curl -sSL -o "$LIVE" -w '%{http_code}' "$MANIFEST_URL")" || refuse "couldn't reach $MANIFEST_URL."
case "$STATUS" in
    200)
        LIVE_VERSION="$(json_get "$LIVE" version)"
        [[ "$LIVE_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || refuse "the live manifest has no valid version ('$LIVE_VERSION')."
        # sort -V puts the larger version last; ours must be strictly larger.
        NEWEST="$(printf '%s\n%s\n' "$LIVE_VERSION" "$VERSION" | sort -V | tail -n 1)"
        if [[ "$LIVE_VERSION" == "$VERSION" || "$NEWEST" != "$VERSION" ]]; then
            refuse "the live manifest is at $LIVE_VERSION, which is not older than $VERSION. Bump the version."
        fi
        ;;
    404) echo "No live manifest yet; treating this as the first Release." ;;
    *) refuse "$MANIFEST_URL answered HTTP $STATUS." ;;
esac

echo "All guards passed for $APP_NAME $VERSION (commit ${COMMIT:0:12})."

# ── Release ──

source "$ROOT/scripts/lib/publish-steps.sh"

NOTES_FILE="$TMP/release-body.md"
printf 'Direct download: https://github.com/%s/releases/download/%s/%s-%s.dmg\n' \
    "$RELEASES_REPO" "$TAG" "$APP_NAME" "$VERSION" >"$NOTES_FILE"

publish_release "$NOTES_FILE"
echo "Published $APP_NAME $VERSION."

# Later steps (release notes from the changelog, source tag) are added by their own tickets.
