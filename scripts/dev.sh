#!/usr/bin/env bash
# Build, install and launch the native Rust HarmonyOS host (FigCraft).
#
# Windows: PackageHap shells out to `java` by name and hvigor is a node script, so JAVA_HOME/bin
# and DevEco's node/bin must be on PATH. hvigorw lives in DevEco's tools dir, not the project.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -f "$ROOT/scripts/dev.local.env" ]]; then source "$ROOT/scripts/dev.local.env"; fi

DEV="/c/Program Files/Huawei/DevEco Studio"
HDC="${FIGCRAFT_HDC:-$DEV/sdk/default/openharmony/toolchains/hdc.exe}"
HVIGOR="${FIGCRAFT_HVIGOR:-$DEV/tools/hvigor/bin/hvigorw.bat}"
OHPM="${FIGCRAFT_OHPM:-$DEV/tools/ohpm/bin/ohpm.bat}"
export DEVECO_SDK_HOME="${DEVECO_SDK_HOME:-$DEV/sdk}"
export JAVA_HOME="${JAVA_HOME:-$DEV/jbr}"
# shellcheck disable=SC2086
export PATH="$JAVA_HOME/bin:$DEV/tools/node:$PATH"
BUNDLE="${FIGCRAFT_BUNDLE:-io.github.shaliuxin.figcraft.hmos}"
# arm64-v8a runs on real phones; x86_64 on the phone/PC emulator images.
ABIS="${FIGCRAFT_ABIS:-arm64-v8a x86_64}"
triple_for() {
  case "$1" in
    arm64-v8a) echo aarch64-unknown-linux-ohos ;;
    x86_64) echo x86_64-unknown-linux-ohos ;;
    armeabi-v7a) echo armv7-unknown-linux-ohos ;;
    *) echo "Unknown ABI: $1" >&2; return 1 ;;
  esac
}
one_device() {
  local count
  count="$("$HDC" list targets 2>/dev/null | tr -d '\r' | awk 'NF && $0 != "[Empty]" {n++} END {print n+0}')"
  [[ "$count" == "1" ]] || { echo "Connect exactly one simulator/device ($count found)" >&2; exit 1; }
}
build() {
  # shellcheck disable=SC2086
  FIGCRAFT_NATIVE_TARGETS="$(for abi in $ABIS; do triple_for "$abi"; done | tr ' ' '\n' | paste -sd' ' -)" \
    "$ROOT/scripts/build_native.sh"
  cd "$ROOT"
  "$OHPM" install
  "$HVIGOR" assembleHap --mode module -p product=default -p buildMode=debug --no-daemon
  hap="$ROOT/entry/build/default/outputs/default/entry-default-unsigned.hap"
  echo "Unsigned HAP: $hap"
  [[ -f "$hap" ]] && echo "HAP produced: $(ls -la "$hap")"
}
install() {
  one_device
  local hap="$ROOT/entry/build/default/outputs/default/entry-default-signed.hap"
  [[ -f "$hap" ]] || { echo "No signed HAP: configure local debug signing in DevEco Studio" >&2; exit 1; }
  local result
  result="$("$HDC" install -r -d "$hap")"
  echo "$result"
  [[ "$result" == *'install bundle successfully'* ]] || exit 1
}
launch() {
  one_device
  "$HDC" shell aa force-stop "$BUNDLE" >/dev/null 2>&1 || true
  "$HDC" shell aa start -a EntryAbility -b "$BUNDLE"
}
case "${1:-run}" in
  build) build ;;
  run) build; install; launch ;;
  launch) launch ;;
  *) echo 'Usage: scripts/dev.sh [build|run|launch]' >&2; exit 2 ;;
esac
