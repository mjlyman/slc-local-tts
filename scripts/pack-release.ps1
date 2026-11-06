# Builds the release and packs it with Velopack:
# Setup.exe, the full .nupkg and releases.win.json.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/pack-release.ps1 [-OutputDir <path>]
#
# The output goes in target\velopack\Releases\ unless -OutputDir is given.
#
# A local test feed: pack into a folder with -OutputDir <folder>, then
# start the installed app with SLC_LOCAL_TTS_UPDATE_FEED=<folder> so its update
# check reads that folder instead of GitHub.
#
# vpk won't repack a version that is already in the output folder. To repack
# the same version, delete that folder (or use a new one) first.
#
# Needs vpk at exactly $VpkVersion and cargo-about at exactly
# $CargoAboutVersion (scripts\release-info.ps1):
#
#   dotnet tool install -g vpk --version 1.2.161
#   cargo install cargo-about --version 0.9.2 --locked --features cli
#
# Everything it writes is under target\ (or -OutputDir) and is never committed.
# It stops at the first step that fails.
#
# This file must stay ASCII only: Windows PowerShell 5.1 reads non-BOM
# scripts as ANSI.

[CmdletBinding()]
param(
    [string] $OutputDir
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'release-info.ps1')

$repoRoot  = Split-Path -Parent $PSScriptRoot
$packDir   = Join-Path $repoRoot 'target\pack'
$filesDir  = Join-Path $packDir 'files'
$notesFile = Join-Path $packDir 'release-notes.md'
$exeName   = 'slc-local-tts.exe'
if (-not $OutputDir) {
    $OutputDir = Join-Path $repoRoot 'target\velopack\Releases'
}
# vpk runs as its own process, so give it an absolute path.
$OutputDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDir)
# A trailing backslash would escape the closing quote when the path is passed
# to vpk ("C:\my feed\" becomes C:\my feed"), so drop it.
$OutputDir = $OutputDir.TrimEnd('\')

# Step 1: the pinned vpk. vpk 1.2.161 has no --version flag, so read the
# version from the first description line of 'vpk -h', which reads
# "Velopack CLI 1.2.161, for distributing applications."
$vpkInstallHint = "Install it with: dotnet tool install -g vpk --version $VpkVersion " +
    "(it needs the .NET 8 SDK or newer), then open a new terminal so vpk is on the PATH."
if (-not (Get-Command vpk -ErrorAction SilentlyContinue)) {
    throw "vpk $VpkVersion is needed, but no 'vpk' command was found. $vpkInstallHint"
}
# Under 'Stop', Windows PowerShell 5.1 turns a redirected native stderr line
# into an error, so run vpk inside a script block with its own 'Continue'.
# The script block's variable is local to it and the setting outside is kept.
$vpkHelp = & {
    $ErrorActionPreference = 'Continue'
    (& vpk -h 2>$null | Out-String)
}
if (-not $vpkHelp) {
    $vpkHelp = ''
}
$vpkFound = 'nothing recognisable'
if ($vpkHelp -match 'Velopack CLI (\S+), for distributing applications') {
    $vpkFound = $Matches[1]
}
if ($vpkFound -ne $VpkVersion) {
    throw "vpk $VpkVersion is needed, but 'vpk -h' reports $vpkFound. $vpkInstallHint"
}

# cargo reads Cargo.toml from the current directory, so run it from the repo
# root. Push-Location -LiteralPath copes with the spaces and '@' in it.
Push-Location -LiteralPath $repoRoot
try {
    # Step 2: the version, from Cargo.toml through cargo itself.
    $metadataJson = (& cargo metadata --no-deps --format-version 1 | Out-String)
    if ($LASTEXITCODE -ne 0) {
        throw "cargo metadata failed (exit code $LASTEXITCODE). Its messages above say what is wrong with Cargo.toml."
    }
    $package = (ConvertFrom-Json $metadataJson).packages |
        Where-Object { $_.name -eq 'slc-local-tts' } |
        Select-Object -First 1
    if (-not $package) {
        throw "cargo metadata did not list the slc-local-tts package. Run this script from a checkout of the repository."
    }
    $version = [string] $package.version
    Write-Host "Packing SLC Local TTS $version"

    # Step 3: the release build. --locked fails if Cargo.lock is out of date
    # instead of quietly changing it.
    & cargo build --release --locked
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build --release --locked failed (exit code $LASTEXITCODE). Its messages above say why. If it says Cargo.lock needs updating, run 'cargo build' and commit Cargo.lock."
    }
} finally {
    Pop-Location
}

# Step 4: the third-party notices. make-notices.ps1 throws on any failure,
# which stops this script too.
$noticesFile = Join-Path $repoRoot 'target\notices\THIRD_PARTY_NOTICES.txt'
& (Join-Path $PSScriptRoot 'make-notices.ps1') -OutFile $noticesFile

# Step 5: the staging folder, recreated so it holds exactly the three files
# that get installed.
if (Test-Path -LiteralPath $filesDir) {
    Remove-Item -LiteralPath $filesDir -Recurse -Force
}
[System.IO.Directory]::CreateDirectory($filesDir) | Out-Null
Copy-Item -LiteralPath (Join-Path $repoRoot "target\release\$exeName") -Destination (Join-Path $filesDir $exeName)
Copy-Item -LiteralPath (Join-Path $repoRoot 'LICENSE') -Destination (Join-Path $filesDir 'LICENSE.txt')
Copy-Item -LiteralPath $noticesFile -Destination (Join-Path $filesDir 'THIRD_PARTY_NOTICES.txt')

# Step 6: the release notes, UTF-8 without a BOM, with LF line endings.
$notes = @(
    "SLC Local TTS $version",
    "",
    "To install, download SLC-Local-TTS-win-Setup.exe below and run it. If Windows SmartScreen stops it, click `"More info`", then `"Run anyway`". An installed copy updates itself.",
    "",
    "Source code for this version: $RepoUrl/tree/v$version",
    "Licence: GPL-3.0-or-later. The third-party notices are in THIRD_PARTY_NOTICES.txt, installed next to the program."
) -join "`n"
[System.IO.File]::WriteAllText($notesFile, $notes + "`n", (New-Object System.Text.UTF8Encoding $false))

# Step 7: the Velopack package. Full packages only (--delta None), for x64,
# with no portable zip, and a Start menu shortcut but no desktop one. vpk
# defaults to delta packages, which an installed copy would update by patch.
[System.IO.Directory]::CreateDirectory($OutputDir) | Out-Null
& vpk pack `
    --packId $PackId `
    --packVersion $version `
    --packTitle $PackTitle `
    --packAuthors $PackAuthors `
    --packDir $filesDir `
    --mainExe $exeName `
    --shortcuts StartMenuRoot `
    --noPortable `
    --delta None `
    --runtime win-x64 `
    --releaseNotes $notesFile `
    --outputDir $OutputDir
if ($LASTEXITCODE -ne 0) {
    throw "vpk pack failed (exit code $LASTEXITCODE). Its messages above say why."
}

# Step 8: where the installer is.
$setupExe = Join-Path $OutputDir "$PackId-win-Setup.exe"
if (-not (Test-Path -LiteralPath $setupExe)) {
    throw "vpk pack finished, but $setupExe is missing. Look in $OutputDir for what it made."
}
Write-Host "OK  $setupExe"
