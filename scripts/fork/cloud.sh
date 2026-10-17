#!/usr/bin/env bash
# One command to use a vast.ai GPU machine Elias rented as the cloud preview (plan/cloud-render.md CR10).
#
#   scripts/fork/cloud.sh connect root@<ip> -p <port> [--project F.ecproj] [--idle MIN]
#   scripts/fork/cloud.sh status
#   scripts/fork/cloud.sh disconnect
#
# connect: check the machine, make sure its effectcraft-cli is this Mac's main (built there from
# source, nothing uploaded), mirror the project's media + fonts (CR7: <= 100 MB from the Mac, bigger
# files from Google Drive links), start the frame server (nohup), open the SSH tunnel, set
# "Preview On: Cloud" in the running Mac app. Running it twice is safe.
# It never rents, stops or destroys an instance: that stays manual in the vast.ai console.
# Docs: docs/fork/cloud.md
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CONFIG_DIR="${EFFECTCRAFT_CONFIG_DIR:-$HOME/Library/Application Support/EffectCraft}"
STATE="$CONFIG_DIR/cloud-connection.env"      # ip, port, remote ports: no secrets
TOKEN_FILE="$CONFIG_DIR/cloud-frame-token"    # the app reads it (CR5)
SSH_KEY="${EC_CLOUD_KEY:-$HOME/.ssh/id_ed25519}"
SOCK="/tmp/ec-cloud-$(id -u).sock"            # ssh ControlMaster = the tunnel
REMOTE_DIR=/workspace/effectcraft
SKILL_SCRIPTS="$HOME/.claude/skills/effectcraft-remote-gpu/scripts"
BANNER='vast.ai|Have fun|AI agents'

say() { printf '%s\n' "$*"; }
die() { printf '❌ %s\n' "$*" >&2; exit 1; }
quiet_banner() { grep -v -E "$BANNER" || true; }

usage() {
  cat <<'EOF'
cloud.sh connect root@IP -p PORT [options]   check, build, mirror, start the server, open the tunnel, switch the app to Cloud
  --project F.ecproj      mirror this project (default: the project open in the app)
  --frames-port N         frame server port on the machine (default 9878)
  --local-port N          port on this Mac for the tunnel (default: same as --frames-port)
  --app-port N            the Mac app's control port (default $EFFECTCRAFT_CONTROL_PORT or 9877)
  --idle MIN              stop the serve process (not the instance) after MIN minutes with no app connected (off)
  --no-mirror             skip the media/fonts mirror
cloud.sh status          tunnel, server commit, GPU, free VRAM/disk, pending uploads
cloud.sh disconnect      app back to This Mac, close the tunnel, stop the server process (the instance stays: destroy it yourself)
EOF
}

# ---------- state file ----------
state_get() { [ -f "$STATE" ] && sed -n "s/^$1=//p" "$STATE" | head -1 || true; }
state_write() {
  mkdir -p "$CONFIG_DIR"
  {
    echo "# written by scripts/fork/cloud.sh; no secrets"
    echo "target=$TARGET"
    echo "ssh_port=$SSH_PORT"
    echo "frames_port=$FRAMES_PORT"
    echo "local_port=$LOCAL_PORT"
    echo "app_port=$APP_PORT"
    echo "idle_min=$IDLE_MIN"
    echo "connected_at=$(date -u +%FT%TZ)"
  } >"$STATE"
}
load_state() {
  [ -f "$STATE" ] || die "no connection saved ($STATE). Run: scripts/fork/cloud.sh connect root@IP -p PORT"
  TARGET="$(state_get target)"; SSH_PORT="$(state_get ssh_port)"; FRAMES_PORT="$(state_get frames_port)"
  LOCAL_PORT="$(state_get local_port)"; APP_PORT="$(state_get app_port)"; IDLE_MIN="$(state_get idle_min)"
}

# ---------- ssh ----------
ssh_opts() {
  printf '%s\n' -o IdentitiesOnly=yes -i "$SSH_KEY" -p "$SSH_PORT" -o ConnectTimeout=15 -o StrictHostKeyChecking=accept-new \
    -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o LogLevel=ERROR
}
rssh() { # rssh 'remote command'   (banner filtered)
  local opts=(); while IFS= read -r l; do opts+=("$l"); done < <(ssh_opts)
  ssh "${opts[@]}" "$TARGET" "$@" 2>&1 | quiet_banner
}
rbash() { # rbash ARGS... < script   runs the script on the machine with args
  local opts=(); while IFS= read -r l; do opts+=("$l"); done < <(ssh_opts)
  ssh "${opts[@]}" "$TARGET" bash -s -- "$@" 2>&1 | quiet_banner
}

# ---------- Mac app control channel (JSON lines on 127.0.0.1) ----------
app_call() { # app_call METHOD 'PARAMS-JSON' -> reply line (empty if the app is not reachable)
  local line
  { exec 3<>"/dev/tcp/127.0.0.1/$APP_PORT"; } 2>/dev/null || return 1
  printf '{"id":1,"method":"%s","params":%s}\n' "$1" "$2" >&3
  IFS= read -r -t 30 line <&3 || true
  exec 3>&- 3<&- || true
  printf '%s\n' "$line"
}
app_cmd() { app_call engine.execute "$(jq -cn --arg c "$1" --argjson p "$2" '{command:$c,params:$p}')"; }
app_up() { { exec 3<>"/dev/tcp/127.0.0.1/$APP_PORT"; } 2>/dev/null && { exec 3>&- 3<&-; return 0; } || return 1; }

# ---------- Mac side pieces ----------
mac_cli() {
  local c
  for c in "${EC_CLI:-}" "$REPO/target/local/local/effectcraft-cli" "$REPO/target/local/effectcraft-cli" \
    "$HOME/Developer/effectcraft/target/local/local/effectcraft-cli" "$HOME/Developer/effectcraft/target/local/effectcraft-cli" \
    "$(command -v effectcraft-cli || true)"; do
    if [ -n "$c" ] && [ -x "$c" ] && "$c" --help 2>&1 | grep -q 'drive-pull\|mirror USER@HOST'; then printf '%s\n' "$c"; return 0; fi
  done
  die "no effectcraft-cli with the mirror command on this Mac. Set EC_CLI=/path/to/effectcraft-cli (the app builds it on launch: scripts/fork/app.sh)."
}
ensure_token() {
  mkdir -p "$CONFIG_DIR"
  if [ ! -s "$TOKEN_FILE" ] || [ "$(wc -c <"$TOKEN_FILE")" -lt 9 ]; then
    (umask 077; openssl rand -hex 16 >"$TOKEN_FILE")
    say "🔑 new frame token saved to $TOKEN_FILE (Mac only; it goes to the server in its environment, not in the state file)"
  fi
  chmod 600 "$TOKEN_FILE"
}
tunnel_up() { ssh -S "$SOCK" -O check "$TARGET" >/dev/null 2>&1; }
tunnel_open() {
  if tunnel_up; then return 0; fi
  rm -f "$SOCK"
  if lsof -nP -iTCP:"$LOCAL_PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    die "port $LOCAL_PORT on this Mac is taken by another program (not our tunnel). Use --local-port N."
  fi
  local opts=(); while IFS= read -r l; do opts+=("$l"); done < <(ssh_opts)
  ssh "${opts[@]}" -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 -o ServerAliveCountMax=3 \
    -M -S "$SOCK" -f -N -L "127.0.0.1:$LOCAL_PORT:127.0.0.1:$FRAMES_PORT" "$TARGET" 2>&1 | quiet_banner
  tunnel_up || die "the SSH tunnel did not start."
}
tunnel_close() { if tunnel_up; then ssh -S "$SOCK" -O exit "$TARGET" >/dev/null 2>&1 || true; fi; rm -f "$SOCK"; }

# ---------- remote scripts ----------
REMOTE_PREFLIGHT='
set -u
echo "gpu=$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -1)"
echo "vram_free_mb=$(nvidia-smi --query-gpu=memory.free --format=csv,noheader,nounits 2>/dev/null | head -1)"
echo "vram_total_mb=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null | head -1)"
echo "disk_free_gb=$(df -BG --output=avail /workspace 2>/dev/null | tail -1 | tr -dc 0-9)"
echo "have_repo=$([ -d /workspace/effectcraft/.git ] && echo 1 || echo 0)"
echo "have_cargo=$(command -v cargo >/dev/null 2>&1 && echo 1 || { [ -x $HOME/.cargo/bin/cargo ] && echo 1 || echo 0; })"
echo "built=$(cat /workspace/effectcraft/target/local/.built-commit 2>/dev/null)"
echo "head=$(git -C /workspace/effectcraft rev-parse HEAD 2>/dev/null)"
'

# $1 = wanted commit. Fast-forwards the checkout (never discards anything) and starts a background build.
REMOTE_UPDATE='
set -eu
want="$1"; cd /workspace/effectcraft
export PATH="$HOME/.cargo/bin:$PATH"
if [ "$(git rev-parse HEAD)" != "$want" ]; then
  git cat-file -e "$want^{commit}" 2>/dev/null || { echo "ERR commit $want did not arrive"; exit 3; }
  git merge -q --ff-only "$want" || { echo "ERR cannot fast-forward /workspace/effectcraft to $want (local changes or another branch there)"; exit 3; }
fi
pgrep -f "cloud-build.state" >/dev/null && { echo "BUILDING (already running)"; exit 0; }
nohup bash -c "echo running >/workspace/cloud-build.state; if nice -n 15 taskset -c ${EC_BUILD_CPUS:-100-111} cargo build --profile local -p effectcraft-cli -j 8 >/workspace/cloud-build.log 2>&1; then echo $want >target/local/.built-commit; echo ok >/workspace/cloud-build.state; else echo failed >/workspace/cloud-build.state; fi" >/dev/null 2>&1 &
echo "BUILDING"
'

# args: frames_port commit idle_min token
REMOTE_SERVE='
set -u
fp="$1"; commit="$2"; idle="$3"; token="$4"
cli=/workspace/effectcraft/target/local/effectcraft-cli
pidf=/workspace/cloud-serve-$fp.pid; stampf=/workspace/cloud-serve-$fp.commit
alive() { [ -f "$pidf" ] && p=$(cat "$pidf") && [ -n "$p" ] && tr "\0" " " </proc/$p/cmdline 2>/dev/null | grep -q -- "serve --gpu --frames $fp"; }
if alive; then
  p=$(cat "$pidf")
  if [ "$(cat "$stampf" 2>/dev/null)" = "$commit" ] && tr "\0" "\n" </proc/$p/environ 2>/dev/null | grep -qx "EFFECTCRAFT_FRAME_TOKEN=$token"; then
    echo "SERVE already running (pid $p, commit ${commit:0:8})"; skip=1
  else
    echo "SERVE restarting (other commit or token)"; kill "$p"; sleep 2; skip=0
  fi
else skip=0; fi
if [ "$skip" = 0 ]; then
  cd /workspace
  [ -f "$HOME/.config/effectcraft/cloud.env" ] && . "$HOME/.config/effectcraft/cloud.env"
  export EFFECTCRAFT_FONT_DIRS EFFECTCRAFT_CJK_LOCALE
  EFFECTCRAFT_FRAME_TOKEN="$token" EFFECTCRAFT_FRAME_GPU_SLOTS=4 WGPU_BACKEND=vulkan RUST_LOG=info \
    setsid nohup "$cli" serve --gpu --frames "$fp" >/workspace/cloud-serve-$fp.log 2>&1 </dev/null &
  echo $! >"$pidf"; echo "$commit" >"$stampf"
  for i in 1 2 3 4 5 6 7 8 9 10; do sleep 1; ss -ltn 2>/dev/null | grep -q ":$fp " && break; done
  ss -ltn 2>/dev/null | grep -q ":$fp " && echo "SERVE started (pid $(cat "$pidf"))" || { echo "ERR serve did not listen on $fp:"; tail -5 /workspace/cloud-serve-$fp.log; exit 4; }
fi
# optional idle watchdog: stops the serve process (never the instance) after N minutes without a connected app
wd=/workspace/cloud-idle-$fp.pid
[ -f "$wd" ] && kill "$(cat "$wd")" 2>/dev/null || true; rm -f "$wd"
if [ "$idle" -gt 0 ] 2>/dev/null; then
  setsid nohup bash -c "last=\$(date +%s); while kill -0 \$(cat $pidf) 2>/dev/null; do
      if [ \$(ss -Htn state established \"( sport = :$fp )\" 2>/dev/null | wc -l) -gt 0 ]; then last=\$(date +%s); fi
      if [ \$((\$(date +%s)-last)) -gt $((idle*60)) ]; then echo \"\$(date -u +%T) idle $idle min: stopping serve\" >>/workspace/cloud-serve-$fp.log; kill \$(cat $pidf); break; fi
      sleep 20; done; rm -f $wd" >/dev/null 2>&1 </dev/null &
  echo $! >"$wd"; echo "IDLE watchdog: stops serve after $idle min without the app"
fi
'

# args: frames_port
REMOTE_STOP='
fp="$1"; pidf=/workspace/cloud-serve-$fp.pid; wd=/workspace/cloud-idle-$fp.pid
[ -f "$wd" ] && kill "$(cat "$wd")" 2>/dev/null; rm -f "$wd"
if [ -f "$pidf" ] && p=$(cat "$pidf") && [ -r /proc/$p/cmdline ] && tr "\0" " " </proc/$p/cmdline 2>/dev/null | grep -q -- "serve --gpu --frames $fp"; then
  kill "$p"; sleep 1; echo "serve stopped (pid $p)"; rm -f "$pidf" /workspace/cloud-serve-$fp.commit
else echo "no serve of ours running on $fp"; rm -f "$pidf"; fi
'

REMOTE_STATUS='
fp="$1"; pidf=/workspace/cloud-serve-$fp.pid
echo "gpu=$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | head -1)"
echo "vram_free_mb=$(nvidia-smi --query-gpu=memory.free --format=csv,noheader,nounits 2>/dev/null | head -1)"
echo "vram_total_mb=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null | head -1)"
echo "disk_free_gb=$(df -BG --output=avail /workspace 2>/dev/null | tail -1 | tr -dc 0-9)"
echo "built=$(cat /workspace/effectcraft/target/local/.built-commit 2>/dev/null)"
if [ -f "$pidf" ] && p=$(cat "$pidf") && tr "\0" " " </proc/$p/cmdline 2>/dev/null | grep -q -- "serve --gpu --frames $fp"; then
  echo "serve=running pid $p since $(ps -o lstart= -p $p | sed "s/  */ /g"), commit $(cat /workspace/cloud-serve-$fp.commit 2>/dev/null)"
  echo "clients=$(ss -Htn state established "( sport = :$fp )" 2>/dev/null | wc -l)"
else echo "serve=not running"; fi
'

parse_kv() { sed -n "s/^$1=//p" | head -1; }

# ---------- connect ----------
cmd_connect() {
  TARGET=""; SSH_PORT=22; FRAMES_PORT=9878; LOCAL_PORT=""; APP_PORT="${EFFECTCRAFT_CONTROL_PORT:-9877}"; IDLE_MIN=0
  local project="" do_mirror=1
  while [ $# -gt 0 ]; do
    case "$1" in
      -p|--ssh-port) SSH_PORT="$2"; shift 2 ;;
      --project) project="$2"; shift 2 ;;
      --frames-port) FRAMES_PORT="$2"; shift 2 ;;
      --local-port) LOCAL_PORT="$2"; shift 2 ;;
      --app-port) APP_PORT="$2"; shift 2 ;;
      --idle) IDLE_MIN="$2"; shift 2 ;;
      --no-mirror) do_mirror=0; shift ;;
      -h|--help) usage; exit 0 ;;
      -*) die "unknown option $1 (cloud.sh --help)" ;;
      *) [ -z "$TARGET" ] && TARGET="$1" || die "unexpected argument $1"; shift ;;
    esac
  done
  [ -n "$TARGET" ] || { usage; exit 2; }
  LOCAL_PORT="${LOCAL_PORT:-$FRAMES_PORT}"
  case "$SSH_PORT$FRAMES_PORT$LOCAL_PORT$APP_PORT$IDLE_MIN" in *[!0-9]*) die "ports and --idle must be numbers" ;; esac
  [ -f "$SSH_KEY" ] || die "ssh key $SSH_KEY not found (EC_CLOUD_KEY=...)"

  say "🔎 1/5 Checking the machine $TARGET (port $SSH_PORT)"
  local pre; pre="$(rbash <<<"$REMOTE_PREFLIGHT")" || true
  [ -n "$(printf '%s' "$pre" | parse_kv gpu)" ] || die "cannot reach $TARGET or no GPU there:\n$pre"
  say "   GPU: $(printf '%s' "$pre" | parse_kv gpu), free VRAM $(printf '%s' "$pre" | parse_kv vram_free_mb) / $(printf '%s' "$pre" | parse_kv vram_total_mb) MB, free disk $(printf '%s' "$pre" | parse_kv disk_free_gb) GB"
  [ "$(printf '%s' "$pre" | parse_kv have_repo)" = 1 ] || die "no $REMOTE_DIR on the machine: it is not set up for EffectCraft yet (see the effectcraft-remote-gpu skill: scripts/template/ec-provision.sh)."
  [ "$(printf '%s' "$pre" | parse_kv have_cargo)" = 1 ] || die "no cargo on the machine, so effectcraft-cli cannot be built there."
  local free; free="$(printf '%s' "$pre" | parse_kv disk_free_gb)"
  [ "${free:-0}" -ge 6 ] 2>/dev/null || say "⚠️  only ${free:-?} GB free on /workspace: a build and the media may not fit"
  if [ -x "$SKILL_SCRIPTS/check.sh" ]; then # the template's own check (non-fatal: it tests NVENC for the browser stream, not needed here)
    say "   machine check (check.sh):"
    rbash <"$SKILL_SCRIPTS/check.sh" | sed 's/^/     /' | head -12 || true
  fi

  local mac_commit; mac_commit="$(git -C "$REPO" rev-parse main)"
  local built; built="$(printf '%s' "$pre" | parse_kv built)"
  if [ "$built" = "$mac_commit" ]; then
    say "✅ effectcraft-cli on the machine is already at ${mac_commit:0:8} (this Mac's main)"
  else
    say "🔨 2/5 Building effectcraft-cli on the machine at ${mac_commit:0:8} (was ${built:0:8}); nothing is uploaded"
    # only the missing commits travel (git over ssh, a few MB at most); the machine never needs GitHub
    local sshcmd="ssh"; while IFS= read -r l; do sshcmd+=" '$l'"; done < <(ssh_opts)
    GIT_SSH_COMMAND="$sshcmd" git -C "$REPO" push -q -f "ssh://$TARGET$REMOTE_DIR" "$mac_commit:refs/cloud/mac-main" 2>&1 | quiet_banner \
      || die "could not send the source commits to the machine"
    local r; r="$(rbash "$mac_commit" <<<"$REMOTE_UPDATE")" || true
    case "$r" in *ERR*) die "$(printf '%s' "$r" | grep ERR)" ;; *BUILDING*) ;; *) die "could not start the build: $r" ;; esac
    local t0=$SECONDS st
    while :; do
      sleep 20
      st="$(rssh "tail -c 600 /workspace/cloud-build.log | tr '\r' '\n' | tail -2; echo STAMP=\$(cat $REMOTE_DIR/target/local/.built-commit 2>/dev/null); echo STATE=\$(cat /workspace/cloud-build.state 2>/dev/null)")" || true
      say "   … $((SECONDS - t0)) s: $(printf '%s' "$st" | grep -v -E 'STAMP=|STATE=' | tail -1 | cut -c1-110)"
      printf '%s' "$st" | grep -q "STAMP=$mac_commit" && break
      if printf '%s' "$st" | grep -q 'STATE=failed'; then
        printf '%s\n' "❌ the build on the machine failed; last lines of /workspace/cloud-build.log there:" >&2
        rssh 'tail -15 /workspace/cloud-build.log' >&2; exit 6
      fi
      [ $((SECONDS - t0)) -lt 3600 ] || die "the build takes over an hour; check /workspace/cloud-build.log on the machine"
    done
    say "✅ built in $((SECONDS - t0)) s"
  fi

  ensure_token
  if [ "$do_mirror" = 1 ]; then
    say "📦 3/5 Mirroring media + fonts (up to 100 MB from this Mac once; bigger files from Google Drive links)"
    local cli; cli="$(mac_cli)"
    local mopts=("$TARGET" --ssh-port "$SSH_PORT" --identity "$SSH_KEY")
    if [ -n "$project" ]; then mopts+=(--project "$project")
    elif app_up; then mopts+=(--bridge "$APP_PORT")
    else say "   ⚠️  no --project and the Mac app is not listening on $APP_PORT: skipping the mirror (re-run with --project F.ecproj)"; mopts=(); fi
    if [ ${#mopts[@]} -gt 0 ]; then
      if ! "$cli" mirror "${mopts[@]}" 2>&1 | quiet_banner; then
        cat >&2 <<'EOF'

❌ The mirror stopped: files or fonts are missing on the machine (listed above).
   Files over 100 MB are never uploaded from this Mac. For each one listed:
   1. upload it to Google Drive and copy its share link,
   2. paste the link into the project's sidecar "<project>.cloud.json" ("driveLinks": {"file name": "link"}),
      or in the app: command cloud.driveLinks {"set": {"file name": "link"}},
   3. run this same connect command again (finished files are not sent twice).
EOF
        exit 5
      fi
    fi
  else say "📦 3/5 Mirror skipped (--no-mirror)"; fi

  say "🚀 4/5 Frame server on the machine (port $FRAMES_PORT)"
  local token; token="$(cat "$TOKEN_FILE")"
  rbash "$FRAMES_PORT" "$mac_commit" "$IDLE_MIN" "$token" <<<"$REMOTE_SERVE" >/tmp/ec-cloud-serve.$$ 2>&1 || true
  if grep -q '^ERR' /tmp/ec-cloud-serve.$$; then cat /tmp/ec-cloud-serve.$$; rm -f /tmp/ec-cloud-serve.$$; die "the frame server did not start"; fi
  sed 's/^/   /' /tmp/ec-cloud-serve.$$; rm -f /tmp/ec-cloud-serve.$$

  say "🔌 5/5 SSH tunnel 127.0.0.1:$LOCAL_PORT → machine:$FRAMES_PORT and the Mac app"
  tunnel_open
  state_write
  say "   tunnel up"
  if app_up; then
    app_cmd prefs.set "$(jq -cn --arg a "127.0.0.1:$LOCAL_PORT" '{values:{"previews.cloudAddress":$a,"previews.previewOn":"cloud"}}')" >/dev/null || true
    local back; back="$(app_cmd prefs.get '{"key":"previews.previewOn"}' || true)"
    printf '%s' "$back" | grep -q cloud && say "   app (port $APP_PORT): Preview On = Cloud, address 127.0.0.1:$LOCAL_PORT" || say "   ⚠️  could not confirm the app setting: $back"
  else
    say "   ⚠️  the Mac app is not listening on control port $APP_PORT. Open Settings ▸ Previews ▸ Cloud Preview: Preview On = Cloud, Cloud Address = 127.0.0.1:$LOCAL_PORT"
  fi
  say "🎉 Connected. Check with: scripts/fork/cloud.sh status   Stop with: scripts/fork/cloud.sh disconnect"
  say "   Destroying the instance stays manual (vast.ai console)."
}

# ---------- status ----------
cmd_status() {
  load_state
  say "🔗 connection: $TARGET -p $SSH_PORT (saved $(state_get connected_at))"
  if tunnel_up; then say "✅ tunnel up: 127.0.0.1:$LOCAL_PORT → machine:$FRAMES_PORT"; else say "❌ tunnel down (connect again to reopen it)"; fi
  local s; s="$(rbash "$FRAMES_PORT" <<<"$REMOTE_STATUS")" || true
  if [ -z "$(printf '%s' "$s" | parse_kv gpu)" ]; then say "❌ machine not reachable over ssh"; else
    local mac_commit; mac_commit="$(git -C "$REPO" rev-parse main)"
    local b; b="$(printf '%s' "$s" | parse_kv built)"
    say "🖥  GPU: $(printf '%s' "$s" | parse_kv gpu), free VRAM $(printf '%s' "$s" | parse_kv vram_free_mb) / $(printf '%s' "$s" | parse_kv vram_total_mb) MB, free disk $(printf '%s' "$s" | parse_kv disk_free_gb) GB"
    say "🧱 cli on the machine: ${b:0:8}$([ "$b" = "$mac_commit" ] && echo " (= this Mac's main)" || echo " (Mac main is ${mac_commit:0:8}: connect again to rebuild)")"
    say "🚀 server: $(printf '%s' "$s" | parse_kv serve), clients connected: $(printf '%s' "$s" | parse_kv clients)"
  fi
  APP_PORT="${APP_PORT:-9877}"
  if app_up; then
    local pend pv ins; pend="$(app_cmd cloud.pendingUploads '{}' || true)"; pv="$(app_cmd prefs.get '{"key":"previews.previewOn"}' || true)"
    ins="$(app_call ui.inspect '{}' || true)"
    say "🍎 app: Preview On = $(printf '%s' "$pv" | jq -r '.result | if type=="object" then .value else . end' 2>/dev/null || echo '?'), pending uploads: $(printf '%s' "$pend" | jq -r '.result.count // "?"' 2>/dev/null || echo '?') ($(printf '%s' "$pend" | jq -r '(.result.bytes // 0) / 1048576 | floor' 2>/dev/null || echo '?') MB)"
    say "   link: $(printf '%s' "$ins" | jq -r '.result.cloud | "connected=\(.connected) ready=\(.ready) rtt=\(.rttMs // "-") ms frames=\(.frames) gpu=\(.gpu // "-")"' 2>/dev/null || echo 'unknown')"
  else say "🍎 app: not listening on control port $APP_PORT"; fi
}

# ---------- disconnect ----------
cmd_disconnect() {
  load_state
  APP_PORT="${APP_PORT:-9877}"
  if app_up; then
    app_cmd prefs.set '{"key":"previews.previewOn","value":"local"}' >/dev/null || true
    say "🍎 app: Preview On = This Mac"
  else say "🍎 app not reachable on $APP_PORT (set Preview On = This Mac yourself if it is open)"; fi
  tunnel_close; say "🔌 tunnel closed"
  rbash "$FRAMES_PORT" <<<"$REMOTE_STOP" | sed 's/^/🚀 /' || say "⚠️  could not reach the machine to stop the server (it may be gone already)"
  rm -f "$STATE"
  say "✅ Disconnected. The rented instance is still running and billing: destroy it in the vast.ai console when you are done."
}

case "${1:-}" in
  connect) shift; cmd_connect "$@" ;;
  status) shift; cmd_status "$@" ;;
  disconnect) shift; cmd_disconnect "$@" ;;
  -h|--help|help|"") usage ;;
  *) usage; exit 2 ;;
esac
