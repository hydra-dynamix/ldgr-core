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

function Test-PathEntryMatchesInstallDirectory {
    param(
        [Parameter(Mandatory = $true)][string]$PathEntry,
        [Parameter(Mandatory = $true)][string]$InstallDirectory,
        [switch]$ExpandEnvironmentNames
    )

    $candidate = $PathEntry.Trim().Trim('"')
    if (-not $candidate) {
        return $false
    }

    try {
        if ($ExpandEnvironmentNames) {
            $candidate = [Environment]::ExpandEnvironmentVariables($candidate)
        }
        $candidate = [System.IO.Path]::GetFullPath($candidate).TrimEnd("\")
        $expected = [System.IO.Path]::GetFullPath($InstallDirectory).TrimEnd("\")
        return $candidate.Equals($expected, [System.StringComparison]::OrdinalIgnoreCase)
    } catch {
        return $candidate.TrimEnd("\").Equals(
            $InstallDirectory.TrimEnd("\"),
            [System.StringComparison]::OrdinalIgnoreCase
        )
    }
}

function Publish-UserEnvironmentChange {
    if (-not ("Ldgr.WindowsEnvironment" -as [type])) {
        Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

namespace Ldgr {
    public static class WindowsEnvironment {
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern IntPtr SendMessageTimeout(
            IntPtr hWnd,
            uint message,
            UIntPtr wParam,
            string lParam,
            uint flags,
            uint timeout,
            out UIntPtr result);
    }
}
"@ | Out-Null
    }

    $result = [UIntPtr]::Zero
    $sent = [Ldgr.WindowsEnvironment]::SendMessageTimeout(
        [IntPtr]0xffff,
        0x001A,
        [UIntPtr]::Zero,
        "Environment",
        0x0002,
        5000,
        [ref]$result
    )
    if ($sent -eq [IntPtr]::Zero) {
        $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        Write-Warning "Updated the user PATH in the registry, but Windows did not broadcast the environment change (Win32 error $errorCode). Open a new terminal; if it does not see the change, sign out and back in."
    }
}

function Add-InstallDirectoryToUserPath {
    param(
        [Parameter(Mandatory = $true)][string]$InstallDirectory,
        [string]$RegistrySubKey = "Environment",
        [switch]$SkipBroadcast
    )

    $registryKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($RegistrySubKey, $true)
    if (-not $registryKey) {
        $registryKey = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($RegistrySubKey)
    }
    if (-not $registryKey) {
        throw "Could not open HKCU:\$RegistrySubKey for writing."
    }

    try {
        # Environment.GetEnvironmentVariable(..., "User") expands REG_EXPAND_SZ.
        # Read the registry value without expansion so existing %VAR% entries and
        # the registry value kind survive the update exactly as the user stored them.
        $rawUserPath = [string]$registryKey.GetValue(
            "Path",
            "",
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
        )
        if ($registryKey.GetValueNames() -contains "Path") {
            $pathValueKind = $registryKey.GetValueKind("Path")
        } else {
            $pathValueKind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        }
        if ($pathValueKind -notin @(
            [Microsoft.Win32.RegistryValueKind]::String,
            [Microsoft.Win32.RegistryValueKind]::ExpandString
        )) {
            throw "HKCU:\$RegistrySubKey\Path has unsupported registry kind $pathValueKind."
        }

        foreach ($pathEntry in @($rawUserPath -split ";")) {
            $entryMatches = $pathEntry -and (Test-PathEntryMatchesInstallDirectory `
                -PathEntry $pathEntry `
                -InstallDirectory $InstallDirectory `
                -ExpandEnvironmentNames:($pathValueKind -eq [Microsoft.Win32.RegistryValueKind]::ExpandString))
            if ($entryMatches) {
                return $false
            }
        }

        $newUserPath = if ($rawUserPath) {
            "$InstallDirectory;$rawUserPath"
        } else {
            $InstallDirectory
        }
        $registryKey.SetValue("Path", $newUserPath, $pathValueKind)
    } finally {
        $registryKey.Dispose()
    }

    if (-not $SkipBroadcast) {
        Publish-UserEnvironmentChange
    }
    return $true
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
    $releaseMetadataPath = Join-Path $temporaryDirectory "ldgr-core-$Version\RELEASE-METADATA.json"
    if (-not (Test-Path -LiteralPath $releaseMetadataPath -PathType Leaf)) {
        throw "The release archive did not contain RELEASE-METADATA.json."
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
    try {
        $pathUpdated = Add-InstallDirectoryToUserPath $InstallDirectory
        if ($pathUpdated) {
            Write-Host "Added $InstallDirectory to your user PATH. Open a new terminal to use it everywhere."
        }
    } catch {
        Write-Warning "Installed the binaries, but could not update your user PATH. Add $InstallDirectory manually. $($_.Exception.Message)"
    }
    $env:Path = "$InstallDirectory;$env:Path"

    $resolvedLdgr = (Get-Command ldgr -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $resolvedAgentctl = (Get-Command agentctl -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    if ([System.IO.Path]::GetFullPath($resolvedLdgr) -ne [System.IO.Path]::GetFullPath($destination) -or
        [System.IO.Path]::GetFullPath($resolvedAgentctl) -ne [System.IO.Path]::GetFullPath($agentctlDestination)) {
        throw "PATH still resolves a different ldgr or agentctl. Expected $destination and $agentctlDestination; resolved $resolvedLdgr and $resolvedAgentctl."
    }
    $coreVersionOutput = (& $destination --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $coreVersionOutput -ne "ldgr $Version") {
        throw "Installed Core version validation failed: expected ldgr $Version; got $coreVersionOutput."
    }
    $agentctlVersionOutput = (& $agentctlDestination --version).Trim()
    if ($LASTEXITCODE -ne 0 -or $agentctlVersionOutput -notmatch '^agentctl\s+(.+)$') {
        throw "Installed agentctl version validation failed: $agentctlVersionOutput."
    }
    $agentctlVersion = $Matches[1]
    & $destination compatibility --agentctl-version $agentctlVersion --json
    if ($LASTEXITCODE -ne 0) {
        throw "Installed Core/agentctl compatibility validation failed."
    }
    $homeDirectory = if ($env:USERPROFILE) { $env:USERPROFILE } else { $env:HOME }
    if (-not $homeDirectory) {
        throw "Could not determine the user home for the Core installation receipt."
    }
    $signingKeyId = if ($env:LDGR_SIGNING_KEY_ID) {
        $env:LDGR_SIGNING_KEY_ID
    } else {
        "ldgr-release-2026-01"
    }
    $receiptArgs = @(
        "__record-core-installation",
        "--home", $homeDirectory,
        "--agentctl-binary", $agentctlDestination,
        "--release-metadata", $releaseMetadataPath,
        "--archive-url", $downloadBase,
        "--archive-sha256", $actualHash,
        "--signing-key-id", $signingKeyId
    )
    & $destination @receiptArgs
    if ($LASTEXITCODE -ne 0) {
        throw "Installed pair validated, but the Core installation receipt was not written."
    }
    Write-Host "Recorded official installation ownership under $homeDirectory\.ldgr"
} finally {
    if (Test-Path -LiteralPath $temporaryDirectory) {
        Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force
    }
}
