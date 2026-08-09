#!/usr/bin/env bash
# Downloads the face-detection and recognition models (macOS / Linux).
# See fetch-models.ps1 for the Windows equivalent and full notes.
set -euo pipefail

repo_models="$(cd "$(dirname "$0")/../models" && pwd)"
app_models="${HOME}/Library/Application Support/com.teorganiser.desktop/models"
zip_url="https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip"
wanted=(det_10g.onnx w600k_r50.onnx)

missing=0
for name in "${wanted[@]}"; do
  [[ -f "${repo_models}/${name}" ]] || missing=1
done

if [[ $missing -eq 0 ]]; then
  echo "Models already present in ${repo_models}"
else
  temp="$(mktemp -d)"
  trap 'rm -rf "${temp}"' EXIT

  echo "Downloading buffalo_l.zip (~280 MB)..."
  curl -L --fail -o "${temp}/buffalo_l.zip" "${zip_url}"

  echo "Extracting..."
  unzip -q "${temp}/buffalo_l.zip" -d "${temp}"

  for name in "${wanted[@]}"; do
    found="$(find "${temp}" -name "${name}" | head -n 1)"
    [[ -n "${found}" ]] || { echo "Expected ${name} inside the archive." >&2; exit 1; }
    cp "${found}" "${repo_models}/${name}"
    echo "  models/${name}"
  done
fi

if [[ -d "$(dirname "${app_models}")" ]]; then
  mkdir -p "${app_models}"
  for name in "${wanted[@]}"; do
    cp "${repo_models}/${name}" "${app_models}/${name}"
  done
  echo "Copied models into ${app_models}"
else
  echo "App data folder not found yet — run the app once, then re-run this script."
fi

echo "Done."
