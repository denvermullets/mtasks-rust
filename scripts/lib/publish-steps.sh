# Release steps for scripts/publish-release.sh, sourced after its guards pass.
# Kept in functions so they can be exercised offline with `mise`, `security` and
# `git` shims on PATH. Expects the caller's strict mode and its `refuse`
# function. Nothing here runs on its own.

# The Keychain item holding the fine-grained token scoped to jait-releases
# (Contents read/write). Created by hand when the repo is set up.
KEYCHAIN_SERVICE="${KEYCHAIN_SERVICE:-jait-releases-upload-token}"

# Runs the mise-pinned gh. The token comes from the Keychain into the
# environment of this one process (never an argument, so it isn't in `ps`), and
# GH_CONFIG_DIR is an empty folder so no account-wide gh login is ever used.
# The Keychain is read once per script run.
gh_run() {
    if [[ -z "${GH_TOKEN_VALUE:-}" ]]; then
        GH_TOKEN_VALUE="$(security find-generic-password -s "$KEYCHAIN_SERVICE" -w 2>/dev/null)" \
            || refuse "no Keychain item '$KEYCHAIN_SERVICE' (the jait-releases upload token)."
        [[ -n "$GH_TOKEN_VALUE" ]] || refuse "the Keychain item '$KEYCHAIN_SERVICE' is empty."
    fi
    mkdir -p "$TMP/gh-config"
    GH_TOKEN="$GH_TOKEN_VALUE" GH_CONFIG_DIR="$TMP/gh-config" GH_PROMPT_DISABLED=1 \
        mise exec -- gh "$@"
}

sha256_of() { shasum -a 256 "$1" | awk '{print $1}'; }
size_of() { stat -f %z "$1"; }

# Writes the Release manifest (schema 1) to stdout.
#   render_manifest <version> <published_at> <zip_url> <zip_sha256> <zip_size> <dmg_url> [releases_json]
# releases_json is a ready JSON array; without it the key is left out and the
# app shows "Release notes unavailable".
render_manifest() {
    local releases=""
    if [[ -n "${7:-}" ]]; then
        releases=$(printf ',\n  "releases": %s' "$7")
    fi
    printf '{\n  "schema": 1,\n  "version": "%s",\n  "published_at": "%s",\n  "zip_url": "%s",\n  "zip_sha256": "%s",\n  "zip_size": %s,\n  "dmg_url": "%s"%s\n}\n' \
        "$1" "$2" "$3" "$4" "$5" "$6" "$releases"
}

# Deletes a leftover draft for the tag. A published release is never touched:
# that version is taken, so we stop.
delete_leftover_draft() {
    local tag="$1" state
    state="$(gh_run release list --repo "$RELEASES_REPO" --limit 200 \
        --json tagName,isDraft --jq ".[] | select(.tagName == \"$tag\") | .isDraft")" \
        || refuse "couldn't list the releases of $RELEASES_REPO."
    case "$state" in
        "") ;;
        true)
            echo "Deleting leftover draft $tag from a failed run."
            gh_run release delete "$tag" --repo "$RELEASES_REPO" --yes >/dev/null \
                || refuse "couldn't delete the leftover draft $tag."
            ;;
        *) refuse "$RELEASES_REPO already has a published release $tag. Bump the version." ;;
    esac
}

# Draft, upload, re-download and compare, go live. Needs VERSION, TAG, ZIP,
# EXPORT_DIR, TMP, RELEASES_REPO, PUBLISHED_AT (RFC 3339), optional RELEASES_JSON.
# $1 is the release body file.
publish_release() {
    local notes_file="$1"
    local dmg="$EXPORT_DIR/$APP_NAME-$VERSION.dmg"
    [[ -f "$dmg" ]] || refuse "$(basename "$dmg") is missing from the export folder."

    local zip_name dmg_name base up
    zip_name="$(basename "$ZIP")"
    dmg_name="$(basename "$dmg")"
    base="https://github.com/$RELEASES_REPO/releases/download/$TAG"
    up="$TMP/upload"
    rm -rf "$up"
    mkdir -p "$up"
    cp "$ZIP" "$up/$zip_name"
    cp "$dmg" "$up/$dmg_name"
    cp "$dmg" "$up/$APP_NAME.dmg" # same bytes: the stable first-install link

    local zip_sha zip_size manifest_sha
    zip_sha="$(sha256_of "$up/$zip_name")"
    zip_size="$(size_of "$up/$zip_name")"
    render_manifest "$VERSION" "$PUBLISHED_AT" "$base/$zip_name" "$zip_sha" \
        "$zip_size" "$base/$dmg_name" "${RELEASES_JSON:-}" >"$up/latest.json"
    manifest_sha="$(sha256_of "$up/latest.json")"

    delete_leftover_draft "$TAG"
    echo "Creating draft release $TAG."
    gh_run release create "$TAG" --repo "$RELEASES_REPO" --draft \
        --title "$APP_NAME $VERSION" --notes-file "$notes_file" >/dev/null \
        || refuse "couldn't create the draft release $TAG."

    echo "Uploading assets."
    gh_run release upload "$TAG" --repo "$RELEASES_REPO" \
        "$up/$zip_name" "$up/$dmg_name" "$up/$APP_NAME.dmg" "$up/latest.json" \
        || refuse "an upload failed. The draft $TAG is left in place; rerun to recreate it."

    echo "Re-downloading the manifest and zip to verify them."
    local down="$TMP/verify"
    rm -rf "$down"
    mkdir -p "$down"
    gh_run release download "$TAG" --repo "$RELEASES_REPO" --dir "$down" \
        --pattern latest.json --pattern "$zip_name" \
        || refuse "couldn't re-download the uploaded assets from the draft."
    [[ "$(sha256_of "$down/latest.json")" == "$manifest_sha" ]] \
        || refuse "the uploaded latest.json doesn't match what we built (sha256). The draft is not live."
    [[ "$(sha256_of "$down/$zip_name")" == "$zip_sha" ]] \
        || refuse "the uploaded $zip_name doesn't match the local zip (sha256). The draft is not live."

    echo "Going live."
    gh_run release edit "$TAG" --repo "$RELEASES_REPO" --draft=false --latest >/dev/null \
        || refuse "couldn't publish $TAG. It is still a verified draft; rerun to recreate it."
}
