# Builds THIRD_PARTY_NOTICES.txt, the licence notices shipped next to
# slc-local-tts.exe: a header, then
# licenses\native.txt, then licenses\onnxruntime-third-party-notices.txt,
# then licenses\voice-models.txt, then the Rust crates
# that cargo-about lists from Cargo.lock.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/make-notices.ps1 [-OutFile <path>]
#
# Needs cargo-about at exactly $CargoAboutVersion (scripts\release-info.ps1):
#
#   cargo install cargo-about --version 0.9.2 --locked --features cli
#
# The output is UTF-8 without a BOM. It goes in target\notices\ by default and
# is never committed.
#
# This file must stay ASCII only: Windows PowerShell 5.1 reads non-BOM
# scripts as ANSI.

[CmdletBinding()]
param(
    [string] $OutFile
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'release-info.ps1')

$repoRoot   = Split-Path -Parent $PSScriptRoot
$noticesDir = Join-Path $repoRoot 'target\notices'
$cratesFile = Join-Path $noticesDir 'rust-crates.txt'
if (-not $OutFile) {
    $OutFile = Join-Path $noticesDir 'THIRD_PARTY_NOTICES.txt'
}

# cargo-about's output can change between versions, so only the pinned one
# may write the notices.
$aboutVersion = $null
try {
    $aboutVersion = (& cargo about --version 2>$null | Out-String).Trim()
} catch {
    $aboutVersion = $null
}
if ($aboutVersion -ne "cargo-about $CargoAboutVersion") {
    throw ("cargo-about $CargoAboutVersion is needed, but 'cargo about --version' printed '$aboutVersion'. " +
        "Install it with: cargo install cargo-about --version $CargoAboutVersion --locked --features cli")
}

New-Item -ItemType Directory -Force -Path $noticesDir | Out-Null

# cargo-about reads Cargo.toml from the current directory, so run it from the
# repo root. Push-Location -LiteralPath copes with the spaces and '@' in it.
Push-Location -LiteralPath $repoRoot
try {
    & cargo about generate --config about.toml --output-file $cratesFile about.hbs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo about generate failed (exit code $LASTEXITCODE). Its messages above name the crate and licence."
    }
} finally {
    Pop-Location
}

$header = @"
SLC Local TTS - third-party notices

SLC Local TTS is free software, licensed under the GNU General Public License,
version 3 or later (see LICENSE.txt). Its source code is at <$RepoUrl>.

It is built from the components below, each under its own licence.
"@

# Reads a UTF-8 file as text, with LF line endings, so the parts join cleanly.
function Read-Utf8Text([string] $Path) {
    $text = [System.IO.File]::ReadAllText($Path, [System.Text.Encoding]::UTF8)
    return ($text -replace "`r`n", "`n").TrimEnd("`n")
}

# onnxruntime's own ThirdPartyNotices.txt, kept byte for byte as upstream, so
# it goes in whole under its own heading at the end of the native section.
$onnxHeading = @"
ONNXRUNTIME'S BUNDLED THIRD-PARTY COMPONENTS

The notices for the components bundled inside onnxruntime 1.28.2, from its
ThirdPartyNotices.txt at
https://github.com/microsoft/onnxruntime/blob/v1.28.2/ThirdPartyNotices.txt
"@

$rule = '=' * 80
$parts = @(
    ($header -replace "`r`n", "`n"),
    (Read-Utf8Text (Join-Path $repoRoot 'licenses\native.txt')),
    (($onnxHeading -replace "`r`n", "`n") + "`n`n" +
        (Read-Utf8Text (Join-Path $repoRoot 'licenses\onnxruntime-third-party-notices.txt'))),
    (Read-Utf8Text (Join-Path $repoRoot 'licenses\voice-models.txt')),
    (Read-Utf8Text $cratesFile)
)
$text = ($parts -join "`n`n$rule`n`n") + "`n"

$outDir = Split-Path -Parent $OutFile
if ($outDir) {
    New-Item -ItemType Directory -Force -Path $outDir | Out-Null
}
# .NET resolves relative paths against the process directory, not PowerShell's
# current location, so make the path absolute first.
$OutFile = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutFile)
[System.IO.File]::WriteAllText($OutFile, $text, (New-Object System.Text.UTF8Encoding $false))

$size = (Get-Item -LiteralPath $OutFile).Length
Write-Host "OK  $OutFile ($size bytes)"
