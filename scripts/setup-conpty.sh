#!/bin/bash
# Windows (Git Bash): put Microsoft's newer ConPTY in .toolchain/conpty/: conpty.dll and the
# OpenConsole.exe it runs, x64, from NuGet's Microsoft.Windows.Console.ConPTY (MIT; the
# notice ships as packaging/assets/conpty-LICENSE.txt). Windows 10's own ConPTY drops dim text
# and keeps an app's keyboard modes to itself; midnad uses this one instead when it sits beside
# midnad.exe (crates/midnad/src/pty_windows.rs).
#
#   scripts/setup-conpty.sh                 download the pinned version from nuget.org
#   scripts/setup-conpty.sh FILE.nupkg      take them from a package you downloaded
#
# Then env.sh points dev builds and the tests at it (MIDNA_CONPTY), and packaging/build-windows.sh
# puts it in the app's folder. Idempotent: the pinned version, once there, is kept.
set -euo pipefail
cd "$(dirname "$0")/.."

version=1.25.260930003
url="https://www.nuget.org/api/v2/package/Microsoft.Windows.Console.ConPTY/$version"
# What the pinned version's x64 files must be (Microsoft-signed; update with the version).
sums="feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a  conpty.dll
3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586  OpenConsole.exe"

case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ;; *) echo "setup-conpty.sh: only Windows needs it" >&2; exit 2 ;; esac
# A worktree uses the main checkout's toolchain, as env.sh does.
main="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)" && main="$(cygpath -u "${main%/.git}")" || main=.
[ -d "$main/.toolchain" ] || main=.
dir="$main/.toolchain/conpty"

given="${1:-}" pkg="${1:-}"
if [ -z "$pkg" ] && [ "$(cat "$dir/VERSION" 2>/dev/null)" = "$version" ] && [ -f "$dir/conpty.dll" ] && [ -f "$dir/OpenConsole.exe" ]; then
  echo "ConPTY $version is in $dir"
  exit 0
fi
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
if [ -z "$pkg" ]; then
  echo "==> download Microsoft.Windows.Console.ConPTY $version"
  curl -fsSL "$url" -o "$tmp/conpty.nupkg"
  pkg="$tmp/conpty.nupkg"
fi
[ -f "$pkg" ] || { echo "setup-conpty.sh: no file $pkg" >&2; exit 1; }

# A .nupkg is a zip. The x64 conpty.dll and OpenConsole.exe sit in an x64 (or win-x64) folder.
list="$(unzip -Z1 "$pkg")"
pick() { grep -iE "(^|/)(win-)?x64/(native/)?$1\$" <<<"$list" | head -1 || true; }
dll="$(pick 'conpty\.dll')" host="$(pick 'OpenConsole\.exe')"
if [ -z "$dll" ] || [ -z "$host" ]; then
  echo "setup-conpty.sh: no x64 conpty.dll and OpenConsole.exe in $pkg; it has:" >&2
  echo "$list" >&2
  exit 1
fi
got="$(unzip -p "$pkg" '*.nuspec' | grep -oE '<version>[^<]+' | head -1 | sed 's/<version>//' || true)"
unzip -q -o -j "$pkg" "$dll" "$host" -d "$tmp/out"
if [ "$got" = "$version" ] && ! (cd "$tmp/out" && sha256sum --quiet -c <<<"$sums"); then
  echo "setup-conpty.sh: $pkg's files aren't the ConPTY $version this script pins; kept nothing" >&2
  exit 1
fi
rm -rf "$dir"
mkdir -p "$dir"
mv "$tmp/out/"* "$dir/"
echo "${got:-unknown}" > "$dir/VERSION"
[ -z "$given" ] || [ "$got" = "$version" ] || echo "note: $pkg is ConPTY ${got:-of no version}; this script pins $version" >&2
echo "==> ConPTY ${got:-?} in $dir"
