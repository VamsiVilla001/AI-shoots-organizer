$ErrorActionPreference = 'Stop'

$version = '4.13.0'
$expectedSha256 = 'F0E98C302464D6860777A7015065E11B9B271B5394E6BA92663F0CF1FC303F2C'
$repositoryRoot = Split-Path -Parent $PSScriptRoot
$dependencyRoot = Join-Path $repositoryRoot '.opencv'
$archive = Join-Path $dependencyRoot "opencv-$version-windows.exe"
$sdk = Join-Path $dependencyRoot 'sdk'
$buildRoot = Join-Path $sdk 'opencv\build'

New-Item -ItemType Directory -Force -Path $dependencyRoot | Out-Null
if (-not (Test-Path -LiteralPath $archive)) {
    Invoke-WebRequest `
        -Uri "https://github.com/opencv/opencv/releases/download/$version/opencv-$version-windows.exe" `
        -OutFile $archive
}

$actualSha256 = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
if ($actualSha256 -ne $expectedSha256) {
    throw "OpenCV download checksum mismatch. Expected $expectedSha256 but received $actualSha256"
}

if (-not (Test-Path -LiteralPath (Join-Path $buildRoot 'include\opencv2\core.hpp'))) {
    & $archive "-o$sdk" -y
    if ($LASTEXITCODE -ne 0) {
        throw "OpenCV SDK extraction failed with exit code $LASTEXITCODE"
    }
}

Write-Host "OpenCV $version is ready at $buildRoot"
Write-Host 'Build the prototype with: cargo test -p teo-video-analysis --features opencv-tracking'
