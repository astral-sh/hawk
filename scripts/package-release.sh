#!/usr/bin/env bash
# Package the two executables required by a prebuilt Hawk release.
set -euo pipefail

target="${1:?usage: package-release.sh TARGET}"
script_root="$(realpath "$(dirname "$0")")"
project_root="$(dirname "$script_root")"
binary_dir="${CARGO_TARGET_DIR:-$project_root/target}/$target/release"
archive="cargo-hawk-$target"
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT

mkdir "$staging/$archive"
cp \
  "$binary_dir/cargo-hawk" \
  "$binary_dir/cargo-hawk-driver" \
  "$project_root/README.md" \
  "$project_root/LICENSE-APACHE" \
  "$project_root/LICENSE-MIT" \
  "$staging/$archive/"

cd "$project_root"
tar -C "$staging" -czf "$archive.tar.gz" "$archive"
shasum -a 256 "$archive.tar.gz" > "$archive.tar.gz.sha256"
