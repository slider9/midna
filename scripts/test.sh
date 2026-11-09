#!/bin/bash
# Run the tests in waves: build first (at low priority, so it doesn't starve tests already
# running), then wait for a machine-wide lock and run. One checkout's tests run at a time, so
# the integration tests' temp daemons and their 5-10s timeouts aren't fighting another
# agent's run for the CPU.
#
#   scripts/test.sh                          the whole workspace
#   scripts/test.sh -p midnad -p midna-cli   just those crates (see "Test what you touched")
#   scripts/test.sh -p midnad queue -- --nocapture
#
# Arguments are cargo test's. The lock is $MIDNA_TEST_LOCK (default /tmp/midna-tests.lock).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck disable=SC1091
. "$ROOT/env.sh"
cd "$ROOT"
LOCK="${MIDNA_TEST_LOCK:-/tmp/midna-tests.lock}"

scope=(--workspace) cargo_args=()
for a in "$@"; do
  [ "$a" = "--" ] && break
  case "$a" in -p|--package|--package=*|-p?*|--workspace) scope=() ;; esac
  cargo_args+=("$a")
done

nice -n 10 cargo test ${scope[@]+"${scope[@]}"} ${cargo_args[@]+"${cargo_args[@]}"} --no-run
if command -v lockf >/dev/null 2>&1; then
  if ! lockf -s -t 0 "$LOCK" true; then
    echo "test.sh: another checkout's tests are running; waiting for $LOCK" >&2
  fi
  exec lockf -s "$LOCK" cargo test ${scope[@]+"${scope[@]}"} "$@"
fi
# No lockf (Git Bash on Windows): a lock directory holding its owner's pid. A dead owner's
# lock is taken over.
waited=
until mkdir "$LOCK.d" 2>/dev/null; do
  owner="$(cat "$LOCK.d/pid" 2>/dev/null || true)"
  if [ -n "$owner" ] && ! kill -0 "$owner" 2>/dev/null; then
    rm -rf "$LOCK.d"
    continue
  fi
  if [ -z "$waited" ]; then
    echo "test.sh: another checkout's tests are running; waiting for $LOCK.d" >&2
    waited=1
  fi
  sleep 1
done
echo $$ > "$LOCK.d/pid"
trap 'rm -rf "$LOCK.d"' EXIT
cargo test ${scope[@]+"${scope[@]}"} "$@"
