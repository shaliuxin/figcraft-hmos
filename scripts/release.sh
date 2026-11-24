#!/usr/bin/env bash
# Publish the signed/unsigned HAP to the shaliuxin/figcraft-hmos GitHub release.
#
# This needs a GitHub credential the headless shell does not have (gh CLI or a token in the
# credential helper). Run it from a shell that is authenticated as shaliuxin, e.g. after
# `gh auth login`, or with GITHUB_TOKEN exported. It mirrors the sibling *-hmos releases
# (tag v<upstream>, name "<App> <ver>", asset "<App>-<ver>-unsigned.hap").
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Upstream version -> versionCode already fixed in AppScope/app.json5; read the name here.
APP="FigCraft"
VER="$(python3 - "$ROOT/AppScope/app.json5" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as f:
    print(json.load(f)["app"]["versionName"])
PY
)"
TAG="v${VER}"
REPO="shaliuxin/figcraft-hmos"
UNSIGNED="$ROOT/entry/build/default/outputs/default/entry-default-unsigned.hap"
SIGNED="$ROOT/entry/build/default/outputs/default/entry-default-signed.hap"

[[ -f "$UNSIGNED" ]] || { echo "Unsigned HAP missing; run scripts/dev.sh build first" >&2; exit 1; }
[[ -x "$(command -v gh)" ]] || { echo 'gh CLI not found; run "gh auth login" first' >&2; exit 2; }

# 1) Create the repo if it does not exist. Sibling *-hmos repos are public; match that.
if ! gh repo view "$REPO" >/dev/null 2>&1; then
  echo "Creating $REPO ..."
  gh repo create "$REPO" --public \
    --description "FigCraft (storytold Figma-style vector design tool, egui 0.37 + wgpu 30.0.1) ported to HarmonyOS: ArkTS UI + C++ N-API bridge + Rust adaptation layer, wgpu GLES, dual-ABI .hap."
fi

# 2) Wire remotes and push the working tree (code, not the 129MB HAP) if the repo is a git repo.
if [[ -d "$ROOT/.git" ]]; then
  git -C "$ROOT" remote remove fork 2>/dev/null || true
  git -C "$ROOT" remote add fork "https://github.com/${REPO}.git"
  git -C "$ROOT" remote remove upstream 2>/dev/null || true
  git -C "$ROOT" remote add upstream "https://github.com/storytold/figcraft.git"
  BR="${1:-main}"
  git -C "$ROOT" add -A
  git -C "$ROOT" commit -m "port: FigCraft ${VER} HarmonyOS" || echo "(nothing to commit)"
  git -C "$ROOT" push -u fork "$BR"
fi

# 3) Build a sibling-style release body (Chinese, with version/bundleName/arch/bytes/MD5).
BYTES=$(stat -c '%s' "$UNSIGNED")
MD5=$(md5sum "$UNSIGNED" | awk '{print $1}')
NOTES="FigCraft ${VER} 鸿蒙 (HarmonyOS NEXT) 移植版。

storytold 的 Figma 式矢量设计工具（egui 0.37 + egui-wgpu + wgpu 30.0.1，storytold/egui
fork rev 6d518fc；winit fork rev a517b38；craft-libs rev ea9407b）移植到鸿蒙，Rust 内核
经 C++ N-API 桥接到 ArkTS UI，wgpu GLES 直接绘制，手机 / 平板 / PC 通用。

- versionName: ${VER}
- versionCode: 1801
- bundleName: \`io.github.shaliuxin.figcraft.hmos\`
- 架构: arm64-v8a + x86_64
- 文件: \`${APP}-${VER}-unsigned.hap\`
- 字节数: ${BYTES}
- MD5: \`${MD5}\`

未签名 HAP，供第三方用自有证书重签后安装；含 AGC 调试 profile 签名的
\`${APP}-${VER}-signed.hap\` 供直接真机调试。"

# 4) Create/update the release and attach the HAP assets.
gh release delete "$TAG" --repo "$REPO" --yes 2>/dev/null || true
gh release create "$TAG" --repo "$REPO" --title "${APP} ${VER}" --notes "$NOTES" \
  --prerelease "$UNSIGNED"
[[ -f "$SIGNED" ]] && gh release upload "$TAG" --repo "$REPO" "$SIGNED" --clobber
echo "Release ${TAG} published to ${REPO} with $(basename "$UNSIGNED") (${BYTES} bytes, MD5 ${MD5})"
