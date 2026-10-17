#!/bin/sh
# One-time Cloudflare setup for the Motion Library (run on the Mac, wrangler logged in):
#   1. creates the private R2 bucket `motion-library` (never public),
#   2. deploys the Worker in front of it (worker.js),
#   3. makes two random tokens and stores them as Worker secrets (READ_TOKEN, PUBLISH_TOKEN),
#   4. writes this Mac's EffectCraft config (motion-library.json: URL + publish token) and a
#      read-only config for rented machines next to it (motion-library.read.json).
#
#   CLOUDFLARE_ACCOUNT_ID=<id> scripts/fork/motion-library/setup.sh
#
# The account id is required on purpose: the login can see more than one account.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
: "${CLOUDFLARE_ACCOUNT_ID:?set CLOUDFLARE_ACCOUNT_ID to the account that should hold the library}"
export CLOUDFLARE_ACCOUNT_ID
config_dir="${EFFECTCRAFT_CONFIG_DIR:-$HOME/Library/Application Support/EffectCraft}"
bucket=motion-library

cd "$here"
if ! wrangler r2 bucket list 2>/dev/null | grep -q "name: *$bucket\$"; then
  wrangler r2 bucket create "$bucket"
fi
url=$(wrangler deploy 2>&1 | tee /dev/stderr | grep -Eo 'https://[a-zA-Z0-9.-]+\.workers\.dev' | head -1)
[ -n "$url" ] || { echo "could not read the Worker URL from wrangler deploy" >&2; exit 1; }

read_token=$(openssl rand -hex 32)
publish_token=$(openssl rand -hex 32)
printf %s "$read_token" | wrangler secret put READ_TOKEN >/dev/null
printf %s "$publish_token" | wrangler secret put PUBLISH_TOKEN >/dev/null

mkdir -p "$config_dir"
umask 077
printf '{\n  "remote": "%s",\n  "token": "%s"\n}\n' "$url" "$publish_token" > "$config_dir/motion-library.json"
printf '{\n  "remote": "%s",\n  "token": "%s"\n}\n' "$url" "$read_token" > "$config_dir/motion-library.read.json"
echo "Worker: $url"
echo "This Mac (publish): $config_dir/motion-library.json"
echo "Rented machines (read-only): copy $config_dir/motion-library.read.json to their settings folder as motion-library.json"
