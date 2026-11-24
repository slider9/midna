#!/bin/bash
# End-to-end Windows update test (docs/RELEASING.md, "End-to-end test"). Run from Git Bash.
#
# Builds TEST-flavored v1 and v2 Midna for Windows (packaging/build-windows.sh --test: app id
# com.mrgnhnt.midna.test, listed as "Midna Test", its home built in under %TEMP%, installed to a
# folder under %TEMP%), so neither the installed Midna nor Midna Dev is touched. Signs their feed
# entries with the dev update key (packaging/gen-update-key.sh makes it if it's missing) and serves
# windows-beta.json and the zips from 127.0.0.1. Installs v1, starts it, and opens a terminal
# running a PowerShell loop that prints a counter. Then:
#
#   1. A bad build: the feed offers v2, but its zip holds v1, whose midnad --selftest reports the
#      wrong version. v1 refuses it at staging and keeps running.
#   2. A file that can't be replaced: the feed offers the real v2 while a file in the install is
#      held open. v1 downloads, verifies and applies it; the swap fails and rolls back, and the
#      helper starts v1 again, which says why.
#   3. The file is let go and v1 applies v2 for real: v2 starts, the daemon hands every terminal to
#      v2's midnad, and the terminal keeps its id, its shell and its counter.
#
# After each step the daemon, the terminal, its shell and the counter are checked. Then everything
# is cleaned up: the app's --uninstall (daemon.stop, the login entry, Start menu and Settings > Apps
# entries, the CLI copy and the install folder), any process left under the test folders, the test
# home and the HTTP server.
#
#   packaging/e2e/update-e2e-windows.sh [--no-build] [--keep]
#     --no-build  reuse the v1 and v2 builds of the last run
#     --keep      leave the test home and install folder for a look afterwards (processes, the
#                 login entry and the other entries are still removed)
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
BUILD=1 KEEP=0
for a in "$@"; do
  case "$a" in
    --no-build) BUILD=0 ;;
    --keep) KEEP=1 ;;
    -h|--help) sed -n '2,28p' "$0"; exit 0 ;;
    *) echo "update-e2e-windows.sh: unknown option $a" >&2; exit 2 ;;
  esac
done
case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ;; *) echo "update-e2e-windows.sh: run it on Windows, from Git Bash (macOS: packaging/e2e/update-e2e.sh)" >&2; exit 2 ;; esac

NAME="Midna Test"
WORK="$ROOT/packaging/e2e/work/windows"
TMP_U="$(cygpath -u "$TEMP")"
TH_U="$TMP_U/mdt-e2e"                    # the test home (short: AF_UNIX paths are 108 bytes at most)
TH="$(cygpath -w "$TH_U")"
DEST_ROOT="$TMP_U/mdt-e2e-app"
DEST="$DEST_ROOT/$NAME"                  # the install folder
APP="$DEST/midna-app.exe"
PORT="" FEED=""                          # a free port, picked once Python is known
KEY_DIR="$HOME/.config/midna-dev"
RUN_KEY='HKCU\Software\Microsoft\Windows\CurrentVersion\Run'
APPS_KEY='HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\com.mrgnhnt.midna.test'
# The test build's toast registration (midna-app/src/notify_windows.rs: its app id and its own
# click handler class, ACTIVATOR_TEST), which --uninstall leaves; and Midna Dev's class, which a
# test build must never point at itself.
TOAST_KEY='HKCU\Software\Classes\AppUserModelId\com.mrgnhnt.midna.test'
TOAST_CLASS_KEY='HKCU\Software\Classes\CLSID\{2F9ECA2D-874C-4854-A07F-81BB897BCFFB}'
DEV_CLASS_KEY='HKCU\Software\Classes\CLSID\{B130EDB7-F2F7-45B7-8AA1-FF07B076EE68}\LocalServer32'
LNK="$(cygpath -u "$APPDATA")/Microsoft/Windows/Start Menu/Programs/$NAME.lnk"
PY="$(command -v python3 || command -v python)"
SRV_PID="" HOLD_PID=""
PASS=0 FAIL=0

BASE="$(awk '/^\[workspace.package\]/{p=1} p && /^version *=/{gsub(/[" ]/,"",$0); sub(/version=/,""); print; exit}' Cargo.toml)"
case "$BASE" in *-*) SEP=. ;; *) SEP=- ;; esac
V1="$BASE${SEP}e2e.1" V2="$BASE${SEP}e2e.2"

say() { printf '\033[1m==> %s\033[0m\n' "$*"; }
ok() { printf '  \033[32mPASS\033[0m %s\n' "$*"; PASS=$((PASS+1)); }
bad() { printf '  \033[31mFAIL\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); }
check() { if eval "$2"; then ok "$1"; else bad "$1"; fi; }
wait_for() {  # <secs> <cmd...>
  local n=$(( $1 * 5 )); shift
  while [ $n -gt 0 ]; do if eval "$*"; then return 0; fi; sleep 0.2; n=$((n-1)); done; return 1
}
json() { "$PY" -c "import json,sys; v=json.load(sys.stdin); print($1)"; }
reg_has() { MSYS2_ARG_CONV_EXCL='*' reg.exe query "$1" "${@:2}" >/dev/null 2>&1; }
# Midna Dev's toast click server, or nothing.
dev_toast_server() { MSYS2_ARG_CONV_EXCL='*' reg.exe query "$DEV_CLASS_KEY" /ve 2>/dev/null | tr -d '\r' | grep -i 'REG_SZ'; }

# Nothing of the terminal this runs in may reach the test build (its own home is built in, but
# a MIDNA_SESSION would make its processes look like this terminal's).
unset MIDNA_HOME MIDNA_SOCKET MIDNA_SESSION MIDNA_PROJECT MIDNA_HOOKS_INJECTED MIDNA_SKILL

# The test build's CLI: bin\current names the folder it's in.
cli_path() {
  local cur; cur="$( { tr -d '[:space:]' < "$TH_U/bin/current"; } 2>/dev/null)" || return 1
  [ -n "$cur" ] && [ -f "$TH_U/bin/$cur/midna.exe" ] && echo "$TH_U/bin/$cur/midna.exe"
}
cli() { local c; c="$(cli_path)" || return 1; "$c" "$@"; }

# The pids of the processes named $2 whose executable is under folder $1.
pids_under() {
  local q="'" dir
  dir="$(cygpath -w "$1")"
  dir="${dir//$q/$q$q}"
  powershell.exe -NoProfile -NonInteractive -Command \
    "Get-Process -Name '$2' -ErrorAction SilentlyContinue | Where-Object { \$_.Path -and \$_.Path.StartsWith('$dir\\', 'OrdinalIgnoreCase') } | ForEach-Object { \$_.Id }" | tr -d '\r'
}
alive() { powershell.exe -NoProfile -NonInteractive -Command "if (Get-Process -Id $1 -ErrorAction SilentlyContinue) { exit 0 } else { exit 1 }"; }
app_pids() { pids_under "$DEST" midna-app; }
one_app() { [ "$(app_pids | wc -w)" -eq 1 ]; }
# The version in the install's marker ("Midna Test <version>. With this file...").
installed_version() { awk 'NR==1{print $3}' "$DEST/midna-bundle.txt" 2>/dev/null | sed 's/\.$//'; }
daemon_field() { cli info --json 2>/dev/null | json "v.get('$1', '')"; }
# The highest counter on the terminal's screen.
counter() { cli read "$SID" --screen 2>/dev/null | grep -o 'tick-[0-9]*' | sed 's/tick-//' | sort -n | tail -1; }
log_has() { grep -q -- "$1" "$TH_U/app.log" 2>/dev/null; }
# What app.log gained since mark_log.
mark_log() { LOG_MARK="$(wc -l < "$TH_U/app.log")"; }
new_log_has() { tail -n +"$((LOG_MARK + 1))" "$TH_U/app.log" 2>/dev/null | grep -q -- "$1"; }
leftovers() { find "$DEST" \( -name '*.old' -o -name '*.new' -o -name midna-update-leftovers.txt \) 2>/dev/null; }

start_app() {
  # Hidden and without taking focus. Its environment is this script's: the helper that applies an
  # update, and the app that helper starts, inherit it.
  powershell.exe -NoProfile -NonInteractive -Command "Start-Process -FilePath '$(cygpath -w "$APP")' -WorkingDirectory '$(cygpath -w "$DEST")' -WindowStyle Hidden"
}

# What must hold after every step: one daemon, the terminal with its shell, and a counter that
# went on from where it was.
terminal_kept() {
  local was="$1" now
  check "terminal $SID still listed" "cli list --json 2>/dev/null | grep -q '$SID'"
  check "its shell (pid $SHELL_PID) still runs" "alive $SHELL_PID"
  wait_for 10 "[ \"\$(counter)\" -gt $was ] 2>/dev/null"
  now="$(counter)"
  check "the counter went on from $was (now ${now:-none}), it didn't start over" "[ -n '$now' ] && [ '$now' -gt '$was' ]"
  LAST="${now:-$was}"
}

cleanup() {
  say "cleanup"
  [ -n "$HOLD_PID" ] && kill "$HOLD_PID" 2>/dev/null
  [ -n "$SRV_PID" ] && kill "$SRV_PID" 2>/dev/null
  if [ -f "$APP" ]; then
    "$APP" --uninstall 2>&1 | sed 's/^/  /'
    sleep 2
  fi
  # Belt and braces: nothing of ours may keep running (each by pid; never by name alone).
  for dir in "$TH_U" "$DEST_ROOT"; do
    for name in midna-app midnad midna-pty OpenConsole midna; do
      for p in $(pids_under "$dir" "$name"); do taskkill //PID "$p" //F >/dev/null 2>&1 && echo "  ended $name (pid $p)"; done
    done
  done
  [ -n "${SHELL_PID:-}" ] && alive "$SHELL_PID" 2>/dev/null && taskkill //PID "$SHELL_PID" //F >/dev/null 2>&1 && echo "  ended the test shell (pid $SHELL_PID)"
  reg_has "$RUN_KEY" /v "$NAME" && MSYS2_ARG_CONV_EXCL='*' reg.exe delete "$RUN_KEY" /v "$NAME" /f >/dev/null && echo "  removed the leftover login entry"
  reg_has "$APPS_KEY" && MSYS2_ARG_CONV_EXCL='*' reg.exe delete "$APPS_KEY" /f >/dev/null && echo "  removed the leftover Settings > Apps entry"
  for k in "$TOAST_KEY" "$TOAST_CLASS_KEY"; do
    reg_has "$k" && MSYS2_ARG_CONV_EXCL='*' reg.exe delete "$k" /f >/dev/null && echo "  removed $k"
  done
  if [ "$(dev_toast_server)" != "$DEV_TOAST0" ]; then echo "  WARNING: Midna Dev's toast class changed: '$DEV_TOAST0' -> '$(dev_toast_server)'"; fi
  rm -f "$LNK"
  if [ "$KEEP" = 0 ]; then
    sleep 1
    rm -rf "$TH_U" "$DEST_ROOT" "$WORK/srv" "$WORK/pub" 2>/dev/null || echo "  couldn't remove all of $TH_U and $DEST_ROOT (in use?)"
  fi
  if reg_has "$RUN_KEY" /v "$NAME" || reg_has "$APPS_KEY" || reg_has "$TOAST_KEY" || reg_has "$TOAST_CLASS_KEY" || [ -e "$LNK" ]; then echo "  WARNING: an entry of $NAME is still registered"; else echo "  no $NAME entries left"; fi
}

say "versions $V1 -> $V2; home $TH; install $(cygpath -w "$DEST")"
[ -n "$PY" ] || { echo "update-e2e-windows.sh: needs python (for the local feed server)" >&2; exit 2; }
if [ -n "$(app_pids)" ] || reg_has "$RUN_KEY" /v "$NAME"; then
  echo "  $NAME is already installed or running (a run that didn't clean up?); refusing. Uninstall it with \"$APP\" --uninstall" >&2
  exit 1
fi
if [ ! -f "$KEY_DIR/update-ed25519.key" ]; then
  say "make the dev update key"
  packaging/gen-update-key.sh "$KEY_DIR" || exit 1
fi
PUBKEY="$(tr -d '[:space:]' < "$KEY_DIR/update-ed25519.pub")"
DEV_TOAST0="$(dev_toast_server)"
PORT="$("$PY" -c "import socket; s = socket.socket(); s.bind(('127.0.0.1', 0)); print(s.getsockname()[1])" | tr -d '\r')"
FEED="http://127.0.0.1:$PORT/{channel}.json"
trap cleanup EXIT

if [ "$BUILD" = 1 ]; then
  for v in "$V2" "$V1"; do
    say "build $v (test flavor)"
    mkdir -p "$WORK/$v"
    if ! MIDNA_UPDATE_PUBKEY="$PUBKEY" LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast packaging/build-windows.sh --version "$v" \
      --out "$WORK/$v" --name "$NAME" --target-dir "$ROOT/target/e2e" --dev-home "$TH" --test --dev-drivers > "$WORK/$v/build.log" 2>&1; then
      grep -E "^error|^build-windows\.sh:" "$WORK/$v/build.log" >&2 || tail -n 20 "$WORK/$v/build.log" >&2
      echo "the $v build failed (log: $WORK/$v/build.log)" >&2
      exit 1
    fi
  done
fi
for v in "$V1" "$V2"; do [ -f "$WORK/$v/$NAME/midna-app.exe" ] || { echo "no build of $v in $WORK/$v (run without --no-build)" >&2; exit 1; }; done

say "sign the feeds"
rm -rf "$WORK/srv" "$WORK/pub"; mkdir -p "$WORK/srv" "$WORK/pub/good" "$WORK/pub/bad"
packaging/make-update.sh --windows "$WORK/$V2/$NAME" --url-base "http://127.0.0.1:$PORT" --channel beta \
  --out "$WORK/pub/good" --notes "e2e test update" --key "$KEY_DIR/update-ed25519.key" >/dev/null || exit 1
TOOL="$ROOT/packaging/release-tool/target/release/midna-release.exe"
# The bad build: v1's files, offered (and signed) as v2.
( cd "$WORK/$V1" && "$SYSTEMROOT/System32/tar.exe" -a -c -f "$(cygpath -w "$WORK/pub/bad/Midna-$V2-bad.zip")" "$NAME" ) || exit 1
"$TOOL" feed "$KEY_DIR/update-ed25519.key" "$WORK/pub/bad/Midna-$V2-bad.zip" --version "$V2" --url "http://127.0.0.1:$PORT/Midna-$V2-bad.zip" \
  --notes "e2e bad build" --os windows --min-windows 10.0.17763 --arch x86_64 > "$WORK/pub/bad/windows-beta.json" || exit 1
echo "  good: $(json "v['version'] + ' ' + v['sha256'][:16]" < "$WORK/pub/good/windows-beta.json"); bad: $(json "v['version'] + ' ' + v['sha256'][:16]" < "$WORK/pub/bad/windows-beta.json")"
publish() {  # good | bad | none
  case "$1" in
    none) echo '{"note":"no update yet"}' > "$WORK/srv/windows-beta.json" ;;
    *) cp "$WORK/pub/$1"/*.zip "$WORK/srv/" && cp "$WORK/pub/$1/windows-beta.json" "$WORK/srv/windows-beta.json" ;;
  esac
}
publish none
TOKEN="e2e-$$-$RANDOM"
echo "$TOKEN" > "$WORK/srv/token.txt"
"$PY" -m http.server --bind 127.0.0.1 --directory "$(cygpath -w "$WORK/srv")" "$PORT" > "$WORK/http.log" 2>&1 &
SRV_PID=$!
# Its own server, not something else on the port.
if ! wait_for 10 "[ \"\$(curl -sf http://127.0.0.1:$PORT/token.txt | tr -d '\r')\" = '$TOKEN' ]"; then
  bad "feed server up on 127.0.0.1:$PORT"; tail -n 5 "$WORK/http.log"; exit 1
fi
ok "feed server up on 127.0.0.1:$PORT"

say "install $V1 to $(cygpath -w "$DEST")"
rm -rf "$TH_U" "$DEST_ROOT"; mkdir -p "$TH_U" "$DEST_ROOT"
cp -R "$WORK/$V1/$NAME" "$DEST"
# Straight to the terminal, past the first-run setup.
echo '{"onboarding":{"finished":true}}' > "$TH_U/app-state.json"
export MIDNA_UPDATE_FEED_URL="$FEED" MIDNA_UPDATE_CHECK_SECS=6 MIDNA_DEBUG_UPDATE=apply MIDNA_NO_ACTIVATE=1
start_app
check "app $V1 started" "wait_for 30 '[ -n \"\$(app_pids)\" ]'"
check "first launch installed midnad into the test home" "wait_for 30 'cli_path >/dev/null'"
check "daemon up" "wait_for 30 'cli info >/dev/null 2>&1'"
check "login entry \"$NAME\" written" "reg_has '$RUN_KEY' /v '$NAME'"
check "Start menu and Settings > Apps entries are the test build's" "[ -f '$LNK' ] && reg_has '$APPS_KEY'"
check "toasts registered under the test build's own app id and class" "wait_for 10 \"reg_has '$TOAST_KEY' && reg_has '$TOAST_CLASS_KEY'\""
check "Midna Dev's toast class untouched" "[ \"\$(dev_toast_server)\" = \"\$DEV_TOAST0\" ]"
INFO0="$(cli info --json)"; echo "  $INFO0" | head -c 400; echo
D_PID0="$(echo "$INFO0" | json "v['pid']")"
check "daemon reports $V1" "[ \"\$(echo '$INFO0' | json \"v['version']\")\" = '$V1' ]"
check "daemon runs from the test home" "echo '$INFO0' | grep -qi 'mdt-e2e'"
cli settings set updates.channel beta >/dev/null || bad "set updates.channel beta"
# The upgrade waits while the PC is busy; a test run shouldn't.
cli settings set system.busy_load 2000 >/dev/null 2>&1 || echo "  (system.busy_load not raised: a busy PC may postpone the daemon upgrade)"

say "open a terminal that counts"
SID="$(cli open --name e2e-counter --json -- powershell.exe -NoLogo -NoProfile -Command '$i=0; while ($true) { $i++; Write-Output tick-$i; Start-Sleep -Milliseconds 500 }' | json "v.get('id') or v['session']['id']")"
echo "  terminal $SID"
check "the counter runs" "wait_for 20 '[ -n \"\$(counter)\" ]'"
SHELL_PID="$(cli list --json | json "[s for s in (v if isinstance(v, list) else v.get('sessions', [])) if s['id'] == '$SID'][0].get('pid') or ''")"
echo "  shell pid $SHELL_PID, counter at $(counter)"
check "the terminal has a shell pid" "[ -n '$SHELL_PID' ]"
LAST="$(counter)"
APP_PID0="$(app_pids)"

say "1. a bad build: the feed offers $V2, its midnad says $V1"
publish bad
check "v1 refused it at staging" "wait_for 40 \"log_has 'the update says it is $V1, the feed said $V2'\""
check "the app wasn't replaced (pid $APP_PID0)" "[ \"\$(app_pids)\" = '$APP_PID0' ]"
check "the install is still $V1" "[ \"\$(installed_version)\" = '$V1' ]"
check "the updater reports the error" "cli updates status --json 2>/dev/null | grep -q 'the update says it is'"
check "same daemon (pid $D_PID0, $V1)" "[ \"\$(daemon_field pid)\" = '$D_PID0' ] && [ \"\$(daemon_field version)\" = '$V1' ]"
terminal_kept "$LAST"

say "2. a file that can't be replaced: the feed offers $V2 while midna.exe is held open"
HELD="$(cygpath -w "$DEST/midna.exe")"
# Open without FILE_SHARE_DELETE, so it can't be renamed aside, until this script lets it go.
powershell.exe -NoProfile -NonInteractive -Command "\$f = [IO.File]::Open('$HELD', 'Open', 'Read', 'Read'); while (\$true) { Start-Sleep -Seconds 1 }" &
HOLD_PID=$!
sleep 2
mark_log
publish good
check "v1 downloaded, verified and staged $V2" "wait_for 60 'new_log_has \"update $V2 verified and staged\"'"
check "the swap failed and put $V1 back" "wait_for 90 'new_log_has \"the old build is back\"'"
# Keep it from trying again until the file is let go.
publish none
check "$V1 started again and says why" "wait_for 30 'new_log_has \"v$V1 pid .* the last update wasn.t installed\"'"
check "one app runs, from the install" "wait_for 20 one_app"
check "the install is still $V1" "[ \"\$(installed_version)\" = '$V1' ]"
check "no .new or .old files left in the install" "[ -z \"\$(leftovers)\" ]"
check "same daemon (pid $D_PID0, $V1)" "[ \"\$(daemon_field pid)\" = '$D_PID0' ] && [ \"\$(daemon_field version)\" = '$V1' ]"
terminal_kept "$LAST"

say "3. the file is let go: $V1 applies $V2"
kill "$HOLD_PID" 2>/dev/null; wait "$HOLD_PID" 2>/dev/null; HOLD_PID=""
for p in $(powershell.exe -NoProfile -NonInteractive -Command "Get-CimInstance Win32_Process -Filter \"Name='powershell.exe'\" | Where-Object { \$_.CommandLine -like '*IO.File]::Open*mdt-e2e-app*' } | ForEach-Object { \$_.ProcessId }" | tr -d '\r'); do
  taskkill //PID "$p" //F >/dev/null 2>&1
done
mark_log
publish good
check "the install is $V2" "wait_for 90 '[ \"\$(installed_version)\" = \"$V2\" ]'"
check "app $V2 started from the install" "wait_for 30 'new_log_has \"v$V2 pid .* launch from\"'"
check "the daemon reports $V2" "wait_for 60 '[ \"\$(daemon_field version)\" = \"$V2\" ]'"
INFO1="$(cli info --json)"; echo "  $INFO1" | head -c 400; echo
D_PID1="$(echo "$INFO1" | json "v['pid']")"
echo "  daemon pid $D_PID0 -> $D_PID1 (Windows starts a new midnad and hands it the terminals)"
check "the old midnad (pid $D_PID0) exited" "wait_for 30 '! alive $D_PID0'"
terminal_kept "$LAST"
SCREEN="$(cli read "$SID" --screen 2>/dev/null)"
check "the screen still shows ticks from before the update" "echo \"\$SCREEN\" | grep -q 'tick-'"
check "exactly one app process" "one_app"
check "the old files are cleared from the install" "wait_for 20 '[ -z \"\$(leftovers)\" ]'"
echo "  app.log:"; sed 's/^/    /' "$TH_U/app.log"
echo "  midnad.log (last 15):"; tail -n 15 "$TH_U/midnad.log" 2>/dev/null | sed 's/^/    /'

say "result: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
