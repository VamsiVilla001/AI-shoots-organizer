# Downloads the face-detection and recognition models (Windows).
#
# The application ships without model weights; this script fetches the
# InsightFace "buffalo_l" pack and extracts the two ONNX files the pipeline
# needs:
#
#   det_10g.onnx    - SCRFD-10G face detector (with landmarks)
#   w600k_r50.onnx  - ArcFace-compatible ResNet-50 recognition model
#
# They are placed in the repository's models/ folder and, if it exists, in the
# installed application's data folder so a packaged build picks them up too.
#
#   powershell -ExecutionPolicy Bypass -File scripts/fetch-models.ps1

$ErrorActionPreference = 'Stop'

$repoModels = Join-Path $PSScriptRoot '..\models' | Resolve-Path
$appModels = Join-Path $env:APPDATA 'com.skwad.mediaorganiser\models'
$zipUrl = 'https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip'
$wanted = @('det_10g.onnx', 'w600k_r50.onnx')

$existing = $wanted | Where-Object { Test-Path (Join-Path $repoModels $_) }
if ($existing.Count -eq $wanted.Count) {
    Write-Host "Models already present in $repoModels"
} else {
    $temp = Join-Path ([System.IO.Path]::GetTempPath()) "skwad-models-$PID"
    New-Item -ItemType Directory -Force -Path $temp | Out-Null
    $zip = Join-Path $temp 'buffalo_l.zip'

    Write-Host "Downloading buffalo_l.zip (~280 MB)..."
    Invoke-WebRequest -Uri $zipUrl -OutFile $zip

    Write-Host "Extracting..."
    Expand-Archive -Path $zip -DestinationPath $temp -Force

    foreach ($name in $wanted) {
        $found = Get-ChildItem -Path $temp -Recurse -Filter $name | Select-Object -First 1
        if (-not $found) { throw "Expected $name inside the archive but it was not there." }
        Copy-Item $found.FullName (Join-Path $repoModels $name) -Force
        Write-Host "  models\$name"
    }

    Remove-Item -Recurse -Force $temp
}

# Mirror into the installed app's data directory when it exists.
if (Test-Path (Split-Path $appModels)) {
    New-Item -ItemType Directory -Force -Path $appModels | Out-Null
    foreach ($name in $wanted) {
        Copy-Item (Join-Path $repoModels $name) (Join-Path $appModels $name) -Force
    }
    Write-Host "Copied models into $appModels"
} else {
    Write-Host "App data folder not found yet - run the app once, then re-run this script,"
    Write-Host "or copy models\*.onnx into %APPDATA%\com.skwad.mediaorganiser\models manually."
}

Write-Host "Done."
