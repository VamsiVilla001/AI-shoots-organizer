<#
.SYNOPSIS
Points this machine at a SKWAD library database, and checks that it works.

.DESCRIPTION
Writes the two files the app reads at startup and verifies the server is
actually reachable before you launch anything:

  %APPDATA%\com.skwad.mediaorganiser\database.json   which server to use
  %APPDATA%\postgresql\pgpass.conf                   the password for it

They are separate because the library folder can be a network share, and a
password sitting in a shared folder is a password everyone has. The password
file is always local to this machine and this user.

No administrator rights needed: both files live in your own profile.

.EXAMPLE
  .\connect-to-library.ps1 -Server 192.168.1.229 -Password skwad_dev

.EXAMPLE
  # Check what this machine is currently configured for, changing nothing.
  .\connect-to-library.ps1 -Check
#>
[CmdletBinding(DefaultParameterSetName = 'Set')]
param(
    [Parameter(ParameterSetName = 'Set', Mandatory = $true)]
    [string] $Server,

    [Parameter(ParameterSetName = 'Set')]
    [string] $Password,

    [Parameter(ParameterSetName = 'Set')]
    [int] $Port = 5432,

    [Parameter(ParameterSetName = 'Set')]
    [string] $Database = 'skwad',

    [Parameter(ParameterSetName = 'Set')]
    [string] $User = 'skwad',

    [Parameter(ParameterSetName = 'Check')]
    [switch] $Check
)

$ErrorActionPreference = 'Stop'
$libraryDir = Join-Path $env:APPDATA 'com.skwad.mediaorganiser'
$configPath = Join-Path $libraryDir 'database.json'
$pgpassDir  = Join-Path $env:APPDATA 'postgresql'
$pgpassPath = Join-Path $pgpassDir 'pgpass.conf'

function Show-Current {
    Write-Host "`nThis machine is currently configured as:" -ForegroundColor Cyan
    if (Test-Path $configPath) {
        $c = Get-Content $configPath -Raw | ConvertFrom-Json
        Write-Host "  server   : $($c.user)@$($c.host):$($c.port)/$($c.database)"
        Write-Host "  from     : $configPath"
    } else {
        Write-Host "  server   : (not configured - the app will try localhost:5432)" -ForegroundColor Yellow
        Write-Host "  expected : $configPath"
    }
    if (Test-Path $pgpassPath) {
        $lines = @(Get-Content $pgpassPath | Where-Object { $_.Trim() -and -not $_.StartsWith('#') })
        Write-Host "  passwords: $($lines.Count) entr$(if($lines.Count -eq 1){'y'}else{'ies'}) in $pgpassPath"
        foreach ($l in $lines) {
            $f = $l -split ':'
            if ($f.Count -ge 5) { Write-Host "             - $($f[3])@$($f[0]):$($f[1])/$($f[2])" }
        }
    } else {
        Write-Host "  passwords: (none) - expected $pgpassPath" -ForegroundColor Yellow
    }
}

if ($Check) { Show-Current; return }

if (-not $Password) {
    $secure = Read-Host "Password for $User@${Server}:$Port" -AsSecureString
    $Password = [Runtime.InteropServices.Marshal]::PtrToStringAuto(
        [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure))
}

# --- reachability first ------------------------------------------------------
# Writing config for a server that cannot be reached just moves the failure to
# launch time, where it is harder to read.
Write-Host "`nChecking $Server`:$Port ..." -NoNewline
$probe = Test-NetConnection -ComputerName $Server -Port $Port -WarningAction SilentlyContinue
if (-not $probe.TcpTestSucceeded) {
    Write-Host " unreachable" -ForegroundColor Red
    Write-Host @"

Nothing is answering on $Server`:$Port. On the machine hosting the library:
  1. is it on, and is its PostgreSQL service running?
  2. does its firewall allow TCP $Port from this machine?
  3. does its pg_hba.conf permit this machine's address?

Nothing was written.
"@ -ForegroundColor Yellow
    exit 1
}
Write-Host " reachable" -ForegroundColor Green

# --- database.json -----------------------------------------------------------
New-Item -ItemType Directory -Path $libraryDir -Force | Out-Null
[ordered]@{
    host               = $Server
    port               = $Port
    database           = $Database
    user               = $User
    maxConnections     = 8
    connectTimeoutSecs = 10
} | ConvertTo-Json | Set-Content $configPath -Encoding utf8
Write-Host "wrote $configPath"

# --- pgpass.conf -------------------------------------------------------------
# Replace any existing line for this exact server/database/user rather than
# appending: a stale password for the same target would win or conflict
# depending on ordering, which is a miserable thing to debug.
New-Item -ItemType Directory -Path $pgpassDir -Force | Out-Null
$entry = "${Server}:${Port}:${Database}:${User}:${Password}"
$kept  = @()
if (Test-Path $pgpassPath) {
    $kept = @(Get-Content $pgpassPath | Where-Object {
        $_.Trim() -and -not $_.StartsWith("${Server}:${Port}:${Database}:${User}:")
    })
}
($kept + $entry) | Set-Content $pgpassPath -Encoding ascii
Write-Host "wrote $pgpassPath"

Show-Current
Write-Host "`nDone. Open SKWAD." -ForegroundColor Green
