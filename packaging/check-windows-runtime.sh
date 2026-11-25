#!/bin/bash
# Check that a Midna for Windows folder (packaging/build-windows.sh) starts on a clean Windows 10,
# which has no Visual C++ redistributable: fails when a file in it needs one of the
# redistributable's DLLs (VCRUNTIME140.dll, MSVCP140.dll and the rest).
#
#   packaging/check-windows-runtime.sh DIR
#
# midna-app.exe, midnad.exe, midna.exe and midna-pty.exe link the C runtime statically
# (+crt-static), so they must not import the Universal CRT either (api-ms-win-crt-*.dll,
# ucrtbase.dll): an import of it means the static runtime didn't take. Microsoft's ConPTY
# (conpty.dll, OpenConsole.exe) imports the Universal CRT, which is part of Windows 10 itself, so
# only the redistributable's DLLs count against those. Run from Git Bash; it needs dumpbin.exe,
# from Visual Studio's C++ build tools (found with vswhere, or on PATH).
set -euo pipefail

DIR="${1:-}"
[ -n "$DIR" ] && [ -d "$DIR" ] || { echo "usage: check-windows-runtime.sh DIR (a Midna for Windows folder)" >&2; exit 2; }

DUMPBIN="$(command -v dumpbin.exe || true)"
if [ -z "$DUMPBIN" ]; then
  # Where the Visual Studio installer always puts it (%ProgramFiles(x86)%).
  vswhere="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
  if [ -x "$vswhere" ]; then
    DUMPBIN="$("$vswhere" -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 \
      -find 'VC/Tools/MSVC/**/bin/Hostx64/x64/dumpbin.exe' | tr -d '\r' | head -n 1)"
  fi
fi
[ -n "$DUMPBIN" ] || { echo "check-windows-runtime.sh: no dumpbin.exe (install Visual Studio's C++ build tools)" >&2; exit 2; }

# The Visual C++ redistributable's DLLs, and the Universal CRT's.
REDIST='^(vcruntime|msvcp|concrt|vccorlib|vcomp|vcamp|mfc)[0-9_a-z]*\.dll$'
UCRT='^(api-ms-win-crt-.*|ucrtbase)\.dll$'

bad=0 checked=0
shopt -s nullglob nocaseglob
for f in "$DIR"/*.exe "$DIR"/*.dll; do
  name="$(basename "$f")"
  case "${name,,}" in
    midna-app.exe|midnad.exe|midna.exe|midna-pty.exe) pattern="$REDIST|$UCRT" ;;
    *) pattern="$REDIST" ;;
  esac
  # dumpbin lists each import (and delay-load import) indented on a line of its own.
  deps="$("$DUMPBIN" -nologo -dependents "$(cygpath -w "$f")" | tr -d '\r' | sed -n 's/^ \{4\}\([^ ]*\.[dD][lL][lL]\)$/\1/p')"
  [ -n "$deps" ] || { echo "check-windows-runtime.sh: dumpbin found no imports in $name" >&2; exit 1; }
  hits="$(tr 'A-Z' 'a-z' <<<"$deps" | grep -E "$pattern" || true)"
  if [ -n "$hits" ]; then
    echo "check-windows-runtime.sh: $name imports C runtime DLLs it must not:" $hits >&2
    bad=1
  fi
  checked=$((checked + 1))
done
[ "$checked" -gt 0 ] || { echo "check-windows-runtime.sh: no .exe or .dll in $DIR" >&2; exit 1; }
[ "$bad" = 0 ] || exit 1
echo "==> $checked files in $DIR need no Visual C++ runtime"
