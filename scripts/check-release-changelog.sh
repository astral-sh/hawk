#!/usr/bin/env bash
# Require cargo-dist to find release notes for the planned announcement.
set -euo pipefail

manifest="${1:?usage: check-release-changelog.sh DIST_MANIFEST}"

if ! jq -e '.announcement_changelog | strings | length > 0' "$manifest" >/dev/null; then
  echo "release has no CHANGELOG.md entry" >&2
  exit 1
fi
