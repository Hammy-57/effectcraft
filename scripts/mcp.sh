#!/bin/sh
# Starts the EffectCraft MCP server for `.mcp.json` without making the client wait for a compile.
#
# The binary lives in its own target dir (target/mcp) so it never waits on another build's lock.
# If the sources are newer than the binary, the existing binary starts at once and a release build
# runs in the background; reconnect the MCP server (Claude Code: /mcp) to pick it up. Only the very
# first start has to compile in the foreground.
#
# Usage: scripts/mcp.sh [effectcraft-cli mcp arguments], e.g. `scripts/mcp.sh --demo`.
set -u

root=$(cd "$(dirname "$0")/.." && pwd)
target="$root/target/mcp"
bin="$target/release/effectcraft-cli"
lock="$target/build.lock"
log="$target/build.log"

mkdir -p "$target"
if command -v sccache >/dev/null 2>&1; then
    export RUSTC_WRAPPER="${RUSTC_WRAPPER:-sccache}"
fi

build() {
    CARGO_TARGET_DIR="$target" cargo build --quiet --release -p effectcraft-cli --manifest-path "$root/Cargo.toml"
}

if [ ! -x "$bin" ]; then
    echo "effectcraft mcp: first start, compiling effectcraft-cli (if the client times out, wait for this and reconnect)" >&2
    build >"$log" 2>&1 || { cat "$log" >&2; exit 1; }
elif [ -n "$(find "$root/crates" "$root/Cargo.toml" "$root/Cargo.lock" -newer "$bin" \( -name '*.rs' -o -name '*.toml' -o -name '*.lock' -o -name '*.json' -o -name '*.wgsl' \) -print 2>/dev/null | head -n 1)" ]; then
    # Stale: serve the current binary now, rebuild once in the background (mkdir is an atomic lock).
    if mkdir "$lock" 2>/dev/null; then
        echo "effectcraft mcp: sources changed, rebuilding in the background; reconnect to use the new build" >&2
        (build >"$log" 2>&1; rmdir "$lock") </dev/null >/dev/null 2>&1 &
    else
        echo "effectcraft mcp: a background rebuild is already running (stale lock? remove $lock)" >&2
    fi
fi

exec "$bin" mcp "$@"
