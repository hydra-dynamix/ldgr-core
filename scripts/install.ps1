$ErrorActionPreference = "Stop"

$Repository = if ($env:LDGR_REPO) { $env:LDGR_REPO } else { "hydra-dynamix/ldgr-core" }
function Resolve-InstallDirectory {
    if ($env:LDGR_INSTALL_DIR) {
        return [System.IO.Path]::GetFullPath($env:LDGR_INSTALL_DIR)
    }
    $resolved = Get-Command ldgr -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($resolved -and $resolved.Source) {
        $candidate = [System.IO.Path]::GetFullPath((Split-Path -Parent $resolved.Source))
        $userRoots = @($env:USERPROFILE, $env:LOCALAPPDATA) |
            Where-Object { $_ } |
            ForEach-Object { [System.IO.Path]::GetFullPath($_).TrimEnd("\") + "\" }
        if ($userRoots | Where-Object { ($candidate.TrimEnd("\") + "\").StartsWith($_, [System.StringComparison]::OrdinalIgnoreCase) }) {
            return $candidate
        }
    }
    return Join-Path $env:LOCALAPPDATA "Programs\ldgr\bin"
}
$InstallDirectory = Resolve-InstallDirectory
$Version = $env:LDGR_VERSION
$Platform = "windows-x86_64"
$Binary = "ldgr.exe"
$AgentctlBinary = "agentctl.exe"

if (-not [Environment]::Is64BitOperatingSystem) {
    throw "ldgr requires 64-bit Windows."
}

if (-not $Version) {
    # Windows PowerShell 5.1 can preserve a multi-item REST response as one
    # nested object, so ask GitHub for exactly one release.
    $release = Invoke-RestMethod `
        -Uri "https://api.github.com/repos/$Repository/releases?per_page=1" `
        -Headers @{ Accept = "application/vnd.github+json" }
    $tagName = @($release.tag_name)[0]
    if ($tagName) {
        $Version = $tagName -replace "^v", ""
    }
}

if (-not $Version) {
    throw "Could not resolve the latest $Repository release."
}

$archiveName = "ldgr-core-$Version-$Platform.tar.gz"
$releaseBase = if ($env:LDGR_RELEASE_BASE_URL) {
    $env:LDGR_RELEASE_BASE_URL.TrimEnd("/")
} else {
    "https://github.com/$Repository/releases/download"
}
$downloadBase = "$releaseBase/v$Version/$archiveName"
$temporaryDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "ldgr-install-$([guid]::NewGuid())"

try {
    New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null
    $archivePath = Join-Path $temporaryDirectory $archiveName
    $checksumPath = "$archivePath.sha256"

    Write-Host "Installing ldgr $Version for $Platform"
    Invoke-WebRequest -UseBasicParsing -Uri $downloadBase -OutFile $archivePath
    Invoke-WebRequest -UseBasicParsing -Uri "$downloadBase.sha256" -OutFile $checksumPath

    $expectedHash = ((Get-Content -Raw $checksumPath).Trim() -split "\s+")[0].ToLowerInvariant()
    $actualHash = (Get-FileHash -Algorithm SHA256 $archivePath).Hash.ToLowerInvariant()
    if ($expectedHash -ne $actualHash) {
        throw "Checksum mismatch for $archiveName."
    }

    tar -xzf $archivePath -C $temporaryDirectory
    if ($LASTEXITCODE -ne 0) {
        throw "Could not extract $archiveName."
    }

    $sourceRoot = Join-Path $temporaryDirectory "ldgr-core-$Version\$Platform"
    $source = Join-Path $sourceRoot $Binary
    $agentctlSource = Join-Path $sourceRoot $AgentctlBinary
    if (-not (Test-Path -LiteralPath $source -PathType Leaf) -or
        -not (Test-Path -LiteralPath $agentctlSource -PathType Leaf)) {
        throw "The release archive did not contain the paired ldgr.exe and agentctl.exe binaries under $sourceRoot."
    }

    New-Item -ItemType Directory -Force -Path $InstallDirectory | Out-Null
    $destination = Join-Path $InstallDirectory $Binary
    $agentctlDestination = Join-Path $InstallDirectory $AgentctlBinary
    foreach ($path in @($agentctlDestination, $destination)) {
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            Copy-Item -LiteralPath $path -Destination "$path.previous" -Force
        }
    }
    try {
        # Update the launcher first. If a running detached Core is locked, the
        # new launcher rejects the old Core visibly until this installer reruns.
        Copy-Item -LiteralPath $agentctlSource -Destination $agentctlDestination -Force
        Copy-Item -LiteralPath $source -Destination $destination -Force
    } catch {
        throw "Could not replace the resolved LDGR binaries in $InstallDirectory. Stop detached loops using this installation, then rerun the installer. Previous binaries remain as *.previous. $($_.Exception.Message)"
    }

    Write-Host "Installed paired binaries to $InstallDirectory"
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $pathEntries = @($userPath -split ";" | Where-Object { $_ })
    if ($InstallDirectory -notin $pathEntries) {
        [Environment]::SetEnvironmentVariable(
            "Path",
            (($InstallDirectory + $pathEntries) -join ";"),
            "User"
        )
        Write-Host "Added $InstallDirectory to your user PATH. Open a new terminal to use it everywhere."
    }
    $env:Path = "$InstallDirectory;$env:Path"

    $resolvedLdgr = (Get-Command ldgr -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $resolvedAgentctl = (Get-Command agentctl -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    if ([System.IO.Path]::GetFullPath($resolvedLdgr) -ne [System.IO.Path]::GetFullPath($destination) -or
        [System.IO.Path]::GetFullPath($resolvedAgentctl) -ne [System.IO.Path]::GetFullPath($agentctlDestination)) {
        throw "PATH still resolves a different ldgr or agentctl. Expected $destination and $agentctlDestination; resolved $resolvedLdgr and $resolvedAgentctl."
    }
    & $destination --version
    & $agentctlDestination --version
    & $destination compatibility --agentctl-version $((& $agentctlDestination --version) -replace '^agentctl\s+', '') --json
} finally {
    if (Test-Path -LiteralPath $temporaryDirectory) {
        Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force
    }
}
