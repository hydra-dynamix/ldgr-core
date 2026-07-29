$ErrorActionPreference = "Stop"

$Repository = if ($env:LDGR_REPO) { $env:LDGR_REPO } else { "hydra-dynamix/ldgr-core" }
$InstallDirectory = if ($env:LDGR_INSTALL_DIR) {
    $env:LDGR_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA "Programs\ldgr\bin"
}
$Version = $env:LDGR_VERSION
$Platform = "windows-x86_64"
$Binary = "ldgr.exe"

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

    $source = Join-Path $temporaryDirectory "ldgr-core-$Version\$Platform\$Binary"
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) {
        throw "The release archive did not contain $source."
    }

    New-Item -ItemType Directory -Force -Path $InstallDirectory | Out-Null
    $destination = Join-Path $InstallDirectory $Binary
    Copy-Item -LiteralPath $source -Destination $destination -Force

    Write-Host "Installed ldgr to $destination"
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $pathEntries = @($userPath -split ";" | Where-Object { $_ })
    if ($InstallDirectory -notin $pathEntries) {
        [Environment]::SetEnvironmentVariable(
            "Path",
            (($pathEntries + $InstallDirectory) -join ";"),
            "User"
        )
        $env:Path = "$env:Path;$InstallDirectory"
        Write-Host "Added $InstallDirectory to your user PATH. Open a new terminal to use it everywhere."
    }

    & $destination --version
} finally {
    if (Test-Path -LiteralPath $temporaryDirectory) {
        Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force
    }
}
