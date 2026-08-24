#!/usr/bin/env bash
# Builds the macOS app on an Apple Silicon Mac (Mac Studio, MacBook M-series).
#
# Produces a .dmg with the face models bundled inside, so the person who
# installs it does not have to fetch anything. Run from the repository root:
#
#   bash scripts/build-macos.sh
#
# Signing is picked up from the environment when present — see
# docs/deployment.md. Without it the build still succeeds and the result is
# unsigned.
set -euo pipefail

cd "$(dirname "$0")/.."

fail() { echo "error: $*" >&2; exit 1; }

echo "==> Checking prerequisites"

[[ "$(uname -s)" == "Darwin" ]] || fail "this script builds the macOS app and has to run on macOS"

arch="$(uname -m)"
if [[ "${arch}" != "arm64" ]]; then
  echo "    note: this Mac is ${arch}, so the build targets Intel, not Apple Silicon."
fi

command -v cargo >/dev/null || fail "Rust is not installed — https://rustup.rs"
command -v node  >/dev/null || fail "Node 20+ is not installed — https://nodejs.org"
xcode-select -p >/dev/null 2>&1 || fail "Xcode Command Line Tools are missing — run: xcode-select --install"

node_major="$(node -p 'process.versions.node.split(".")[0]')"
[[ "${node_major}" -ge 20 ]] || fail "Node 20+ required, found $(node -v)"

# FFmpeg is a runtime dependency, not a build one: without it the app still
# handles JPEG and PNG, and says so on the Settings screen.
if command -v ffmpeg >/dev/null; then
  echo "    ffmpeg: $(ffmpeg -version | head -n 1)"
else
  echo "    ffmpeg: not found. Videos, HEIC and camera raw need it (brew install ffmpeg)."
fi

echo "==> Installing npm dependencies"
npm ci

echo "==> Fetching the face models (~280 MB, skipped when already present)"
bash scripts/fetch-models.sh

models_present=0
for name in det_10g.onnx w600k_r50.onnx; do
  [[ -f "models/${name}" ]] && models_present=$((models_present + 1))
done

# The models config is a glob over models/*.onnx, and Tauri treats a glob that
# matches nothing as a build error — so only add it when they are really there.
config_args=()
if [[ "${models_present}" -eq 2 ]]; then
  echo "    models will be bundled into the app"
  config_args=(--config src-tauri/tauri.models.conf.json)
else
  echo "    building without bundled models; the app will ask for them on first run"
fi

target_args=()
if [[ "${arch}" == "arm64" ]]; then
  target_args=(--target aarch64-apple-darwin)
fi

# The desktop app is a client of `teo-server`: the window, a log and a
# supervisor, with every command living in the server. Ship the app without the
# sidecar and it opens, reports that the local server did not start, and can do
# nothing — so this is part of building it, not an extra.
# macOS ships bash 3.2, where "set -u" treats expanding an empty array as an
# unbound variable — which is what happens on an Intel Mac, where no --target is
# passed. The ${a[@]+...} form expands to nothing instead of failing.
echo "==> Building the server sidecar"
cargo build --release -p teo-server ${target_args[@]+"${target_args[@]}"}
node scripts/stage-sidecar.mjs
config_args+=(--config src-tauri/tauri.sidecar.conf.json)

if [[ -n "${APPLE_SIGNING_IDENTITY:-}" ]]; then
  echo "==> Signing as ${APPLE_SIGNING_IDENTITY}"
else
  echo "==> Building unsigned (set APPLE_SIGNING_IDENTITY to sign; see docs/deployment.md)"
fi

echo "==> Building"
npm run tauri:build -w @teo/desktop -- ${target_args[@]+"${target_args[@]}"} "${config_args[@]}"

echo
echo "==> Done. Installers:"
find target -path '*release/bundle/dmg/*.dmg' -o -path '*release/bundle/macos/*.app' | sed 's/^/    /'

cat <<'NOTE'

If the .dmg is unsigned, the first launch on someone else's Mac needs one of:
  * right-click the app in Applications and choose Open, then Open again, or
  * xattr -dr com.apple.quarantine "/Applications/Esports AI Media Organiser.app"

A signed and notarised build needs neither. See docs/deployment.md.
NOTE
