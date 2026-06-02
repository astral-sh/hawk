#!/usr/bin/env bash
# Verify that a release frontend is portable across matching Rust installations.
set -euo pipefail

target="${1:?usage: check-release-linkage.sh TARGET [PROFILE]}"
profile="${2:-release}"
binary_dir="${CARGO_TARGET_DIR:-target}/$target/$profile"
frontend="$binary_dir/cargo-hawk"
driver="$binary_dir/cargo-hawk-driver"
sysroot="$(rustc --print sysroot)"

for binary in "$frontend" "$driver"; do
  if [[ ! -x "$binary" ]]; then
    echo "missing release executable: $binary" >&2
    exit 1
  fi
done

case "$(uname -s)" in
  Darwin)
    frontend_linkage="$(otool -L "$frontend")"
    driver_linkage="$(otool -L "$driver")"
    if grep -q librustc_driver <<<"$frontend_linkage"; then
      echo "cargo-hawk must not link librustc_driver" >&2
      exit 1
    fi
    if ! grep -q librustc_driver <<<"$driver_linkage"; then
      echo "cargo-hawk-driver must link librustc_driver" >&2
      exit 1
    fi
    for binary in "$frontend" "$driver"; do
      loader_metadata="$(otool -l "$binary")"
      if grep -Fq "$sysroot" <<<"$loader_metadata"; then
        echo "$binary must not embed the build sysroot in loader metadata" >&2
        exit 1
      fi
    done
    ;;
  Linux)
    frontend_linkage="$(readelf -d "$frontend")"
    driver_linkage="$(readelf -d "$driver")"
    if grep -q librustc_driver <<<"$frontend_linkage"; then
      echo "cargo-hawk must not link librustc_driver" >&2
      exit 1
    fi
    if ! grep -q librustc_driver <<<"$driver_linkage"; then
      echo "cargo-hawk-driver must link librustc_driver" >&2
      exit 1
    fi
    for binary in "$frontend" "$driver"; do
      loader_metadata="$(readelf -d "$binary")"
      if grep -Fq "$sysroot" <<<"$loader_metadata"; then
        echo "$binary must not embed the build sysroot in loader metadata" >&2
        exit 1
      fi
    done
    ;;
  *)
    echo "unsupported release host: $(uname -s)" >&2
    exit 1
    ;;
esac
