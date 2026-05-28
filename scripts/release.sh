#!/usr/bin/env bash
# Prepare for a release.
#
# All additional options are passed to `rooster release`.
set -eu

script_root="$(realpath "$(dirname "$0")")"
project_root="$(dirname "$script_root")"

echo "Updating metadata with rooster..."
cd "$project_root"
uvx --python 3.12 rooster@0.1.1 release "$@"

echo "Updating lockfile..."
cargo update -p cargo-hawk

version="$(cargo metadata --format-version=1 --no-deps | jq -r '.packages[] | select(.name == "cargo-hawk") | .version')"

echo "Creating release branch..."
git checkout -b "release/$version"
git add CHANGELOG.md Cargo.lock Cargo.toml
git commit -m "Bump version to $version"
