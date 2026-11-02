#!/usr/bin/env bash
# Test, build, sign and notarize JAIT for macOS into its own export folder.
#
#   ./scripts/build-release.sh [--skip-tests] [--no-notarize]
#
#   --skip-tests   Skip `cargo test`.
#   --no-notarize  Sign only, with no upload to Apple. The folder is suffixed
#                  -unnotarized because Gatekeeper rejects this build elsewhere.
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
    echo "cargo-bundle not found. Install it: cargo install cargo-bundle --locked" >&2
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

if [[ -n "$(git status --porcelain)" ]]; then
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

# Never ship a build that talks to the dev server.
BINARY="$BUNDLED_APP/Contents/MacOS/mtasks-rusty"
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
fi

echo
du -sh "$APP" "$DMG"
echo "Done: exports/$BUILD"
if [[ "$NOTARIZE" != "1" ]]; then
    echo "Signed but NOT notarized: Gatekeeper rejects this build on other Macs."
fi
