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

function Copy-InstallerSource {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination,
        [switch]$Offline
    )
    $uri = [Uri]$Source
    if ($uri.IsFile) {
        Copy-Item -LiteralPath $uri.LocalPath -Destination $Destination -Force
        return
    }
    if ($uri.Scheme -ne "https") {
        throw "Installer sources must use HTTPS or file://: $Source"
    }
    if ($Offline) {
        throw "Offline installation requires file:// sources."
    }
    Add-Type -AssemblyName System.Net.Http
    $handler = [System.Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $false
    $client = [System.Net.Http.HttpClient]::new($handler)
    try {
        $current = $uri
        for ($redirects = 0; $redirects -le 10; $redirects++) {
            $response = $client.GetAsync(
                $current,
                [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead
            ).GetAwaiter().GetResult()
            try {
                $status = [int]$response.StatusCode
                if ($status -ge 300 -and $status -lt 400) {
                    $location = $response.Headers.Location
                    if (-not $location) {
                        throw "HTTPS redirect omitted Location: $current"
                    }
                    $current = [Uri]::new($current, $location)
                    if ($current.Scheme -ne "https") {
                        throw "Installer redirects must remain HTTPS: $current"
                    }
                    continue
                }
                $response.EnsureSuccessStatusCode() | Out-Null
                $inputStream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
                $outputStream = [System.IO.File]::Create($Destination)
                try {
                    $inputStream.CopyTo($outputStream)
                } finally {
                    $outputStream.Dispose()
                    $inputStream.Dispose()
                }
                return
            } finally {
                $response.Dispose()
            }
        }
        throw "Installer source exceeded the HTTPS redirect limit: $Source"
    } finally {
        $client.Dispose()
        $handler.Dispose()
    }
}

function Resolve-PythonCommand {
    foreach ($name in @("python3", "python", "py")) {
        $command = Get-Command $name -CommandType Application -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($command) {
            return $command.Source
        }
    }
    throw "The signed Core installer requires Python 3."
}

function Invoke-CatalogHelper {
    param(
        [Parameter(Mandatory = $true)][string]$Python,
        [Parameter(Mandatory = $true)][string]$Helper,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )
    $prefix = if ([System.IO.Path]::GetFileNameWithoutExtension($Python) -eq "py") {
        @("-3")
    } else {
        @()
    }
    & $Python @prefix $Helper @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Signed Core catalog verification failed."
    }
}

$InstallDirectory = Resolve-InstallDirectory
$Version = $env:LDGR_VERSION
$Platform = "windows-x86_64"
$Binary = "ldgr.exe"
$AgentctlBinary = "agentctl.exe"

if (-not [Environment]::Is64BitOperatingSystem) {
    throw "ldgr requires 64-bit Windows."
}

$catalogSource = if ($env:LDGR_CORE_UPDATE_INDEX) {
    $env:LDGR_CORE_UPDATE_INDEX
} else {
    "https://raw.githubusercontent.com/hydra-dynamix/ldgr-releases/main/core-index.json"
}
$helperSource = if ($env:LDGR_CORE_CATALOG_HELPER) {
    $env:LDGR_CORE_CATALOG_HELPER
} else {
    "https://raw.githubusercontent.com/$Repository/main/scripts/core-catalog.py"
}
$offline = $env:LDGR_INSTALL_OFFLINE -eq "1"
$temporaryDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "ldgr-install-$([guid]::NewGuid())"

try {
    New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null
    $python = Resolve-PythonCommand
    $helperPath = Join-Path $temporaryDirectory "core-catalog.py"
    $catalogPath = Join-Path $temporaryDirectory "core-index.json"
    $catalogSignaturePath = "$catalogPath.sig"
    $keyringPath = Join-Path $temporaryDirectory "release-keyring.json"
    $resolvedPath = Join-Path $temporaryDirectory "resolved.json"
    Copy-InstallerSource -Source $helperSource -Destination $helperPath -Offline:$offline
    Copy-InstallerSource -Source $catalogSource -Destination $catalogPath -Offline:$offline
    Copy-InstallerSource -Source "$catalogSource.sig" -Destination $catalogSignaturePath -Offline:$offline
    if ($env:LDGR_CORE_RELEASE_KEYRING) {
        Copy-InstallerSource -Source $env:LDGR_CORE_RELEASE_KEYRING -Destination $keyringPath -Offline:$offline
    } else {
        [System.IO.File]::WriteAllText(
            $keyringPath,
            '{"keys":[{"key_id":"ldgr-release-2026-01","public_key":"3wI34tu3PrqWp6VdNrNsFfX1W5PWSeQ3vsR04B69d+I="}]}'
        )
    }
    $resolveArgs = @(
        "resolve", "--catalog", $catalogPath,
        "--signature", $catalogSignaturePath,
        "--keyring", $keyringPath,
        "--platform", $Platform,
        "--output", $resolvedPath
    )
    if ($Version) { $resolveArgs += @("--version", $Version) }
    if ($env:LDGR_PRERELEASE -eq "1") { $resolveArgs += "--prerelease" }
    if ($offline) { $resolveArgs += "--offline" }
    Invoke-CatalogHelper -Python $python -Helper $helperPath -Arguments $resolveArgs
    $resolvedRelease = Get-Content -Raw -LiteralPath $resolvedPath | ConvertFrom-Json
    $Version = [string]$resolvedRelease.version
    $expectedAgentctlVersion = [string]$resolvedRelease.agentctl.version
    $downloadBase = [string]$resolvedRelease.platform.archive_url
    $signatureUrl = [string]$resolvedRelease.platform.signature_url
    $actualHash = [string]$resolvedRelease.platform.sha256
    $signingKeyId = [string]$resolvedRelease.platform.signing_key_id
    $archiveName = "ldgr-core-$Version-$Platform.tar.gz"
    $archivePath = Join-Path $temporaryDirectory $archiveName
    $checksumPath = "$archivePath.sha256"
    $signaturePath = "$archivePath.sig"

    Write-Host "Installing signed ldgr $Version for $Platform"
    Copy-InstallerSource -Source $downloadBase -Destination $archivePath -Offline:$offline
    Copy-InstallerSource -Source "$downloadBase.sha256" -Destination $checksumPath -Offline:$offline
    Copy-InstallerSource -Source $signatureUrl -Destination $signaturePath -Offline:$offline
    Invoke-CatalogHelper -Python $python -Helper $helperPath -Arguments @(
        "verify-archive", "--resolved", $resolvedPath,
        "--archive", $archivePath, "--checksum", $checksumPath,
        "--signature", $signaturePath
    )

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
    if ($LASTEXITCODE -ne 0 -or
        $agentctlVersionOutput -ne "agentctl $expectedAgentctlVersion") {
        throw "Installed agentctl version validation failed: expected agentctl $expectedAgentctlVersion; got $agentctlVersionOutput."
    }
    & $destination compatibility --agentctl-version $expectedAgentctlVersion --json
    if ($LASTEXITCODE -ne 0) {
        throw "Installed Core/agentctl compatibility validation failed."
    }
    $homeDirectory = if ($env:USERPROFILE) { $env:USERPROFILE } else { $env:HOME }
    if (-not $homeDirectory) {
        throw "Could not determine the user home for the Core installation receipt."
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
    if ($LASTEXITCODE -eq 0) {
        Write-Host "Recorded official installation ownership under $homeDirectory\.ldgr"
    } else {
        $historical = Get-Content -Raw -LiteralPath $releaseMetadataPath | ConvertFrom-Json
        $historicalProperties = @($historical.PSObject.Properties.Name | Sort-Object)
        $expectedHistoricalProperties = @(
            "agentctl_commit", "agentctl_repository", "agentctl_version", "binary",
            "component", "component_commit", "package", "platform", "root_commit",
            "schema_version", "source_repository", "version"
        ) | Sort-Object
        $historicalShape =
            ($historicalProperties -join "|") -eq ($expectedHistoricalProperties -join "|") -and
            $historical.schema_version -eq 1 -and
            $historical.component -eq "ldgr-core" -and
            $historical.root_commit -match "^[0-9a-f]{40}$" -and
            $historical.component_commit -match "^[0-9a-f]{40}$"
        if (-not $historicalShape) {
            throw "Installed pair validated, but the Core installation receipt was not written."
        }
        Write-Host "Installed reviewed historical paired Core; the first update requires --yes for safe ownership adoption."
    }
} finally {
    if (Test-Path -LiteralPath $temporaryDirectory) {
        Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force
    }
}
