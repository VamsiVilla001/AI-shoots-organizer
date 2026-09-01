#!/usr/bin/env bash
# Build a self-contained Apple Silicon or Intel macOS app and DMG.
#
# Face models are downloaded and bundled so a new install can analyse shoots
# immediately. FFmpeg remains an optional runtime dependency; Finder-launched
# builds discover Homebrew and MacPorts installations automatically.
set -euo pipefail

cd "$(dirname "$0")/.."

fail() { echo "error: $*" >&2; exit 1; }

echo "==> Checking macOS build prerequisites"
[[ "$(uname -s)" == "Darwin" ]] || fail "this package must be built on macOS"
command -v cargo >/dev/null || fail "Rust is not installed — install it from https://rustup.rs"
command -v node >/dev/null || fail "Node 20+ is not installed — install it from https://nodejs.org"
command -v codesign >/dev/null || fail "codesign is missing — install the Xcode Command Line Tools"
xcode-select -p >/dev/null 2>&1 || fail "Xcode Command Line Tools are missing — run: xcode-select --install"

node_major="$(node -p 'process.versions.node.split(".")[0]')"
[[ "${node_major}" -ge 20 ]] || fail "Node 20+ required, found $(node -v)"

arch="$(uname -m)"
case "${arch}" in
  arm64) rust_target="aarch64-apple-darwin" ;;
  x86_64) rust_target="x86_64-apple-darwin" ;;
  *) fail "unsupported Mac architecture: ${arch}" ;;
esac

if command -v ffmpeg >/dev/null; then
  echo "    $(ffmpeg -version | head -n 1)"
else
  echo "    FFmpeg not found; install with: brew install ffmpeg"
fi

echo "==> Installing JavaScript dependencies"
npm ci

echo "==> Fetching face models (skipped when already present)"
bash scripts/fetch-models.sh
for model in det_10g.onnx w600k_r50.onnx; do
  [[ -f "models/${model}" ]] || fail "models/${model} was not downloaded"
done

config_args=(--config src-tauri/tauri.models.conf.json)
if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  echo "==> Developer ID signing: ${APPLE_SIGNING_IDENTITY}"
else
  echo "==> Applying a complete ad-hoc signature"
  config_args+=(--config src-tauri/tauri.adhoc.conf.json)
fi

echo "==> Building ${rust_target} app and DMG"
npm run tauri:build -w @teo/desktop -- \
  --ci \
  --target "${rust_target}" \
  --bundles app,dmg \
  "${config_args[@]}"

bundle_root="target/${rust_target}/release/bundle"
app_bundle="$(find "${bundle_root}/macos" -maxdepth 1 -name '*.app' -print -quit)"
dmg_bundle="$(find "${bundle_root}/dmg" -maxdepth 1 -name '*.dmg' -print -quit)"
[[ -n "${app_bundle}" ]] || fail "the macOS app bundle was not produced"
[[ -n "${dmg_bundle}" ]] || fail "the macOS DMG was not produced"

echo "==> Verifying application signature and disk image"
codesign --verify --deep --strict --verbose=2 "${app_bundle}"
hdiutil verify "${dmg_bundle}" >/dev/null

echo
echo "macOS package ready:"
echo "  ${app_bundle}"
echo "  ${dmg_bundle}"

if [[ -z "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  cat <<'NOTE'

This build is ad-hoc signed, not notarised. On another Mac, right-click the
application and choose Open on first launch. Developer ID signing and
notarisation variables are documented in docs/deployment.md.
NOTE
fi
