# Dot-source this file, then call Get-VerifiedArchive.
#
# Downloads a .tar.bz2 archive, checks its byte size and SHA256 against pinned
# values, extracts it to <DestDir>\<archive name minus .tar.bz2>, and returns
# that folder's path. A `.verified` marker inside it makes repeat calls a no-op.
# Works under Windows PowerShell 5.1 and from paths containing spaces and '@'.

function Get-VerifiedArchive {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory)] [string] $Url,
        [Parameter(Mandatory)] [string] $Sha256,
        [Parameter(Mandatory)] [long]   $Size,
        [Parameter(Mandatory)] [string] $DestDir
    )

    $ErrorActionPreference = 'Stop'

    $name = [System.IO.Path]::GetFileName(([Uri]$Url).AbsolutePath)
    if (-not $name.EndsWith('.tar.bz2')) {
        throw "Get-VerifiedArchive only handles .tar.bz2 archives, got '$name'."
    }
    $stem = $name.Substring(0, $name.Length - '.tar.bz2'.Length)

    $null = New-Item -ItemType Directory -Force -Path $DestDir
    $DestDir = (Resolve-Path -LiteralPath $DestDir).ProviderPath

    $finalDir   = Join-Path $DestDir $stem
    $marker     = Join-Path $finalDir '.verified'
    $partFile   = Join-Path $DestDir "$name.part"
    $extractDir = Join-Path $DestDir ".extract-$stem"

    if (Test-Path -LiteralPath $marker) {
        return $finalDir
    }

    function Remove-Leftovers {
        foreach ($p in @($partFile, $extractDir)) {
            if (Test-Path -LiteralPath $p) { Remove-Item -LiteralPath $p -Recurse -Force }
        }
    }

    Remove-Leftovers
    try {
        Write-Host "Downloading $name ($([math]::Round($Size / 1MB, 1)) MB)..."
        # PowerShell 5.1 defaults to old TLS versions and a very slow progress bar.
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        $oldProgress = $ProgressPreference
        $ProgressPreference = 'SilentlyContinue'
        try {
            Invoke-WebRequest -Uri $Url -OutFile $partFile -UseBasicParsing
        } finally {
            $ProgressPreference = $oldProgress
        }

        $actualSize = (Get-Item -LiteralPath $partFile).Length
        if ($actualSize -ne $Size) {
            throw "Size mismatch for ${name}: expected $Size bytes, got $actualSize bytes."
        }

        $actualHash = (Get-FileHash -LiteralPath $partFile -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actualHash -ne $Sha256.ToLowerInvariant()) {
            throw "SHA256 mismatch for ${name}: expected $($Sha256.ToLowerInvariant()), got $actualHash."
        }

        Write-Host "Extracting $name..."
        $null = New-Item -ItemType Directory -Force -Path $extractDir
        # Use Windows' bsdtar explicitly; Git Bash's GNU tar can't handle C:\ paths.
        $tar = Join-Path $env:SystemRoot 'System32\tar.exe'
        & $tar -xjf $partFile -C $extractDir
        if ($LASTEXITCODE -ne 0) {
            throw "Extracting $name failed (tar exit code $LASTEXITCODE)."
        }

        # Most archives wrap everything in one '<stem>' folder; some (piper-phonemize)
        # put include\ and lib\ at the top level. Either way we end up with <DestDir>\<stem>.
        $entries = @(Get-ChildItem -LiteralPath $extractDir -Force)
        $wrapped = ($entries.Count -eq 1) -and $entries[0].PSIsContainer -and ($entries[0].Name -eq $stem)
        $extracted = if ($wrapped) { $entries[0].FullName } else { $extractDir }
        if ($entries.Count -eq 0) {
            throw "$name extracted to nothing."
        }

        if (Test-Path -LiteralPath $finalDir) {
            # An earlier run was interrupted after the rename but before the marker.
            Remove-Item -LiteralPath $finalDir -Recurse -Force
        }
        Move-Item -LiteralPath $extracted -Destination $finalDir
        Set-Content -LiteralPath $marker -Value "$Sha256  $Url" -Encoding ASCII
    } finally {
        Remove-Leftovers
    }

    return $finalDir
}
