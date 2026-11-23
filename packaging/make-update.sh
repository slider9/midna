#!/bin/bash
# Package a built Midna as an update: the archive plus a signed feed entry.
#   packaging/make-update.sh --app dist/0.1.1/Midna.app --url-base https://midna.dev/updates/files \
#       [--channel stable] [--notes "…"] [--out dist/updates] [--key ~/.config/midna-dev/update-ed25519.key]
#       [--arch universal|aarch64|x86_64] [--min-macos 12.0]
#   packaging/make-update.sh --windows dist/0.1.1-windows/Midna --url-base … [--min-windows 10.0.17763] …
# macOS: writes <out>/Midna-<version>.app.tar.gz and <out>/<channel>.json (signed with
# midna-update-v1). Windows (--windows, the folder packaging/build-windows.sh makes): writes
# <out>/Midna-<version>-windows-<arch>.zip and <out>/windows-<channel>.json (midna-update-v2,
# arch x86_64 unless --arch says otherwise). Upload the archive to <url-base>/ and the JSON to
# the feed URL (updates.feed_url, {channel} -> channel, or windows-<channel> on Windows).
# MIDNA_RELEASE_TOOL: a prebuilt packaging/release-tool binary to use instead of building it
# (CI builds it once and hands it to the job that packs the notarized app).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="" WIN="" URL_BASE="" CHANNEL=stable NOTES="" OUT="$ROOT/dist/updates" KEY="${MIDNA_UPDATE_KEY:-$HOME/.config/midna-dev/update-ed25519.key}"
ARCH="" MIN=12.0 MIN_WINDOWS=10.0.17763
while [ $# -gt 0 ]; do
  case "$1" in
    --app) APP="$2"; shift 2 ;;
    --windows) WIN="$2"; shift 2 ;;
    --url-base) URL_BASE="${2%/}"; shift 2 ;;
    --channel) CHANNEL="$2"; shift 2 ;;
    --notes) NOTES="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --key) KEY="$2"; shift 2 ;;
    --arch) ARCH="$2"; shift 2 ;;
    --min-macos) MIN="$2"; shift 2 ;;
    --min-windows) MIN_WINDOWS="$2"; shift 2 ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "make-update.sh: unknown option $1" >&2; exit 2 ;;
  esac
done
usage="usage: make-update.sh --app PATH/Midna.app --url-base URL, or --windows PATH/Midna --url-base URL"
{ [ -n "$APP" ] && [ -n "$WIN" ]; } && { echo "make-update.sh: --app or --windows, not both" >&2; exit 2; }
[ -n "$URL_BASE" ] || { echo "$usage" >&2; exit 2; }
if [ -n "$WIN" ]; then
  # It asks the build's midnad.exe for the version and zips with Windows' own tar.
  case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ;; *) echo "make-update.sh: --windows runs on Windows, from Git Bash" >&2; exit 2 ;; esac
  [ -f "$WIN/midna-bundle.txt" ] && [ -f "$WIN/midnad.exe" ] ||
    { echo "make-update.sh: $WIN isn't a Midna for Windows folder (no midna-bundle.txt and midnad.exe; packaging/build-windows.sh makes one)" >&2; exit 2; }
else
  [ -d "$APP" ] || { echo "$usage" >&2; exit 2; }
fi
[ -f "$KEY" ] || { echo "no signing key at $KEY (packaging/gen-update-key.sh makes a dev one)" >&2; exit 1; }
if [ -n "${MIDNA_RELEASE_TOOL:-}" ]; then
  TOOL="$MIDNA_RELEASE_TOOL"
else
  TOOL="$ROOT/packaging/release-tool/target/release/midna-release"
  cargo build --release -q --manifest-path "$ROOT/packaging/release-tool/Cargo.toml" --target-dir "$ROOT/packaging/release-tool/target"
fi
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"   # tar runs from the app's folder, so a relative --out would miss

if [ -n "$WIN" ]; then
  ARCH="${ARCH:-x86_64}"
  # The folder has no version file (the marker is prose), so midnad says it.
  VERSION="$("$WIN/midnad.exe" --version | awk '{print $2}' | tr -d '\r')"
  [ -n "$VERSION" ] || { echo "make-update.sh: $WIN/midnad.exe --version printed no version" >&2; exit 1; }
  ARCHIVE="$OUT/Midna-$VERSION-windows-$ARCH.zip"
  FEED="$OUT/windows-$CHANNEL.json"
  rm -f "$ARCHIVE"
  # The folder sits at the top of the zip, as Midna.app does in the macOS archive. Windows'
  # own tar (bsdtar, in every Windows 10 since 1803) writes zips; Git Bash's GNU tar doesn't.
  ( cd "$(dirname "$WIN")" && "$SYSTEMROOT/System32/tar.exe" -a -c -f "$(cygpath -w "$ARCHIVE")" "$(basename "$WIN")" )
  "$TOOL" feed "$KEY" "$ARCHIVE" --version "$VERSION" --url "$URL_BASE/$(basename "$ARCHIVE")" \
    --notes "$NOTES" --os windows --min-windows "$MIN_WINDOWS" --arch "$ARCH" > "$FEED"
else
  ARCH="${ARCH:-$(uname -m | sed 's/arm64/aarch64/')}"
  /usr/bin/codesign --verify --deep --strict "$APP"
  VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
  ARCHIVE="$OUT/Midna-$VERSION.app.tar.gz"
  FEED="$OUT/$CHANNEL.json"
  # COPYFILE_DISABLE: no AppleDouble ._ files (they'd break the code signature check).
  ( cd "$(dirname "$APP")" && COPYFILE_DISABLE=1 tar -czf "$ARCHIVE" "$(basename "$APP")" )
  "$TOOL" feed "$KEY" "$ARCHIVE" --version "$VERSION" --url "$URL_BASE/$(basename "$ARCHIVE")" \
    --notes "$NOTES" --min-macos "$MIN" --arch "$ARCH" > "$FEED"
fi
"$TOOL" verify "$FEED" "$("$TOOL" pubkey "$KEY")" "$ARCHIVE" >/dev/null
echo "wrote $ARCHIVE"
echo "wrote $FEED:"
cat "$FEED"
