#!/usr/bin/env bash
# Build the HarmonyOS native libraries (every ABI in FIGCRAFT_NATIVE_TARGETS) before hvigor/CMake.
#
# WINDOWS NOTE (2026-11-18 lesson): the NDK's <triple>-clang is a #!/bin/sh wrapper that the Rust
# `cc` crate cannot exec on Windows ("%1 is not a valid Win32 application"), and it is never needed:
# use clang.exe (PE) with a -target/--sysroot -D__MUSL__ .bat wrapper as CC, and clang.exe as the
# Rust linker. The <triple>-clang script is only a problem for C compilation, not for a staticlib.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -f "$ROOT/scripts/dev.local.env" ]]; then source "$ROOT/scripts/dev.local.env"; fi

# The NDK native dir: clang.exe + llvm-ar.exe live here; the sysroot one level up under openharmony.
SDK="${FIGCRAFT_NATIVE_SDK:-${DEVECO_SDK_HOME:-${HOS_SDK_HOME:-}}/default/openharmony/native}"
if [[ ! -d "$SDK/llvm" ]]; then
  for candidate in \
    "/c/Program Files/Huawei/DevEco Studio/sdk/default/openharmony/native" \
    "${DEVECO_SDK_HOME:-}/openharmony/native" \
    "${DEVECO_SDK_HOME:-}/native"; do
    if [[ -d "$candidate/llvm" ]]; then SDK="$candidate"; break; fi
  done
fi
CLANG="$SDK/llvm/bin/clang.exe"
AR="$SDK/llvm/bin/llvm-ar.exe"
# The OHOS NDK sysroot lives INSIDE the native dir (.../openharmony/native/sysroot),
# not one level up. A wrong sysroot makes clang fail to find assert.h etc.
SYSROOT="$SDK/sysroot"
[[ -x "$CLANG" ]] || { echo "clang.exe not found under $SDK/llvm/bin" >&2; exit 1; }

# One list drives the build: CMake maps the same ABIs (entry/build-profile.json5 abiFilters).
TARGETS="${FIGCRAFT_NATIVE_TARGETS:-aarch64-unknown-linux-ohos x86_64-unknown-linux-ohos}"
PROFILE="${FIGCRAFT_RUST_PROFILE:-release}"
case "$PROFILE" in
  release) ARGS=(--release);;
  debug) ARGS=(--profile dev);;
  *) echo 'Profile must be release or debug' >&2; exit 2;;
esac

# The CC wrapper passes the OHOS target + sysroot + -D__MUSL__ to clang.exe for every C compile.
CC_DIR="$ROOT/native/rust/tools"
for target in $TARGETS; do
  case "$target" in
    aarch64-unknown-linux-ohos|x86_64-unknown-linux-ohos|armv7-unknown-linux-ohos) ;;
    *) echo "Unsupported target: $target" >&2; exit 2;;
  esac
  [[ -d "$(rustc --print sysroot)/lib/rustlib/$target" ]] || { echo "Install target: rustup target add $target" >&2; exit 1; }
  suffix="${target//-/_}"
  upper="$(printf '%s' "$suffix" | tr '[:lower:]' '[:upper:]')"
  cc="$CC_DIR/cc_${suffix}.bat"
  # Generate the per-ABI CC wrapper if missing.
  if [[ ! -f "$cc" ]]; then
    ohos_target="${target%%-unknown-*}-linux-ohos"
    cat > "$cc" <<EOF
@echo off
"$CLANG" -target $ohos_target --sysroot="$SYSROOT" -D__MUSL__ %*
EOF
  fi
  env "CC_${suffix}=$cc" "AR_${suffix}=$AR" \
      "CARGO_TARGET_${upper}_LINKER=$CLANG" \
      cargo build --locked --manifest-path "$ROOT/native/rust/Cargo.toml" --target "$target" "${ARGS[@]}"
done
echo "Native libraries built for: $TARGETS"
