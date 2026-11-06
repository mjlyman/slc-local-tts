# The release values shared by the packaging scripts. Dot-source it:
#
#   . (Join-Path $PSScriptRoot 'release-info.ps1')
#
# tests/notices.rs checks $RepoUrl and $PackId against the code, so a change
# here that the code doesn't match fails `cargo test`.
#
# This file must stay ASCII only: Windows PowerShell 5.1 reads non-BOM
# scripts as ANSI.

# The GitHub repository the update check reads releases from (update::REPO_URL).
$RepoUrl = "https://github.com/mjlyman/slc-local-tts"

# Velopack package identity. The pack ID is also the install folder's
# name under %LocalAppData%.
$PackId = "SLC-Local-TTS"
$PackTitle = "SLC Local TTS"
$PackAuthors = "Matthew Lyman"

# Pinned tool versions.
$VpkVersion = "1.2.161"
$CargoAboutVersion = "0.9.2"
