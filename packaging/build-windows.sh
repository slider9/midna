#!/bin/bash
# Build release binaries and assemble Midna for Windows: a folder holding midna-app.exe,
# midnad.exe, midna.exe, the terminal keeper midna-pty.exe and Microsoft's newer ConPTY
# (conpty.dll and OpenConsole.exe; these three go wherever midnad.exe goes), the marker that makes
# midna-app.exe run as an installed Midna (crates/midna-app/src/nonmac/install.rs) and the
# licenses of the fonts and ConPTY. Copy the
# folder to %LOCALAPPDATA%\Programs\Midna and run midna-app.exe: it installs midnad into its
# home, starts it at login and copies the midna CLI into ~\.local\bin. Run from Git Bash.
#
#   packaging/build-windows.sh [options]
#     --version V         version for every binary (default: workspace Cargo version)
#     --out DIR           output directory (default: dist/<version>-windows); the build is DIR/NAME
#     --name NAME         the folder's name (default Midna)
#     --no-build          reuse the binaries from the last build (same --target-dir)
#     --target-dir DIR    cargo target dir (default: target/package, kept apart from dev builds)
#     --conpty FILE       the conpty.dll to ship, with its OpenConsole.exe beside it (default:
#                         .toolchain/conpty, which scripts/setup-conpty.sh fills; env.sh names it)
#     --dev-home DIR      build Midna Dev: every binary always uses DIR (a Windows path) as its
#                         home (MIDNA_HOME and MIDNA_SOCKET are ignored), and refuses to write the
#                         installed Midna's files (midna_proto::paths::guard_write); its icon is
#                         inverted and Windows lists it as "Midna Dev"
#     --test              with --dev-home: a test build of Midna Dev (packaging/e2e/update-e2e-windows.sh):
#                         app id com.mrgnhnt.midna.test, listed as "Midna Test", refuses Midna Dev's
#                         files too, and updates itself with MIDNA_UPDATE_PUBKEY
#     --dev-drivers       honour the MIDNA_DEBUG_* / updater-override env vars (cargo feature
#                         midna-app/dev-drivers). Test builds only: those drivers act as the human.
#
# Env: MIDNA_UPDATE_PUBKEY, the update keys to compile in (base64 ed25519 public keys, separated by
# commas or spaces; the release workflow sets the fork's). Without it, or with --dev-home but not
# --test, the build never updates itself. Unlike build-app.sh, there's no default dev key.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="" OUT="" NAME="Midna" BUILD=1 TARGET_DIR="$ROOT/target/package" DEV_HOME="" CONPTY="" TEST="" FEATURES=()
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --name) NAME="$2"; shift 2 ;;
    --no-build) BUILD=0; shift ;;
    --target-dir) TARGET_DIR="$2"; shift 2 ;;
    --dev-home) DEV_HOME="$2"; shift 2 ;;
    --conpty) CONPTY="$2"; shift 2 ;;
    --test) TEST=1; shift ;;
    --dev-drivers) FEATURES=(--features midna-app/dev-drivers); shift ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "build-windows.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ;; *) echo "build-windows.sh: run it on Windows, from Git Bash" >&2; exit 2 ;; esac
if [ -z "$VERSION" ]; then
  VERSION="$(awk '/^\[workspace.package\]/{p=1} p && /^version *=/{gsub(/[" ]/,"",$0); sub(/version=/,""); print; exit}' Cargo.toml)"
fi
OUT="${OUT:-$ROOT/dist/$VERSION-windows}"
case "$OUT" in /*|[A-Za-z]:*) ;; *) OUT="$ROOT/$OUT" ;; esac
if [ -n "$DEV_HOME" ]; then
  case "$DEV_HOME" in [A-Za-z]:\\*) ;; *) echo "build-windows.sh: --dev-home must be an absolute Windows path (C:\\…)" >&2; exit 2 ;; esac
  # The installed Midna's home is %LOCALAPPDATA%\<its bundle id>, with nothing after the id.
  prod="$(cygpath -w "$LOCALAPPDATA")\\com.mrgnhnt.midna"
  home="${DEV_HOME%\\}"
  if [ "${home,,}" = "${prod,,}" ] || [[ "${home,,}" == "${prod,,}\\"* ]]; then
    echo "build-windows.sh: --dev-home can't be the installed Midna's home" >&2; exit 2
  fi
  if [ -n "$TEST" ] && [ "${home,,}" = "${prod,,}.dev" ]; then
    echo "build-windows.sh: a --test build's --dev-home can't be Midna Dev's home" >&2; exit 2
  fi
fi
[ -z "$TEST" ] || [ -n "$DEV_HOME" ] || { echo "build-windows.sh: --test needs --dev-home" >&2; exit 2; }
DEST="$OUT/$NAME"
# shellcheck disable=SC1091
. "$ROOT/env.sh"
# Windows 10's own ConPTY drops dim text and keeps an app's keyboard modes to itself, so Midna
# never ships without the newer one (crates/midnad/src/pty_windows.rs).
CONPTY="${CONPTY:-${MIDNA_CONPTY:-}}"
[ -z "$CONPTY" ] || CONPTY="$(cygpath -u "$CONPTY")"
if [ -z "$CONPTY" ] || [ ! -f "$CONPTY" ] || [ ! -f "$(dirname "$CONPTY")/OpenConsole.exe" ]; then
  echo "build-windows.sh: no newer ConPTY (conpty.dll with OpenConsole.exe beside it); run scripts/setup-conpty.sh or pass --conpty" >&2
  exit 1
fi

# ---------------------------------------------------------------- build
BIN="$TARGET_DIR/release"
if [ "$BUILD" = 1 ]; then
  echo "==> cargo build --release (midna $VERSION, target $TARGET_DIR)"
  if [ -n "${MIDNA_UPDATE_PUBKEY:-}" ] && { [ -z "$DEV_HOME" ] || [ -n "$TEST" ]; }; then
    export MIDNA_UPDATE_PUBKEY
  else
    unset MIDNA_UPDATE_PUBKEY
    echo "build-windows.sh: no update key (MIDNA_UPDATE_PUBKEY): this build will never update itself" >&2
  fi
  MIDNA_DEV_HOME="$DEV_HOME" MIDNA_TEST_BUILD="$TEST" MIDNA_BUILD_VERSION="$VERSION" CARGO_TARGET_DIR="$TARGET_DIR" CARGO_BUILD_BUILD_DIR="$TARGET_DIR" \
    cargo build --release -p midna-app -p midnad -p midna-cli -p midna-pty --bin midna-app --bin midnad --bin midna --bin midna-pty ${FEATURES[@]+"${FEATURES[@]}"}
fi
for b in midna-app midnad midna midna-pty; do [ -f "$BIN/$b.exe" ] || { echo "missing $BIN/$b.exe" >&2; exit 1; }; done

# ---------------------------------------------------------------- assemble
echo "==> assembling $DEST"
rm -rf "$DEST"
mkdir -p "$DEST/Fonts"
cp "$BIN/midna-app.exe" "$BIN/midnad.exe" "$BIN/midna.exe" "$BIN/midna-pty.exe" "$DEST/"
cp "$CONPTY" "$DEST/conpty.dll"
cp "$(dirname "$CONPTY")/OpenConsole.exe" "$DEST/"
cp packaging/assets/conpty-LICENSE.txt "$DEST/ConPTY-LICENSE.txt"
# The fonts are compiled into midna-app (include_bytes!); the files and their OFL licenses
# ship too so the licenses travel with the binary.
cp crates/midna-app/assets/fonts/* "$DEST/Fonts/"
cat > "$DEST/midna-bundle.txt" <<EOF
$NAME $VERSION. With this file next to it, midna-app.exe runs as an installed Midna: it
installs midnad into its home, starts it at login and copies the midna CLI into ~\.local\bin.
EOF
echo "==> built $DEST"
