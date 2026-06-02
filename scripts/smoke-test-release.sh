#!/usr/bin/env bash
# Run an installed or extracted prebuilt Hawk release.
set -euo pipefail

binary_dir="${1:?usage: smoke-test-release.sh BINARY_DIR}"
script_root="$(realpath "$(dirname "$0")")"
project_root="$(dirname "$script_root")"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT

for binary in cargo-hawk cargo-hawk-driver; do
  if [[ ! -x "$binary_dir/$binary" ]]; then
    echo "missing release executable: $binary_dir/$binary" >&2
    exit 1
  fi
done

cp -R "$project_root/tests/fixtures/basic/." "$fixture"

env -u RUSTC_BOOTSTRAP RUSTUP_TOOLCHAIN=1.95.0 \
  "$binary_dir/cargo-hawk" --help >/dev/null
env -u RUSTC_BOOTSTRAP RUSTUP_TOOLCHAIN=1.95.0 \
  "$binary_dir/cargo-hawk" \
  --manifest-path "$fixture/Cargo.toml" \
  -A warnings
