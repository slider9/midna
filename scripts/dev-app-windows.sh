#!/bin/bash
# Windows (Git Bash): build "Midna Dev" from this checkout, install it to
# %LOCALAPPDATA%\Programs\Midna Dev and (re)start it. scripts/dev-app.sh is the macOS one. It runs
# side by side with an installed Midna and never touches it: its own home
# (%LOCALAPPDATA%\com.mrgnhnt.midna.dev, built in, so the MIDNA_HOME / MIDNA_SOCKET a terminal
# exports can't point it elsewhere), its own daemon, login entry ("Midna Dev"), toasts and CLI
# copy (in its home's cli folder), and it refuses to write the installed Midna's files. It has
# an inverted icon and the Gruvbox theme.
#
# Windows can't upgrade a daemon in place yet (its terminals would close), so a rerun leaves a
# running daemon on its old build; the app says so. --restart-daemon stops it first.
#
#   scripts/dev-app-windows.sh [--test[=CRATES]] [--no-build] [--restart-daemon]
#     --test            run all the tests (scripts/test.sh) first and stop if they fail
#     --test=app        test only the named crates (comma separated: app, daemon, cli, proto) plus
#                       the crates that depend on them
#     --no-build        skip the build; just reinstall and restart from the last dev build
#     --restart-daemon  stop Midna Dev's daemon, which closes its terminals, so the new build's
#                       starts
#   scripts/dev-app-windows.sh --uninstall   quit it, stop its daemon, remove its login entry,
#                                            Start menu entry and CLI copy, and delete the app
#                                            (its home stays). Run it from a shell outside any
#                                            app package: inside one (the Claude desktop app's
#                                            terminals), Windows redirects the registry and
#                                            AppData changes into that package's own copy.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ;; *) echo "dev-app-windows.sh: run it on Windows, from Git Bash (macOS: scripts/dev-app.sh)" >&2; exit 2 ;; esac
ID="com.mrgnhnt.midna.dev"
DEV_HOME="$(cygpath -w "$LOCALAPPDATA")\\$ID"
HOME_U="$(cygpath -u "$DEV_HOME")"
DEST="$(cygpath -u "$LOCALAPPDATA")/Programs/Midna Dev"
APP="$DEST/midna-app.exe"
OUT="$ROOT/dist/dev-windows"
LOG="$HOME_U/app.log"

TEST=0 TEST_CRATES="" BUILD=1 UNINSTALL=0 RESTART=0
for a in "$@"; do
  case "$a" in
    --test) TEST=1 ;;
    --test=*) TEST=1; TEST_CRATES="${a#--test=}" ;;
    --no-build) BUILD=0 ;;
    --restart-daemon) RESTART=1 ;;
    --uninstall) UNINSTALL=1 ;;
    -h|--help) sed -n '2,25p' "$0"; exit 0 ;;
    *) echo "dev-app-windows.sh: unknown option $a" >&2; exit 2 ;;
  esac
done

# The pids of the processes named $2 that run from under folder $1.
pids_under() {
  local q="'" dir
  dir="$(cygpath -w "$1")"
  dir="${dir//$q/$q$q}"
  powershell.exe -NoProfile -NonInteractive -Command \
    "Get-Process -Name '$2' -ErrorAction SilentlyContinue | Where-Object { \$_.Path -and \$_.Path.StartsWith('$dir\\', 'OrdinalIgnoreCase') } | ForEach-Object { \$_.Id }" | tr -d '\r'
}

quit_dev_app() {
  [ -f "$APP" ] || return 0
  [ -n "$(pids_under "$DEST" midna-app)" ] || return 0
  echo "==> quit Midna Dev"
  "$APP" --quit || true
  for _ in $(seq 1 20); do [ -z "$(pids_under "$DEST" midna-app)" ] && return 0; sleep 0.5; done
  echo "Midna Dev didn't quit; quit it yourself and run this again" >&2
  return 1
}

# The installed CLI (bin\current names the folder it's in).
dev_cli() {
  local cur; cur="$(tr -d '[:space:]' < "$HOME_U/bin/current" 2>/dev/null)" || return 1
  [ -n "$cur" ] && [ -f "$HOME_U/bin/$cur/midna.exe" ] && echo "$HOME_U/bin/$cur/midna.exe"
}

# Stop Midna Dev's daemon (it runs from its home's bin folder) by its pid. Its terminals close.
stop_dev_daemon() {
  local pid
  for pid in $(pids_under "$HOME_U/bin" midnad); do
    echo "==> stop Midna Dev's daemon (pid $pid; its terminals close)"
    taskkill //PID "$pid" //F >/dev/null
  done
}

if [ "$UNINSTALL" = 1 ]; then
  quit_dev_app
  if [ -f "$APP" ]; then
    "$APP" --uninstall || true
  fi
  rm -rf "$DEST"
  echo "==> removed $DEST (home kept: $DEV_HOME)"
  exit 0
fi

# Crate graph: midna-proto <- midnad <- midna-cli, and midna-proto <- midna-app. A crate's
# dependents are tested with it, so --test=proto is the whole workspace.
test_args() {
  [ -n "$TEST_CRATES" ] || { echo "--workspace"; return; }
  local app=0 daemon=0 cli=0 c
  for c in ${TEST_CRATES//,/ }; do
    case "$c" in
      app|midna-app) app=1 ;;
      daemon|midnad) daemon=1 cli=1 ;;
      cli|midna-cli) cli=1 ;;
      proto|midna-proto) echo "--workspace"; return ;;
      *) echo "dev-app-windows.sh: unknown crate $c (app, daemon, cli, proto)" >&2; exit 2 ;;
    esac
  done
  local args=""
  [ "$app" = 0 ] || args="$args -p midna-app"
  [ "$daemon" = 0 ] || args="$args -p midnad"
  [ "$cli" = 0 ] || args="$args -p midna-cli"
  echo "${args# }"
}

if [ "$TEST" = 1 ]; then
  args="$(test_args)"
  echo "==> scripts/test.sh $args"
  if ! scripts/test.sh $args; then
    echo "tests failed; not installing" >&2
    exit 1
  fi
fi

if [ "$BUILD" = 1 ]; then
  echo "==> build Midna Dev"
  # Own target dir, so dev and release builds don't rebuild each other (different home). A failed
  # build stops here, so the last one's folder in $OUT is never installed in its place.
  mkdir -p "$OUT"
  if ! LIBGHOSTTY_VT_SYS_OPTIMIZE=ReleaseFast packaging/build-windows.sh --out "$OUT" --name "Midna Dev" \
    --target-dir "$ROOT/target/dev-app" --dev-home "$DEV_HOME" > "$OUT/build.log" 2>&1; then
    grep -E "^error|^build-windows\.sh:" "$OUT/build.log" >&2 || tail -n 20 "$OUT/build.log" >&2
    echo "the build failed (log: $OUT/build.log); not installing" >&2
    exit 1
  fi
  grep -E "==> built" "$OUT/build.log" || true
fi
[ -f "$OUT/Midna Dev/midna-app.exe" ] || { echo "no app at $OUT/Midna Dev (run without --no-build)" >&2; exit 1; }

# Copy next to the destination first, so the running copy is only quit once the new one is ready.
mkdir -p "$(dirname "$DEST")"
NEW="$DEST.new" OLD="$DEST.old"
rm -rf "$NEW" "$OLD"
cp -R "$OUT/Midna Dev" "$NEW"
quit_dev_app || { rm -rf "$NEW"; exit 1; }
[ "$RESTART" = 0 ] || stop_dev_daemon
# Swap what's in the folder, not the folder: Windows won't rename a folder that a running process
# works in (a daemon from before midnad moved into its home, or a program the app opened), but
# the files in it move freely.
mkdir -p "$DEST" "$OLD"
shopt -s nullglob dotglob
old=("$DEST"/*) new=("$NEW"/*)
shopt -u nullglob dotglob
[ ${#old[@]} = 0 ] || mv "${old[@]}" "$OLD/"
mv "${new[@]}" "$DEST/"
rmdir "$NEW"
rm -rf "$OLD" 2>/dev/null || echo "    left $OLD (something in it is in use)"

echo "==> start $(cygpath -w "$APP")"
lines=0
[ -f "$LOG" ] && lines=$(wc -l < "$LOG")
# Through Explorer, the way a human starts it: with Explorer's environment, holding none of this
# script's handles (one would keep a pipe the script is read through open while the app runs),
# and outside any app package the script runs in. The Claude desktop app is one: AppData writes
# from inside it land in its own folder, where midnad's socket doesn't work. Explorer's exit
# status is 1 either way.
explorer.exe "$(cygpath -w "$APP")" || true
# Wait for the app to report on its daemon (current, or still running an older build).
for _ in $(seq 1 40); do
  sleep 0.5
  line="$(tail -n +"$((lines + 1))" "$LOG" 2>/dev/null | grep -E "daemon\.upgrade|is current|no installed daemon|install:" | tail -1 || true)"
  if [ -n "$line" ]; then
    echo "    ${line#*Z }"
    break
  fi
done
# A fresh dev home starts on the default theme; give it Gruvbox so it can't pass for the real
# Midna (Twilight). A theme picked by hand is left alone.
if cli="$(dev_cli)" && [ "$("$cli" settings get theme 2>/dev/null)" = "system" ]; then
  "$cli" settings set theme gruvbox >/dev/null && echo "    theme: gruvbox"
fi
echo "==> done"
