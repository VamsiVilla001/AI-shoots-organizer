<#
.SYNOPSIS
    Copies a built SKWAD installer into the Apache document root and rebuilds
    the download page the team uses.

.DESCRIPTION
    SKWAD is a desktop application, so Apache only ever hands out the installer
    file - the app itself runs on each workstation and reaches the shared
    library over a Windows file share, not over HTTP. There is no auto-update:
    people download the new installer from this page and run it over the top of
    the old one.

.PARAMETER Destination
    The Apache document root for the downloads, e.g. C:\Apache24\htdocs\skwad.

.PARAMETER Installer
    The installer to publish. Defaults to the newest .exe that `npm run build`
    left in target\release\bundle\nsis.

.PARAMETER LibraryPath
    The UNC path of the shared library, printed on the page so people can set
    it up straight after installing, e.g. \\STUDIO-PC\skwad-library.

.PARAMETER Keep
    How many past versions to leave on the server. Default 3.

.EXAMPLE
    pwsh scripts\publish-installer.ps1 -Destination C:\Apache24\htdocs\skwad `
        -LibraryPath \\STUDIO-PC\skwad-library
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Destination,
    [string]$Installer,
    [string]$LibraryPath,
    [int]$Keep = 3
)

$ErrorActionPreference = 'Stop'
$repository = Split-Path -Parent $PSScriptRoot

if (-not $Installer) {
    $bundle = Join-Path $repository 'target\release\bundle\nsis'
    if (-not (Test-Path $bundle)) {
        throw "No installer found. Run 'npm run build' first, or pass -Installer."
    }
    $newest = Get-ChildItem -Path $bundle -Filter *.exe |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if (-not $newest) { throw "No .exe in $bundle. Run 'npm run build' first." }
    $Installer = $newest.FullName
}
if (-not (Test-Path $Installer)) { throw "Installer not found: $Installer" }

if (-not (Test-Path $Destination)) {
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
}

$source = Get-Item $Installer
$target = Join-Path $Destination $source.Name
Write-Host "Publishing $($source.Name) ($([math]::Round($source.Length / 1MB, 1)) MB)"
Copy-Item -Path $source.FullName -Destination $target -Force

# A checksum beside each installer, so anyone can confirm the download is
# intact before running it.
$hash = (Get-FileHash -Path $target -Algorithm SHA256).Hash
Set-Content -Path "$target.sha256" -Value "$hash  $($source.Name)" -Encoding ascii

# Retire older builds, newest first, keeping the current one.
$installers = Get-ChildItem -Path $Destination -Filter *.exe | Sort-Object LastWriteTime -Descending
if ($installers.Count -gt $Keep) {
    foreach ($old in $installers | Select-Object -Skip $Keep) {
        Write-Host "Removing old build $($old.Name)"
        Remove-Item $old.FullName -Force -Confirm:$false
        if (Test-Path "$($old.FullName).sha256") {
            Remove-Item "$($old.FullName).sha256" -Force -Confirm:$false
        }
    }
    $installers = Get-ChildItem -Path $Destination -Filter *.exe | Sort-Object LastWriteTime -Descending
}

function Format-Html([string]$text) {
    if ($null -eq $text) { return '' }
    return [System.Net.WebUtility]::HtmlEncode($text)
}

$rows = ''
$first = $true
foreach ($item in $installers) {
    $size = "{0:N1} MB" -f ($item.Length / 1MB)
    $when = $item.LastWriteTime.ToString('d MMMM yyyy')
    $sum = ''
    if (Test-Path "$($item.FullName).sha256") {
        $sum = (Get-Content "$($item.FullName).sha256" -Raw).Split(' ')[0].Trim()
    }
    $tag = ''
    if ($first) { $tag = '<span class="current">current</span>' }
    $name = Format-Html $item.Name
    # Installer names contain spaces, so the link needs them percent-encoded.
    $href = Format-Html ([System.Uri]::EscapeDataString($item.Name))
    $rows += @"
      <li>
        <a class="download" href="$href" download>$name</a> $tag
        <span class="meta">$size &middot; published $when</span>
        <code class="sum">SHA-256 $sum</code>
      </li>

"@
    $first = $false
}

$libraryBlock = ''
if ($LibraryPath) {
    $library = Format-Html $LibraryPath
    $libraryBlock = @"
      <li>Open <strong>Settings &rarr; Team library location</strong>, enter
          <code>$library</code> and save.</li>
      <li>Restart SKWAD when it asks. You are now on the shared library.</li>
"@
} else {
    $libraryBlock = @"
      <li>Open <strong>Settings &rarr; Team library location</strong> and enter the
          shared library path your administrator gave you, then restart.</li>
"@
}

$generated = (Get-Date).ToString('d MMMM yyyy, HH:mm')
$page = @"
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>SKWAD Media Organiser &mdash; download</title>
<style>
  :root { color-scheme: light dark; }
  body { margin: 0; padding: 48px 24px; font: 15px/1.6 system-ui, sans-serif;
         background: #faf8f5; color: #23201c; }
  @media (prefers-color-scheme: dark) { body { background: #17150f; color: #ece7dd; } }
  main { max-width: 680px; margin: 0 auto; }
  h1 { font-size: 24px; letter-spacing: 0.02em; margin: 0 0 4px; }
  p.lead { margin: 0 0 32px; opacity: 0.75; }
  ul { list-style: none; padding: 0; }
  li { padding: 14px 0; border-top: 1px solid rgba(128,128,128,0.3); }
  a.download { font-weight: 700; font-size: 16px; }
  .current { font-size: 11px; font-weight: 700; text-transform: uppercase;
             letter-spacing: 0.06em; border: 1px solid currentColor;
             border-radius: 999px; padding: 1px 8px; margin-left: 6px; }
  .meta { display: block; font-size: 13px; opacity: 0.7; }
  .sum { display: block; font-size: 11px; opacity: 0.55; word-break: break-all; }
  ol { padding-left: 20px; }
  footer { margin-top: 40px; font-size: 12px; opacity: 0.6; }
</style>
</head>
<body>
<main>
  <h1>SKWAD Media Organiser</h1>
  <p class="lead">Windows installer for the team. Run it over your existing
     install &mdash; your library and settings are left alone.</p>

  <ul>
$rows
  </ul>

  <h2>After installing</h2>
  <ol>
$libraryBlock
    <li>Sign in with your work email and the password you were given.</li>
  </ol>

  <p>Windows may warn that the publisher is unknown. Choose
     <strong>More info &rarr; Run anyway</strong> &mdash; the installer is unsigned,
     and it came from this server on your own network.</p>

  <footer>Page rebuilt $generated.</footer>
</main>
</body>
</html>
"@

$indexPath = Join-Path $Destination 'index.html'
Set-Content -Path $indexPath -Value $page -Encoding utf8

Write-Host ""
Write-Host "Published to $Destination"
Write-Host "  installer  $target"
Write-Host "  checksum   $hash"
Write-Host "  page       $indexPath"
Write-Host ""
Write-Host "Send the team the URL Apache serves that folder on."
