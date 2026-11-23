#!/usr/bin/env python3
"""Print the newest of the versions given, ordered like semver.

    ./scripts/newest-version.py 0.7.0-beta.2 0.6.2 0.7.0   ->  0.7.0

Versions are major.minor.patch, optionally with a pre-release: -beta.N, and on
Windows a build counter after it (0.7.0-beta.2.w.1, or 0.7.0-w.1 on a stable
base; docs/RELEASING.md, "Windows releases"). Pre-releases sort by semver's
rules, as the app's updater orders them: a pre-release comes before its release
(0.7.0-beta.2 < 0.7.0), numbers compare as numbers and come before words, and
more parts win when the rest are equal (0.7.0-beta.2 < 0.7.0-beta.2.w.1 <
0.7.0-beta.3 < 0.7.0-w.1 < 0.7.0).
"""
import re
import sys

PATTERN = re.compile(r"^(\d+)\.(\d+)\.(\d+)(?:-(beta\.\d+(?:\.w\.\d+)?|w\.\d+))?$")


def key(version):
    match = PATTERN.match(version)
    if not match:
        sys.exit(f"Not a version: {version}")
    major, minor, patch, pre = match.groups()
    # Semver: numeric parts compare as numbers and sort before alphanumeric ones.
    parts = tuple((0, int(p), "") if p.isdigit() else (1, 0, p) for p in pre.split(".")) if pre else ()
    return (int(major), int(minor), int(patch), pre is None, parts)


if len(sys.argv) < 2:
    sys.exit(f"Usage: {sys.argv[0]} <version>...")
print(max(sys.argv[1:], key=key))
