# Generates src/catalog/data.rs, the compiled-in voice catalog, from
# scripts/catalog-piper.csv plus the fixed Kokoro and Supertonic voice lists.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\gen-catalog.ps1
#
# Needs an authenticated GitHub CLI (`gh`) and HTTPS access to Hugging Face.
# Archive sizes and sha256 digests come from the sherpa-onnx `tts-models`
# release; each Piper voice's espeak voice comes from its .onnx.json on
# rhasspy/piper-voices. The output is UTF-8 without BOM, LF line endings,
# already rustfmt-clean, and identical across runs given the same inputs.
#
# Fails loudly (non-zero exit) if an archive asset is missing, a digest is not
# sha256, an espeak voice is `en-gb` or otherwise outside {en-us, en,
# en-gb-x-rp}, or the Kokoro/Supertonic assets differ from the pinned values.
#
# This file must stay ASCII only: Windows PowerShell 5.1 reads non-BOM
# scripts as ANSI.

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repoRoot = Split-Path -Parent $PSScriptRoot
$csvPath  = Join-Path $PSScriptRoot 'catalog-piper.csv'
$outPath  = Join-Path $repoRoot 'src\catalog\data.rs'

$repo         = 'k2-fsa/sherpa-onnx'
$releaseTag   = 'tts-models'
$modelUrlBase = "https://github.com/$repo/releases/download/$releaseTag/"
$hfBase       = 'https://huggingface.co/rhasspy/piper-voices/resolve/main/en'
$allowedEspeak = @('en-us', 'en', 'en-gb-x-rp')

# Kokoro (fp32) and Supertonic archives are pinned. A mismatch
# means the asset was re-uploaded and must be re-measured before shipping.
$pinned = @{
    'kokoro-multi-lang-v1_0' = @{
        Size   = 349886476
        Sha256 = '60703707bc05ccd48d60e1eb723f38b07d6e474795a0b6a588d7220400e0f7a4'
    }
    'sherpa-onnx-supertonic-3-tts-int8-2026-05-11' = @{
        Size   = 128774318
        Sha256 = '82fa96f91c4ef8abaae3a14a3f4153facf88bed821d1f7331cec2700f432c427'
    }
}
$kokoroStem     = 'kokoro-multi-lang-v1_0'
$supertonicStem = 'sherpa-onnx-supertonic-3-tts-int8-2026-05-11'

# Kokoro v1.0 English speakers, in sid order (0..27).
$kokoroSpeakers = @(
    'af_alloy', 'af_aoede', 'af_bella', 'af_heart', 'af_jessica', 'af_kore',
    'af_nicole', 'af_nova', 'af_river', 'af_sarah', 'af_sky',
    'am_adam', 'am_echo', 'am_eric', 'am_fenrir', 'am_liam', 'am_michael',
    'am_onyx', 'am_puck', 'am_santa',
    'bf_alice', 'bf_emma', 'bf_isabella', 'bf_lily',
    'bm_daniel', 'bm_fable', 'bm_george', 'bm_lewis'
)

function Invoke-Gh {
    param([string[]]$GhArgs)
    $out = & gh @GhArgs
    if ($LASTEXITCODE -ne 0) {
        throw "gh $($GhArgs -join ' ') failed with exit code $LASTEXITCODE"
    }
    return $out
}

# Rust string literal; refuses anything that would need escaping.
function Rs([string]$s) {
    if ($s -match '["\\]' -or $s -match '[^\x20-\x7E]') {
        throw "Value needs escaping or is not printable ASCII: $s"
    }
    return '"' + $s + '"'
}

function RsOpt([string]$variant) {
    if ($variant) { return "Some($variant)" } else { return 'None' }
}

# Reads espeak.voice from a Piper .onnx.json. ConvertFrom-Json in PowerShell
# 5.1 rejects these files (phoneme_id_map has keys like "X" and "x" that differ
# only in case), so parse with the case-sensitive JavaScriptSerializer.
Add-Type -AssemblyName System.Web.Extensions
function Get-PiperEspeakVoice([string]$url) {
    $text = (Invoke-WebRequest -Uri $url -UseBasicParsing).Content
    if ($text -is [byte[]]) { $text = [System.Text.Encoding]::UTF8.GetString($text) }
    $serializer = New-Object System.Web.Script.Serialization.JavaScriptSerializer
    $serializer.MaxJsonLength = [int]::MaxValue
    $meta = $serializer.DeserializeObject($text)
    if (-not ($meta -is [System.Collections.IDictionary]) -or -not $meta.ContainsKey('espeak') -or
        -not ($meta['espeak'] -is [System.Collections.IDictionary]) -or -not $meta['espeak'].ContainsKey('voice')) {
        throw "No espeak.voice in $url"
    }
    return [string]$meta['espeak']['voice']
}

# --- Release assets ---------------------------------------------------------

$releaseId = (Invoke-Gh @('api', "repos/$repo/releases/tags/$releaseTag", '--jq', '.id')) | Select-Object -First 1
$releaseId = "$releaseId".Trim()
if ($releaseId -notmatch '^\d+$') { throw "Unexpected release id for ${releaseTag}: '$releaseId'" }
Write-Host "Release $releaseTag id $releaseId"

# No double quotes in the jq filter: Windows PowerShell 5.1 does not escape
# them when passing arguments to native programs. A missing digest becomes
# the string "null", which the sha256 check below rejects.
$assetLines = Invoke-Gh @('api', "repos/$repo/releases/$releaseId/assets", '--paginate',
    '--jq', '.[] | [.name, (.size | tostring), (.digest | tostring)] | @tsv')
$assets = @{}
foreach ($line in $assetLines) {
    $parts = "$line".Split("`t")
    if ($parts.Count -ne 3) { throw "Unexpected asset line: $line" }
    $assets[$parts[0]] = @{ Size = [uint64]$parts[1]; Digest = $parts[2] }
}
Write-Host "Assets listed: $($assets.Count)"

# Archive records in first-use order, keyed by stem.
$archiveOrder = New-Object System.Collections.Generic.List[string]
$archives = @{}

function Get-Archive([string]$stem) {
    if ($archives.ContainsKey($stem)) { return $archives[$stem] }
    $file = "$stem.tar.bz2"
    if (-not $assets.ContainsKey($file)) {
        throw "Asset missing from the $releaseTag release: $file"
    }
    $asset = $assets[$file]
    if ($asset.Digest -notmatch '^sha256:([0-9a-f]{64})$') {
        throw "Digest for $file is not a sha256: '$($asset.Digest)'"
    }
    $sha = $Matches[1]
    if ($asset.Size -le 0) { throw "Asset $file has size $($asset.Size)" }
    if ($pinned.ContainsKey($stem)) {
        $pin = $pinned[$stem]
        if ($asset.Size -ne $pin.Size -or $sha -ne $pin.Sha256) {
            throw "Asset $file differs from the pinned value: got size $($asset.Size) sha256 $sha, pinned size $($pin.Size) sha256 $($pin.Sha256)"
        }
    }
    $const = 'ARCHIVE_' + ($stem.ToUpperInvariant() -replace '[^A-Z0-9]', '_')
    foreach ($other in $archives.Values) {
        if ($other.Const -eq $const) { throw "Archive constant collision: $const" }
    }
    $record = @{ Stem = $stem; Url = "$modelUrlBase$file"; Sha256 = $sha; Size = $asset.Size; Const = $const }
    $archives[$stem] = $record
    $archiveOrder.Add($stem)
    return $record
}

# --- Voices -----------------------------------------------------------------

$voices = New-Object System.Collections.Generic.List[hashtable]

$genderVariant = @{ 'female' = 'Gender::Female'; 'male' = 'Gender::Male'; 'neutral' = 'Gender::Neutral'; '-' = '' }

$rows = Import-Csv -Path $csvPath
foreach ($row in $rows) {
    $id = $row.id
    $parts = $id.Split('-')
    if ($parts.Count -ne 3) { throw "Piper id is not <lang>-<speaker>-<quality>: $id" }
    $lang, $speaker, $quality = $parts
    switch ($lang) {
        'en_US' { $accent = 'Accent::American'; $short = 'US'; $country = 'United States' }
        'en_GB' { $accent = 'Accent::British';  $short = 'UK'; $country = 'Great Britain' }
        default { throw "Unsupported Piper language for ${id}: $lang" }
    }
    if (-not $genderVariant.ContainsKey($row.gender)) { throw "Unknown gender for ${id}: $($row.gender)" }

    $jsonUrl = "$hfBase/$lang/$speaker/$quality/$id.onnx.json"
    $espeak = Get-PiperEspeakVoice $jsonUrl
    if ($espeak -eq 'en-gb') {
        throw "$id uses espeak voice 'en-gb', which espeak-ng does not have. Decide on a mapping before regenerating."
    }
    if ($allowedEspeak -notcontains $espeak) {
        throw "$id uses espeak voice '$espeak', outside {$($allowedEspeak -join ', ')}."
    }
    Write-Host ("Piper  {0,-36} espeak={1}" -f $id, $espeak)

    $description = "English ($country), $quality"
    if ($row.license -cmatch 'NC') { $description += '; non-commercial use only' }

    $voices.Add(@{
        Id = $id; Engine = 'Engine::Piper'
        Name = "$($row.name) ($short, $quality, Piper)"
        Gender = $genderVariant[$row.gender]; Accent = $accent
        Description = $description; License = $row.license
        Archive = (Get-Archive "vits-piper-$id"); Sid = 0
        Espeak = $espeak; Slow = $false
    })
}

$kokoroArchive = Get-Archive $kokoroStem
for ($sid = 0; $sid -lt $kokoroSpeakers.Count; $sid++) {
    $id = $kokoroSpeakers[$sid]
    switch ($id[0]) {
        'a' { $accent = 'Accent::American'; $short = 'US'; $country = 'United States'; $espeak = 'en-us' }
        'b' { $accent = 'Accent::British';  $short = 'UK'; $country = 'Great Britain'; $espeak = 'en-gb-x-rp' }
        default { throw "Unexpected Kokoro speaker: $id" }
    }
    switch ($id[1]) {
        'f' { $gender = 'Gender::Female' }
        'm' { $gender = 'Gender::Male' }
        default { throw "Unexpected Kokoro speaker: $id" }
    }
    $suffix = $id.Substring(3)
    $display = $suffix.Substring(0, 1).ToUpperInvariant() + $suffix.Substring(1)
    $voices.Add(@{
        Id = $id; Engine = 'Engine::Kokoro'
        Name = "$display ($short, Kokoro, slow)"
        Gender = $gender; Accent = $accent
        Description = "English ($country), Kokoro v1.0 (slow on most CPUs)"; License = 'Apache-2.0'
        Archive = $kokoroArchive; Sid = $sid
        Espeak = $espeak; Slow = $true
    })
}

$supertonicArchive = Get-Archive $supertonicStem
$supertonicIds = @('F1', 'F2', 'F3', 'F4', 'F5', 'M1', 'M2', 'M3', 'M4', 'M5')
for ($sid = 0; $sid -lt $supertonicIds.Count; $sid++) {
    $id = $supertonicIds[$sid]
    if ($id[0] -eq 'F') { $gender = 'Gender::Female' } else { $gender = 'Gender::Male' }
    $voices.Add(@{
        Id = $id; Engine = 'Engine::Supertonic'
        Name = "$id (Supertonic)"
        Gender = $gender; Accent = ''
        Description = 'English, Supertonic 3'; License = 'OpenRAIL-M'
        Archive = $supertonicArchive; Sid = $sid
        Espeak = ''; Slow = $false
    })
}

# --- Emit Rust --------------------------------------------------------------

$sb = New-Object System.Text.StringBuilder
function Line([string]$s) { [void]$sb.Append($s).Append("`n") }

Line '// GENERATED by scripts/gen-catalog.ps1 from scripts/catalog-piper.csv. Do not edit.'
Line ''
Line 'use super::{Accent, Archive, Engine, Gender, Voice};'
foreach ($stem in $archiveOrder) {
    $a = $archives[$stem]
    Line ''
    Line "static $($a.Const): Archive = Archive {"
    Line "    stem: $(Rs $a.Stem),"
    Line "    url: $(Rs $a.Url),"
    Line "    sha256: $(Rs $a.Sha256),"
    Line "    size: $($a.Size),"
    Line '};'
}
Line ''
Line 'pub static VOICES: &[Voice] = &['
foreach ($v in $voices) {
    if ($v.Espeak) { $espeakRs = "Some($(Rs $v.Espeak))" } else { $espeakRs = 'None' }
    Line '    Voice {'
    Line "        id: $(Rs $v.Id),"
    Line '        aliases: &[],'
    Line "        engine: $($v.Engine),"
    Line "        name: $(Rs $v.Name),"
    Line "        gender: $(RsOpt $v.Gender),"
    Line "        accent: $(RsOpt $v.Accent),"
    Line "        description: $(Rs $v.Description),"
    Line "        license: $(Rs $v.License),"
    Line "        archive: &$($v.Archive.Const),"
    Line "        sid: $($v.Sid),"
    Line "        espeak_voice: $espeakRs,"
    Line "        slow: $(if ($v.Slow) { 'true' } else { 'false' }),"
    Line '    },'
}
Line '];'

$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText($outPath, $sb.ToString(), $utf8NoBom)

$byEngine = $voices | Group-Object { $_.Engine } | ForEach-Object { "$($_.Name)=$($_.Count)" }
Write-Host "Wrote $outPath : $($voices.Count) voices ($($byEngine -join ', ')), $($archiveOrder.Count) archives"
