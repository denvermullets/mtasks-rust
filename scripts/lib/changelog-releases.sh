# CHANGELOG.md -> Release manifest `releases` array and release body.
# Sourced by scripts/publish-release.sh; pure functions over files, so they can
# be run offline against a sample changelog. Plain POSIX awk (macOS has no gawk).
#
# A Release's section is `## <major.minor.patch> · <yyyy-mm-dd>` followed by its
# bullets, up to the next `## ` heading. Dated sections (`## 2026-11-12`) from
# before versioned sections are not Releases and are skipped.

# The text of one version's section, verbatim, without surrounding blank lines.
#   changelog_notes <changelog> <version>
changelog_notes() {
    awk -v want="$2" '
        /^## / {
            if (in_sec) exit
            in_sec = (index($0, "## " want " · ") == 1)
            next
        }
        in_sec {
            if ($0 == "" && n == 0) next      # leading blanks
            lines[n++] = $0
        }
        END {
            while (n > 0 && lines[n - 1] == "") n--   # trailing blanks
            for (i = 0; i < n; i++) print lines[i]
        }
    ' "$1"
}

# `releases` as a JSON array, newest first as in the changelog.
#   changelog_releases_json <changelog> <live_manifest|""> <new_version> <new_published_at>
# published_at: the new version gets <new_published_at>; an older version keeps
# the one from the live manifest, else its heading date at midnight UTC.
changelog_releases_json() {
    local changelog="$1" live="$2" new_version="$3" new_at="$4" known="" i=0 v at
    if [[ -n "$live" && -f "$live" ]]; then
        while v="$(plutil -extract "releases.$i.version" raw -o - "$live" 2>/dev/null)"; do
            at="$(plutil -extract "releases.$i.published_at" raw -o - "$live" 2>/dev/null || true)"
            [[ -n "$at" ]] && known+="$v	$at"$'\n'
            i=$((i + 1))
        done
    fi
    KNOWN="$known" awk -v new_version="$new_version" -v new_at="$new_at" '
        function esc(s) {
            gsub(/\\/, "\\\\", s)
            gsub(/"/, "\\\"", s)
            gsub(/\t/, "\\t", s)
            gsub(/\r/, "", s)
            return s
        }
        function flush(   i, text, at) {
            if (!in_sec) return
            while (n > 0 && body[n - 1] == "") n--
            text = ""
            for (i = 0; i < n; i++) text = text (i ? "\\n" : "") esc(body[i])
            at = (ver == new_version) ? new_at : ((ver in known) ? known[ver] : date "T00:00:00Z")
            out = out (count++ ? ",\n" : "") "  {\"version\": \"" ver "\", \"published_at\": \"" esc(at) "\", \"notes\": \"" text "\"}"
            in_sec = 0
            n = 0
        }
        BEGIN {
            m = split(ENVIRON["KNOWN"], rows, "\n")
            for (r = 1; r <= m; r++) {
                if (rows[r] == "") continue
                split(rows[r], kv, "\t")
                known[kv[1]] = kv[2]
            }
        }
        /^## / {
            flush()
            if (match($0, /^## [0-9]+\.[0-9]+\.[0-9]+ · /)) {
                head = substr($0, 4)
                ver = head; sub(/ · .*/, "", ver)
                date = head; sub(/^[^ ]+ · /, "", date)
                in_sec = 1
            }
            next
        }
        in_sec {
            if ($0 == "" && n == 0) next
            body[n++] = $0
        }
        END {
            flush()
            if (count) print "[\n" out "\n]"; else print "[]"
        }
    ' "$changelog"
}

# The GitHub release description: the version's notes, then the DMG link.
#   release_body <changelog> <version> <dmg_url>
release_body() {
    changelog_notes "$1" "$2"
    printf '\nDirect download: %s\n' "$3"
}
