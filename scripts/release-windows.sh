#!/bin/bash
# Cut a Windows release: check the version, tag it and push the tag.
#
#   tb release cut windows-v0.1.0-beta.58.w.1   a build of a beta base (a prerelease)
#   tb release cut windows-v0.2.0-w.1           a build of a stable base
#   scripts/release-windows.sh <version> --dry-run   every check, then stop before the tag
#
# Windows releases ship from the fork (slider9/midna) and only from feat/windows-native, betas
# and full releases alike. tb release cut runs this script with TB_RELEASE set and records the
# release on the board when it succeeds. Run on its own, the script refuses unless you pass
# --force or --dry-run. It takes windows-v<version>, v<version> or <version>.
#
# A Windows version is the upstream base the files say (Cargo.toml) plus a counter that lives
# only in the tag: <base>.w.N on a beta base, <base>-w.N on a stable one (docs/RELEASING.md,
# "Windows releases"). So unlike release.sh this never writes a version or commits; it only
# tags the branch's head. It looks only at windows-v tags: bare v tags are macOS's.
#
# Pushing the tag runs .github/workflows/release-windows.yml, which builds the zip and its signed
# feed entry, publishes the release and updates windows-{stable,beta}.json.
set -euo pipefail
cd "$(dirname "$0")/.."

branch_from=feat/windows-native

force=
dry_run=
args=()
for arg in "$@"; do
  case "$arg" in
    --force) force=1 ;;
    --dry-run) dry_run=1 ;;
    *) args+=("$arg") ;;
  esac
done
version="${args[0]:-}"
version="${version#windows-}"
version="${version#v}"
if [ -z "${TB_RELEASE:-}" ] && [ -z "$force" ] && [ -z "$dry_run" ]; then
  echo "Run tb release cut windows-v${version:-<version>} instead; it runs this script and records the release. (--force to run it without the board, --dry-run to only check.)" >&2
  exit 1
fi
if [[ "$version" =~ ^([0-9]+\.[0-9]+\.[0-9]+-beta\.[0-9]+)\.w\.([0-9]+)$ ]]; then
  sep=.
elif [[ "$version" =~ ^([0-9]+\.[0-9]+\.[0-9]+)-w\.([0-9]+)$ ]]; then
  sep=-
else
  echo "Usage: $0 [windows-v|v]<base>.w.N (beta base, 0.1.0-beta.58.w.1) or <base>-w.N (stable base, 0.2.0-w.1) [--dry-run] [--force]" >&2
  exit 1
fi
base="${BASH_REMATCH[1]}"
counter="${BASH_REMATCH[2]}"
tag="windows-v$version"
if [ "$counter" = 0 ] || [[ "$counter" == 0* ]]; then
  echo "$tag: the counter starts at 1 and has no leading zeros." >&2
  exit 1
fi

# The fork's slug, for gh: with an upstream remote beside origin, gh would pick upstream's.
repo=${MIDNA_RELEASE_REPO:-$(git remote get-url origin | sed -E 's#^(https://github\.com/|git@github\.com:)##; s#\.git$##')}
if ! [[ "$repo" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  echo "origin isn't a GitHub repo ($(git remote get-url origin)); set MIDNA_RELEASE_REPO=<owner>/<repo>." >&2
  exit 1
fi

# The board runs this in the background: a push that wants a password or a sign-in window must
# fail instead of waiting for one.
export GIT_TERMINAL_PROMPT=0 GCM_INTERACTIVE=never

branch=$(git branch --show-current)
if [ "$branch" != "$branch_from" ]; then
  echo "Windows releases go out from $branch_from (on ${branch:-a detached HEAD})." >&2
  exit 1
fi
if [ -n "$(git status --porcelain)" ]; then
  echo "Commit or stash your changes first." >&2
  exit 1
fi
git fetch -q origin "$branch_from"
if [ "$(git rev-parse HEAD)" != "$(git rev-parse "origin/$branch_from")" ]; then
  echo "$branch_from isn't the same as origin's: pull (or push) it first, so the tag is on the commit everyone has." >&2
  git status -sb | head -1 >&2
  exit 1
fi

# The files' upstream base (they all have to agree; set-version.sh --check says which).
if ! files=$(./scripts/set-version.sh --check 2>&1); then
  echo "$files" >&2
  exit 1
fi
files_base=$(echo "$files" | awk 'NR==1{print $2}')

# Every Windows version tagged here or on origin. A tag only on this checkout still counts: it
# may be one whose push failed, or one deleted from origin that this checkout should drop too.
remote_tags=$(git ls-remote --tags --refs origin 'refs/tags/windows-v*') || {
  echo "Couldn't list origin's tags." >&2
  exit 1
}
versions=$({ git tag --list 'windows-v*'; echo "$remote_tags" | sed 's#.*refs/tags/##'; } |
  sed 's/^windows-v//' | sort -u | grep -E '^[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+\.w\.[0-9]+|-w\.[0-9]+)$' || true)

# The counter after the newest build of a base, or 1 for a base with none.
next_of() {
  local b="$1" s n
  [[ "$b" == *-beta.* ]] && s=. || s=-
  n=$(echo "$versions" | grep -E "^${b//./\\.}${s/./\\.}w\.[0-9]+$" | sed -E 's/.*w\.//' | sort -n | tail -1 || true)
  echo "$b${s}w.$(( ${n:-0} + 1 ))"
}

# Whether a tag's release is published: a draft (a workflow_dispatch run) or no release isn't.
published() {
  [ "$(gh release view "$1" --repo "$repo" --json isDraft -q .isDraft 2>/dev/null || true)" = false ]
}
recut_hint() {
  echo "Don't skip it. Once no run is going (gh run list --repo $repo --workflow release-windows.yml)," >&2
  echo "re-run its failed jobs, or delete its tag and cut it again:" >&2
  echo "  git push origin :refs/tags/$1 && git tag -d $1 && tb release cut $1" >&2
}

if echo "$versions" | grep -qxF -- "$version"; then
  echo "$tag is already tagged; a version is never reused." >&2
  if ! published "$tag"; then
    echo "It has no published release: its workflow failed, was cancelled or is still running." >&2
    recut_hint "$tag"
  fi
  exit 1
fi

if [ "$base" != "$files_base" ]; then
  echo "$tag is a build of $base, but the files say $files_base (scripts/set-version.sh --check)." >&2
  echo "The next Windows version of $files_base is windows-v$(next_of "$files_base")." >&2
  exit 1
fi

latest=
[ -z "$versions" ] || latest=$(./scripts/newest-version.py $versions)
if [ -n "$latest" ] && [ "$(./scripts/newest-version.py "$latest" "$version")" != "$version" ]; then
  echo "$tag isn't newer than the newest Windows tag, windows-v$latest." >&2
  exit 1
fi

next=$(next_of "$base")
if [ "$version" != "$next" ]; then
  echo "$tag skips a number: the next Windows build of $base is windows-v$next." >&2
  exit 1
fi

# Never skip a version: a tag whose release was never published gets cut again, not a new one.
if [ -n "$latest" ] && ! published "windows-v$latest"; then
  echo "windows-v$latest has no published release (none, or only a draft): its workflow failed, was cancelled or is still running." >&2
  recut_hint "windows-v$latest"
  exit 1
fi

# The push the cut ends with, without making it: it has to get past sign-in on its own.
if ! out=$(git push --dry-run --porcelain origin "HEAD:refs/tags/$tag" </dev/null 2>&1); then
  echo "$out" >&2
  echo "Pushing to origin needs a sign-in this script can't give. Push something to it by hand once (the credential manager keeps the sign-in), then run this again." >&2
  exit 1
fi

head=$(git rev-parse --short HEAD)
if [ -n "$dry_run" ]; then
  echo "Dry run: $tag (a build of $base${latest:+, after windows-v$latest}) would tag $branch_from at $head and push the tag to $repo."
  exit 0
fi

git tag -a "$tag" -m "Midna $version for Windows"
if ! git push origin "refs/tags/$tag" </dev/null; then
  git tag -d "$tag" >/dev/null
  echo "Couldn't push $tag; the local tag is gone again, so cut it again once the push works." >&2
  exit 1
fi

echo
echo "Pushed $tag. The Windows release workflow builds it and publishes the release:"
workflow="https://github.com/$repo/actions/workflows/release-windows.yml"
run=
for _ in $(seq 1 15); do
  run=$(gh run list --repo "$repo" --workflow release-windows.yml --branch "$tag" --limit 1 --json url -q '.[0].url' 2>/dev/null || true)
  [ -z "$run" ] || break
  sleep 2
done
echo "  ${run:-$workflow}"
