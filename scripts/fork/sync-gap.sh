#!/usr/bin/env bash
# Fork-only (eliasx45/effectcraft): how far the fork's shareable work is from upstream.
#
#   scripts/fork/sync-gap.sh [--json] [--no-fetch] [--fail-if-growing <file>]
#
# Shareable = non-merge commits in upstream/main..origin/main that touch a path outside
# scripts/fork/upstream-exclude.txt. Pending = shareable, minus commits already queued (cherry-picked
# with -x onto a branch named in origin/pr-queue's queue/*.md) and minus commits upstream already has
# (`git cherry`). Prints one summary line and the 10 task ids with the most pending lines.
# --fail-if-growing <file>: exit 1 when pending commits grew since the count stored in <file>; the
# file is rewritten with today's count either way (the nightly uses this).
set -euo pipefail

json=0 fetch=1 state=""
while [ $# -gt 0 ]; do
    case "$1" in
        --json) json=1; shift ;;
        --no-fetch) fetch=0; shift ;;
        --fail-if-growing) [ $# -ge 2 ] || { echo "sync-gap: --fail-if-growing needs a file" >&2; exit 2; }; state=$2; shift 2 ;;
        -h|--help) sed -n '4,4p' "$0" | sed 's/^# *//' >&2; exit 2 ;;
        *) echo "sync-gap: unknown argument $1" >&2; exit 2 ;;
    esac
done

root=$(git rev-parse --show-toplevel)
cd "$root"
if [ $fetch = 1 ]; then
    for i in 1 2 3 4; do
        git fetch --quiet upstream main && git fetch --quiet --prune origin && break
        [ "$i" = 4 ] && { echo "sync-gap: fetch failed, using local refs" >&2; break; }
        sleep 10
    done
fi
up=upstream/main fork=origin/main

excludes=()
while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ""|"#"*|hunk:*) ;; *) excludes+=(":(exclude)$(echo "$line" | sed 's/ *$//')") ;; esac
done < scripts/fork/upstream-exclude.txt

tmp=$(mktemp -d "${TMPDIR:-/tmp}/sync-gap.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# Commits upstream already has (same patch): "-" lines of git cherry.
git cherry "$up" "$fork" | sed -n 's/^- //p' | sort > "$tmp/upstream"
# Commits already queued: -x trailers on the branches the queue files name.
: > "$tmp/queued"
if git rev-parse --verify --quiet refs/remotes/origin/pr-queue >/dev/null; then
    for f in $(git ls-tree --name-only origin/pr-queue queue/ 2>/dev/null); do
        b=$(git show "origin/pr-queue:$f" | sed -n '2s/^branch: //p')
        git rev-parse --verify --quiet "refs/remotes/origin/$b" >/dev/null || continue
        git log --format=%B "$up..origin/$b" | sed -n 's/^(cherry picked from commit \([0-9a-f]*\))$/\1/p' >> "$tmp/queued"
    done
fi
sort -u -o "$tmp/queued" "$tmp/queued"

# One pass: per commit, its sha, subject and line count on shareable paths.
git log --no-merges --format='@%H%x09%s' --numstat "$up..$fork" -- . ${excludes[@]+"${excludes[@]}"} \
| awk -F'\t' '
    /^@/ { if (sha != "") print sha "\t" n "\t" subj; sha = substr($1, 2); subj = $2; n = 0; next }
    NF >= 3 { if ($1 != "-") n += $1 + $2 }
    END { if (sha != "") print sha "\t" n "\t" subj }' > "$tmp/shareable"

awk -F'\t' -v A="$tmp/upstream" -v B="$tmp/queued" '
    BEGIN { while ((getline l < A) > 0) up[l] = 1; while ((getline l < B) > 0) q[l] = 1 }
    { if ($1 in up) print "U\t" $0; else if ($1 in q) print "Q\t" $0; else print "P\t" $0 }' \
    "$tmp/shareable" > "$tmp/classified"

count() { awk -F'\t' -v k="$1" '$1 == k' "$tmp/classified" | wc -l | tr -d ' '; }
lines() { awk -F'\t' -v k="$1" '$1 == k { s += $3 } END { print s + 0 }' "$tmp/classified"; }
shareable=$(wc -l < "$tmp/shareable" | tr -d ' ')
pending=$(count P) pending_lines=$(lines P) queued=$(count Q) upstreamed=$(count U)
diff_lines=$(git diff --numstat "$up...$fork" -- . ${excludes[@]+"${excludes[@]}"} | awk '$1 != "-" { s += $1 + $2 } END { print s + 0 }')

# Top 10 task ids (subject prefix before ":") by pending lines.
awk -F'\t' '$1 == "P" { id = $4; sub(/:.*/, "", id); if (length(id) > 24) id = "(no id)";
        n[id]++; l[id] += $3; if (!(id in s)) s[id] = substr($2, 1, 10) " " $4 }
    END { for (k in n) print l[k] "\t" n[k] "\t" k "\t" s[k] }' "$tmp/classified" \
    | sort -t "$(printf '\t')" -k1,1nr | head -10 > "$tmp/top"

previous=""
[ -z "$state" ] || [ ! -f "$state" ] || previous=$(head -1 "$state" | tr -dc '0-9')

if [ $json = 1 ]; then
    printf '{"pending_commits":%s,"pending_lines":%s,"queued_commits":%s,"upstream_commits":%s,"shareable_commits":%s,"shareable_diff_lines":%s,"previous_pending":%s,"top":[' \
        "$pending" "$pending_lines" "$queued" "$upstreamed" "$shareable" "$diff_lines" "${previous:-null}"
    awk -F'\t' '{ gsub(/\\/, "\\\\", $4); gsub(/"/, "\\\"", $4); gsub(/\\/, "\\\\", $3); gsub(/"/, "\\\"", $3)
        printf "%s{\"task\":\"%s\",\"commits\":%s,\"lines\":%s,\"newest\":\"%s\"}", (NR > 1 ? "," : ""), $3, $2, $1, $4 }' "$tmp/top"
    printf ']}\n'
else
    echo "sync gap: $pending commits / $pending_lines lines not queued or upstream (of $shareable shareable; $queued queued, $upstreamed already upstream); shareable diff vs upstream/main: $diff_lines lines"
    if [ -s "$tmp/top" ]; then
        echo "top un-queued task ids (lines, commits, newest commit):"
        awk -F'\t' '{ printf "  %-10s %6d lines %4d commits  %s\n", $3, $1, $2, substr($4, 1, 110) }' "$tmp/top"
    fi
fi

if [ -n "$state" ]; then
    echo "$pending" > "$state"
    if [ -n "$previous" ] && [ "$pending" -gt "$previous" ]; then
        echo "sync-gap: growing: $previous -> $pending pending commits" >&2
        exit 1
    fi
fi
