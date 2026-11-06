# Fetches the pinned, sha256-verified sherpa-onnx and piper-phonemize static
# libraries into <repo>\.native, where .cargo/config.toml points the -sys
# build scripts. Safe to re-run: verified archives are skipped.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/fetch-native-libs.ps1

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Get-VerifiedArchive.ps1')

$repoRoot  = Split-Path -Parent $PSScriptRoot
$nativeDir = Join-Path $repoRoot '.native'

$archives = @(
    @{
        Url    = 'https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.1/sherpa-onnx-v2.0.1-win-x64-static-MT-Release-lib.tar.bz2'
        Size   = 122863596
        Sha256 = '23e7b986b197eda0bbc47ff3d7d5c87afd6ef2b9077214ba26b0f1ec52ef0e66'
    },
    @{
        Url    = 'https://github.com/csukuangfj/piper-phonemize/releases/download/v1.5.9/piper-phonemize-v1.5.9-windows-x64-static-lib.tar.bz2'
        Size   = 464899
        Sha256 = '7c69b30f6ce0715905c323c613735db84337ae0387436a038c6efe1a5738e388'
    }
)

foreach ($archive in $archives) {
    $dir = Get-VerifiedArchive @archive -DestDir $nativeDir
    Write-Host "OK  $dir"
}
