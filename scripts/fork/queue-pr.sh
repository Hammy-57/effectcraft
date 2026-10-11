#!/usr/bin/env bash
# Fork-only (eliasx45/effectcraft): queue landed fork work as an upstream PR branch.
#
#   scripts/fork/queue-pr.sh <topic-slug> --title "fix(viewer): …" [--depends upstream/<other>]
#       [--body-file f.md] [--update] [--no-check] [--no-push] [--allow-leaks] <commit-or-range>...
#
# Builds `upstream/<topic-slug>` from upstream/main (or from origin/upstream/<other> with
# --depends) in a temporary worktree, cherry-picks the given non-merge commits with -x, drops
# changes to the paths in scripts/fork/upstream-exclude.txt, checks rustfmt + `cargo check` on the
# affected crates, pushes the branch to origin and writes queue/<topic-slug>.md on origin's
# `pr-queue` branch. The Mac opener turns queue files into PRs on storytold; this script never
# opens a PR. --update adds commits to an existing queued branch (no force push). --no-push does
# everything locally and prints the queue file instead (for trying it out). Added lines that name
# fork-only things (docs/fork, owner-assets, profile.local…) stop it unless --allow-leaks.
# Docs: docs/fork/agent-builds.md, "Sharing with upstream (queue at landing)".
set -euo pipefail

die() { echo "queue-pr: $*" >&2; exit 1; }
warn() { echo "queue-pr: warning: $*" >&2; }
note() { echo "queue-pr: $*" >&2; }
retry() { # flaky GitHub: 4 tries, 10 s apart
    local i
    for i in 1 2 3 4; do
        if "$@"; then return 0; fi
        [ "$i" = 4 ] || { warn "'$*' failed (try $i of 4), retrying in 10 s"; sleep 10; }
    done
    return 1
}
usage() { sed -n '4,6p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 2; }

slug="" title="" depends="" body_file="" update=0 check=1 push=1 allow_leaks=0
specs=()
while [ $# -gt 0 ]; do
    case "$1" in
        --title) [ $# -ge 2 ] || usage; title=$2; shift 2 ;;
        --depends) [ $# -ge 2 ] || usage; depends=$2; shift 2 ;;
        --body-file) [ $# -ge 2 ] || usage; body_file=$2; shift 2 ;;
        --update) update=1; shift ;;
        --no-check) check=0; shift ;;
        --no-push) push=0; shift ;;
        --allow-leaks) allow_leaks=1; shift ;;
        -h|--help) usage ;;
        -*) die "unknown option $1" ;;
        *) if [ -z "$slug" ]; then slug=$1; else specs+=("$1"); fi; shift ;;
    esac
done
[ -n "$slug" ] && [ ${#specs[@]} -gt 0 ] || usage
case "$slug" in *[!a-z0-9._-]*|-*|"") die "topic slug '$slug': use lowercase letters, digits, '-', '.', '_'" ;; esac
case "$depends" in ""|upstream/*) ;; *) die "--depends takes a queued branch name, upstream/<topic>" ;; esac
[ -z "$body_file" ] || [ -f "$body_file" ] || die "no body file $body_file"
[ -z "$body_file" ] || body_file=$(cd "$(dirname "$body_file")" && pwd)/$(basename "$body_file")
branch="upstream/$slug"

root=$(git rev-parse --show-toplevel)
# Every worktree shares one object store; the main checkout holds the shared target dir.
main_root=$(cd "$(git rev-parse --git-common-dir)/.." && pwd)
exclude_file="$root/scripts/fork/upstream-exclude.txt"
[ -f "$exclude_file" ] || die "missing $exclude_file"

globs=() hunks=()
while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
        ""|"#"*) ;;
        hunk:*) hunks+=("$(echo "${line#hunk:}" | sed 's/^ *//')") ;;
        *) globs+=("$(echo "$line" | sed 's/ *$//')") ;;
    esac
done < "$exclude_file"
# shellcheck disable=SC2254 # the patterns are globs on purpose
excluded() { local g; for g in ${globs[@]+"${globs[@]}"}; do case "$1" in $g) return 0 ;; esac; done; return 1; }

tmp=$(mktemp -d "${TMPDIR:-/tmp}/queue-pr.XXXXXX")
wt="$tmp/wt"
cleanup() {
    if [ -d "$wt" ]; then git -C "$root" worktree remove --force "$wt" >/dev/null 2>&1 || true; fi
    git -C "$root" worktree prune >/dev/null 2>&1 || true
    rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

note "fetching upstream and origin"
retry git -C "$root" fetch --quiet upstream main || die "could not fetch upstream"
retry git -C "$root" fetch --quiet --prune origin || die "could not fetch origin"

# Resolve commits (oldest first, no merges, no duplicates).
commits=()
seen=" "
for spec in "${specs[@]}"; do
    case "$spec" in
        *..*) list=$(git -C "$root" rev-list --reverse --no-merges "$spec") || die "bad range $spec" ;;
        *)
            c=$(git -C "$root" rev-parse --verify --quiet "$spec^{commit}") || die "unknown commit $spec"
            [ "$(git -C "$root" rev-list --no-walk --count --min-parents=2 "$c")" = 0 ] \
                || die "$spec is a merge commit: pass the commits it brought in (a range) instead"
            list=$c ;;
    esac
    for c in $list; do
        case "$seen" in *" $c "*) ;; *) commits+=("$c"); seen="$seen$c " ;; esac
    done
done
[ ${#commits[@]} -gt 0 ] || die "no non-merge commits in: ${specs[*]}"

exists=0
retry_ls() { git -C "$root" ls-remote --heads origin "$branch" > "$tmp/ls" ; }
retry retry_ls || die "could not reach origin"
[ -s "$tmp/ls" ] && exists=1
if [ $exists = 1 ] && [ $update = 0 ]; then
    die "$branch already exists on origin: pass --update to add commits to it, or pick another slug"
fi
[ $exists = 1 ] || [ $update = 0 ] || die "--update: $branch is not on origin yet"

if [ $update = 1 ]; then
    base="origin/$branch"
elif [ -n "$depends" ]; then
    git -C "$root" rev-parse --verify --quiet "refs/remotes/origin/$depends" >/dev/null || die "--depends $depends: no such branch on origin (queue the prerequisite first)"
    base="origin/$depends"
else
    base="upstream/main"
fi
base_sha=$(git -C "$root" rev-parse "$base^{commit}")
git -C "$root" worktree add --quiet --detach "$wt" "$base_sha"
note "building $branch on $base ($(git -C "$root" rev-parse --short "$base_sha"))"

hunk_hits=""
picked=()
for c in "${commits[@]}"; do
    short=$(git -C "$root" rev-parse --short "$c")
    subject=$(git -C "$root" log -1 --format=%s "$c")
    if git -C "$wt" cat-file -e "$c" 2>/dev/null && git -C "$wt" merge-base --is-ancestor "$c" HEAD; then
        note "skip $short (already on the branch)"; continue
    fi
    # Apply without committing; conflicts in fork-only paths are fine, they're dropped below.
    git -C "$wt" cherry-pick --no-commit "$c" >/dev/null 2>&1 || true
    for f in $(git -C "$wt" diff --name-only HEAD; git -C "$wt" diff --name-only --cached HEAD;
               git -C "$wt" ls-files --unmerged | cut -f2; git -C "$wt" ls-files --others --exclude-standard); do
        excluded "$f" || continue
        if git -C "$wt" cat-file -e "HEAD:$f" 2>/dev/null; then
            git -C "$wt" checkout --quiet HEAD -- "$f"
        else
            git -C "$wt" rm --quiet --cached --force --ignore-unmatch -- "$f" >/dev/null
            rm -f "$wt/$f"
        fi
    done
    conflicted=$(git -C "$wt" diff --name-only --diff-filter=U)
    if [ -n "$conflicted" ]; then
        echo "queue-pr: commit $short does not apply on $base:" >&2
        echo "  $subject" >&2
        echo "  conflicting files:" >&2
        echo "$conflicted" | sed 's/^/    /' >&2
        echo "  It probably builds on fork work that isn't upstream yet. Queue that prerequisite" >&2
        echo "  first and pass --depends upstream/<its-topic>, or include its commits in this PR." >&2
        echo "  Nothing was pushed or queued." >&2
        exit 1
    fi
    for h in ${hunks[@]+"${hunks[@]}"}; do
        hp=${h%% *}
        if ! git -C "$wt" diff --cached --quiet HEAD -- "$hp"; then
            hunk_hits="$hunk_hits  $hp: $(echo "${h#"$hp"}" | sed 's/^ *//')  (from $short)
"
        fi
    done
    if git -C "$wt" diff --cached --quiet HEAD; then
        git -C "$wt" cherry-pick --quit >/dev/null 2>&1 || true
        git -C "$wt" reset --quiet --hard HEAD
        note "skip $short (only fork-only paths, or already upstream): $subject"
        continue
    fi
    git -C "$root" log -1 --format=%B "$c" | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}' > "$tmp/msg"
    printf '\n(cherry picked from commit %s)\n' "$(git -C "$root" rev-parse "$c")" >> "$tmp/msg"
    GIT_AUTHOR_NAME=$(git -C "$root" log -1 --format=%an "$c") \
    GIT_AUTHOR_EMAIL=$(git -C "$root" log -1 --format=%ae "$c") \
    GIT_AUTHOR_DATE=$(git -C "$root" log -1 --format=%aD "$c") \
        git -C "$wt" commit --quiet --no-verify -F "$tmp/msg"
    git -C "$wt" cherry-pick --quit >/dev/null 2>&1 || true
    picked+=("$c")
    note "picked $short $subject"
done
[ ${#picked[@]} -gt 0 ] || die "nothing left to queue: every commit was fork-only or already on the branch"

if [ -n "$hunk_hits" ]; then
    warn "these files carry fork-only hunks; check the branch diff and remove them by hand:"
    printf '%s' "$hunk_hits" >&2
fi
# Fork-only content that slipped into shared files.
leaks=$(git -C "$wt" diff "$base_sha" HEAD | grep -E '^\+' | grep -v '^+++' \
    | grep -n -E 'docs/fork|scripts/fork|owner-assets|local_assets|profile\.local|eliasx45|effectcraft-lane-' || true)
if [ -n "$leaks" ]; then
    warn "added lines mention fork-only things:"
    echo "$leaks" | head -20 | sed 's/^/  /' >&2
    [ $allow_leaks = 1 ] || die "not queued: land the fork-only part as its own commit and queue the rest, or pass --allow-leaks if these lines are fine upstream"
fi

changed=$(git -C "$wt" diff --name-only --diff-filter=ACMR "$base_sha" HEAD)
if [ $check = 1 ]; then
    rs=$(echo "$changed" | grep -E '\.rs$' || true)
    if [ -n "$rs" ]; then
        note "rustfmt --check"
        # shellcheck disable=SC2086
        (cd "$wt" && rustfmt --edition 2024 --check $rs) || die "rustfmt: the files above need formatting (nothing pushed)"
    fi
    pkgs=() all_targets=""
    for f in $changed; do
        case "$f" in fuzz/*) continue ;; */tests/*|*/benches/*|*/examples/*) all_targets="--all-targets" ;; esac
        d=$(dirname "$f")
        while [ "$d" != "." ] && [ ! -f "$wt/$d/Cargo.toml" ]; do d=$(dirname "$d"); done
        [ "$d" != "." ] || continue
        p=$(sed -n '/^\[package\]/,/^\[/s/^name *= *"\(.*\)"/\1/p' "$wt/$d/Cargo.toml" | head -1)
        [ -n "$p" ] || continue
        case " ${pkgs[*]-} " in *" $p "*) ;; *) pkgs+=("$p") ;; esac
    done
    if [ ${#pkgs[@]} -gt 0 ]; then
        args=()
        for p in "${pkgs[@]}"; do args+=(-p "$p"); done
        note "cargo check ${pkgs[*]} $all_targets (target/agent-upstream)"
        # shellcheck disable=SC2086
        (cd "$wt" && CARGO_BUILD_JOBS=4 CARGO_TARGET_DIR="$main_root/target/agent-upstream" \
            nice -n 10 cargo check --quiet "${args[@]}" $all_targets) || die "cargo check failed on $branch (nothing pushed)"
    fi
fi

# Queue file.
queue_path="queue/$slug.md"
qpr="refs/remotes/origin/pr-queue"
old_queue=""
if git -C "$root" cat-file -e "$qpr:$queue_path" 2>/dev/null; then old_queue=$(git -C "$root" show "$qpr:$queue_path"); fi
if [ -z "$title" ] && [ -n "$old_queue" ]; then title=$(echo "$old_queue" | sed -n '1s/^title: //p'); fi
[ -n "$title" ] || die "--title is required"
if [ -z "$depends" ] && [ -n "$old_queue" ]; then depends=$(echo "$old_queue" | sed -n '3s/^depends: //p'); fi
[ -n "$depends" ] || depends=none

new_body() {
    local c
    echo "## What"
    for c in "${picked[@]}"; do echo "- $(git -C "$root" log -1 --format=%s "$c")"; done
    echo
    echo "## Why"
    for c in "${picked[@]}"; do
        b=$(git -C "$root" log -1 --format=%b "$c" | grep -v -E '^(Co-Authored-By|Signed-off-by|Generated with|🤖)' \
            | sed -e :a -e '/^\n*$/{$d;N;ba' -e '}')
        [ -z "$b" ] || { echo "$b"; echo; }
    done
    echo "See the commit messages; each commit is cherry-picked from eliasx45/effectcraft (\`-x\` trailer)."
    echo
    echo "## How tested"
    notes=$(for c in "${picked[@]}"; do git -C "$root" log -1 --format=%B "$c"; done \
        | grep -i -E 'test|cargo |verified|checked' | grep -v -i '^co-authored' | sed 's/^[-* ]*/- /' || true)
    echo "TODO before opening: confirm what ran. The commits' own test notes:"
    if [ -n "$notes" ]; then echo "$notes"; else echo "- (none in the commit messages)"; fi
    tests=$(git -C "$wt" diff --name-only "$base_sha" HEAD | grep -E '(^|/)tests?(/|_)|/tests\.rs$' || true)
    [ -z "$tests" ] || echo "- tests added or changed: $(echo "$tests" | tr '\n' ' ')"
    [ $check = 0 ] || echo "- queue-pr.sh: rustfmt --check and cargo check on the affected crates passed on this branch"
}
{
    echo "title: $title"
    echo "branch: $branch"
    echo "depends: $depends"
    echo
    if [ -n "$body_file" ]; then
        cat "$body_file"
    elif [ -n "$old_queue" ]; then
        echo "$old_queue" | sed '1,4d'
        echo
        echo "## Added later"
        for c in "${picked[@]}"; do echo "- $(git -C "$root" log -1 --format=%s "$c")"; done
    else
        new_body
    fi
} > "$tmp/queue.md"

if [ $push = 0 ]; then
    note "--no-push: branch built at $(git -C "$wt" rev-parse --short HEAD); not pushed. Queue file would be:"
    cat "$tmp/queue.md"
    exit 0
fi

note "pushing $branch"
retry git -C "$wt" push --quiet origin "HEAD:refs/heads/$branch" || die "could not push $branch"

# Commit the queue file onto origin's pr-queue with plumbing (a private index; no checkout touched).
# If someone else pushed in between, fetch and re-apply on the new tip (no rebase, no force).
readme="$tmp/README.md"
cat > "$readme" <<'EOF'
# Upstream PR queue (eliasx45/effectcraft)

Fork-only branch. Each `queue/<topic>.md` is a finished change waiting to become a pull request on
storytold/effectcraft: line 1 `title:`, line 2 `branch:` (on this fork), line 3 `depends:`, a blank
line, then the PR body. Written by `scripts/fork/queue-pr.sh`; the Mac opener opens the PRs.
See docs/fork/agent-builds.md on `main`.
EOF
publish_queue() {
    local idx="$tmp/index" parent="" blob tree commit
    rm -f "$idx"
    git -C "$root" fetch --quiet origin "+refs/heads/pr-queue:$qpr" 2>/dev/null || true
    if git -C "$root" rev-parse --verify --quiet "$qpr" >/dev/null; then
        parent=$(git -C "$root" rev-parse "$qpr")
        GIT_INDEX_FILE=$idx git -C "$root" read-tree "$parent"
    else
        GIT_INDEX_FILE=$idx git -C "$root" read-tree --empty
    fi
    if ! GIT_INDEX_FILE=$idx git -C "$root" ls-files --error-unmatch README.md >/dev/null 2>&1; then
        blob=$(git -C "$root" hash-object -w "$readme")
        GIT_INDEX_FILE=$idx git -C "$root" update-index --add --cacheinfo "100644,$blob,README.md"
    fi
    blob=$(git -C "$root" hash-object -w "$tmp/queue.md")
    GIT_INDEX_FILE=$idx git -C "$root" update-index --add --cacheinfo "100644,$blob,$queue_path"
    tree=$(GIT_INDEX_FILE=$idx git -C "$root" write-tree)
    if [ -n "$parent" ]; then
        commit=$(git -C "$root" commit-tree "$tree" -p "$parent" -m "queue: $slug")
    else
        commit=$(git -C "$root" commit-tree "$tree" -m "queue: $slug")
    fi
    git -C "$root" push --quiet origin "$commit:refs/heads/pr-queue"
}
retry publish_queue || die "pushed $branch, but could not write $queue_path on pr-queue: rerun with --update"
retry git -C "$root" fetch --quiet origin "+refs/heads/pr-queue:$qpr" || true
note "queued: $branch and $queue_path on origin/pr-queue"
