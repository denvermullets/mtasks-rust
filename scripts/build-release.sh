#!/usr/bin/env bash
# Test, build, sign and notarize JAIT for macOS into its own export folder.
#
#   ./scripts/build-release.sh [--skip-tests] [--no-notarize]
#
#   --skip-tests   Skip `cargo test`.
#   --no-notarize  Sign only, with no upload to Apple. The folder is suffixed
#                  -unnotarized because Gatekeeper rejects this build elsewhere,
#                  and it gets no update zip, so it can't be published.
#
# A notarized build leaves, in exports/<yymmdd>/jait-<version>/: the stapled
# JAIT.app, JAIT-<version>.dmg (first-install path) and JAIT-<version>-arm64.zip
# (the update zip the publish script uploads) and build-info.json (version,
# commit, dirty flag).
#
# SIGN_IDENTITY overrides the signing identity (default: the only Developer ID
# Application identity in the keychain). NOTARY_PROFILE overrides the notarytool
# keychain profile (default: BrokenColony). See docs/desktop-run-notes.md.
set -euo pipefail

RUN_TESTS=1
NOTARIZE=1
for arg in "$@"; do
    case "$arg" in
        --skip-tests) RUN_TESTS=0 ;;
        --no-notarize) NOTARIZE=0 ;;
        *)
            echo "usage: $0 [--skip-tests] [--no-notarize]" >&2
            exit 2
            ;;
    esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NOTARY_PROFILE="${NOTARY_PROFILE:-BrokenColony}"
APP_NAME="JAIT"
# DEFAULT_API_BASE_URL for release builds in src/config.rs.
PROD_API_URL="https://justanotherissuetracker.com"
BUNDLED_APP="$ROOT/target/release/bundle/osx/$APP_NAME.app"
cd "$ROOT"

# ── Preflight: fail before the slow steps ──

# The version lives in two places in Cargo.toml; a mismatch ships a wrong label.
PACKAGE_VERSION="$(awk -F'"' '/^\[/ { section = $0 } section == "[package]" && /^version *=/ { print $2; exit }' Cargo.toml)"
BUNDLE_VERSION="$(awk -F'"' '/^\[/ { section = $0 } section == "[package.metadata.bundle]" && /^version *=/ { print $2; exit }' Cargo.toml)"
if [[ -z "$PACKAGE_VERSION" || "$PACKAGE_VERSION" != "$BUNDLE_VERSION" ]]; then
    echo "Cargo.toml versions differ: [package] '$PACKAGE_VERSION', [package.metadata.bundle] '$BUNDLE_VERSION'" >&2
    exit 1
fi

if ! cargo bundle --version >/dev/null 2>&1; then
    echo "cargo-bundle not found. Run mise install, and run this script as: mise exec -- $0" >&2
    exit 1
fi

if [[ -z "${SIGN_IDENTITY:-}" ]]; then
    IDENTITIES="$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application:.*\)"$/\1/p')"
    COUNT="$(printf '%s' "$IDENTITIES" | grep -c . || true)"
    if [[ "$COUNT" != "1" ]]; then
        echo "Expected one Developer ID Application identity, found $COUNT. Set SIGN_IDENTITY." >&2
        exit 1
    fi
    SIGN_IDENTITY="$IDENTITIES"
fi

if [[ "$NOTARIZE" == "1" ]]; then
    if ! xcrun notarytool history --keychain-profile "$NOTARY_PROFILE" >/dev/null 2>&1; then
        echo "notarytool can't use keychain profile '$NOTARY_PROFILE'. Store one with:" >&2
        echo "  xcrun notarytool store-credentials $NOTARY_PROFILE" >&2
        echo "or pick another with NOTARY_PROFILE=<name>." >&2
        exit 1
    fi
fi

# Recorded in build-info.json; read before the build writes anything into the tree.
COMMIT="$(git rev-parse HEAD)"
DIRTY=false
if [[ -n "$(git status --porcelain)" ]]; then
    DIRTY=true
    echo "warning: working tree has uncommitted changes; this build won't match a commit." >&2
fi

# ── Export folder: never reuse one, old builds are the backups ──

BASE="$(date +%y%m%d)/jait-$PACKAGE_VERSION"
[[ "$NOTARIZE" == "1" ]] || BASE="$BASE-unnotarized"
BUILD="$BASE"
n=2
while [[ -e "exports/$BUILD" ]]; do
    BUILD="$BASE-$n"
    n=$((n + 1))
done
OUT="$ROOT/exports/$BUILD"
APP="$OUT/$APP_NAME.app"
DMG="$OUT/$APP_NAME-$PACKAGE_VERSION.dmg"
ZIP="$OUT/$APP_NAME-$PACKAGE_VERSION-arm64.zip"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ── Steps ──

# Submit a file, wait for Apple's verdict, then staple the ticket to it.
notarize_and_staple() {
    local submission="$1" log id
    echo "==> notarizing $(basename "$submission") (usually a few minutes)"
    log="$(xcrun notarytool submit "$submission" --keychain-profile "$NOTARY_PROFILE" --wait)" || true
    echo "$log"
    if [[ "$log" != *"status: Accepted"* ]]; then
        id="$(echo "$log" | awk '/^ *id:/ { print $2; exit }')"
        echo "Notarization was not accepted. Get the reason with:" >&2
        echo "  xcrun notarytool log ${id:-<submission-id>} --keychain-profile $NOTARY_PROFILE" >&2
        exit 1
    fi
    xcrun stapler staple "$submission"
}

if [[ "$RUN_TESTS" == "1" ]]; then
    echo "==> testing"
    cargo test
fi

echo "==> building"
rm -rf "$BUNDLED_APP"
cargo bundle --release

# The bundle's own version (what the app reports, and what the updater compares)
# must match Cargo.toml.
PLIST_VERSION="$(plutil -extract CFBundleShortVersionString raw -o - "$BUNDLED_APP/Contents/Info.plist")"
if [[ "$PLIST_VERSION" != "$PACKAGE_VERSION" ]]; then
    echo "Info.plist version '$PLIST_VERSION' differs from Cargo.toml '$PACKAGE_VERSION'." >&2
    exit 1
fi

# Never ship a build that talks to the dev server.
BINARY="$BUNDLED_APP/Contents/MacOS/JAIT"
if LC_ALL=C grep -qaF "localhost:3005" "$BINARY"; then
    echo "Release binary contains localhost:3005; refusing to ship it." >&2
    exit 1
fi
if ! LC_ALL=C grep -qaF "$PROD_API_URL" "$BINARY"; then
    echo "Release binary doesn't contain $PROD_API_URL; check src/config.rs." >&2
    exit 1
fi

mkdir -p "$OUT"
echo "==> building into exports/$BUILD"
ditto "$BUNDLED_APP" "$APP"
# Extended attributes make codesign fail with "resource fork ... not allowed".
xattr -cr "$APP"

echo "==> signing app as $SIGN_IDENTITY"
codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$APP"
codesign --verify --strict --verbose=2 "$APP"

echo "==> creating DMG"
STAGE="$TMP/dmg"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/$APP_NAME.app"
ln -s /Applications "$STAGE/Applications"
hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG"

if [[ "$NOTARIZE" == "1" ]]; then
    # Notarizing the DMG covers the app inside. Only the DMG gets a stapled ticket,
    # so Gatekeeper looks the app's ticket up online the first time it opens.
    notarize_and_staple "$DMG"

    echo "==> checking Gatekeeper"
    spctl -a -t open --context context:primary-signature -vv "$DMG"
    xcrun stapler validate "$DMG"

    # The update zip holds the app on its own, so the app needs its own stapled
    # ticket (the DMG's covers only the DMG). The ticket exists now that Apple
    # accepted the DMG, which contained this same signed app. A failure here
    # fails the build (set -e), so nothing publishable is left half-made.
    echo "==> stapling and validating $APP_NAME.app"
    xcrun stapler staple "$APP"
    xcrun stapler validate "$APP"
    spctl -a -t exec -vv "$APP"

    echo "==> zipping the update"
    ditto -c -k --keepParent "$APP" "$ZIP"

    # The publish script reads this to refuse dirty builds and builds whose
    # commit isn't HEAD. Only notarized builds get one.
    printf '{\n  "version": "%s",\n  "commit": "%s",\n  "dirty": %s\n}\n' \
        "$PACKAGE_VERSION" "$COMMIT" "$DIRTY" >"$OUT/build-info.json"
fi

echo
du -sh "$APP" "$DMG"
[[ "$NOTARIZE" != "1" ]] || du -sh "$ZIP"
echo "Done: exports/$BUILD"
if [[ "$NOTARIZE" != "1" ]]; then
    echo "Signed but NOT notarized: Gatekeeper rejects this build on other Macs."
fi
