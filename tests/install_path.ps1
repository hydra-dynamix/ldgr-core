$ErrorActionPreference = "Stop"

$installerPath = Join-Path (Split-Path -Parent $PSScriptRoot) "scripts\install.ps1"
$tokens = $null
$parseErrors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    $installerPath,
    [ref]$tokens,
    [ref]$parseErrors
)
if ($parseErrors.Count -ne 0) {
    throw "Installer has PowerShell parse errors: $($parseErrors -join '; ')"
}

$functionNames = @(
    "Test-PathEntryMatchesInstallDirectory",
    "Publish-UserEnvironmentChange",
    "Add-InstallDirectoryToUserPath"
)
$definitions = $ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -in $functionNames
}, $true)
if ($definitions.Count -ne $functionNames.Count) {
    throw "Could not load the installer PATH helper functions."
}
Invoke-Expression (($definitions | ForEach-Object { $_.Extent.Text }) -join "`n")

$testId = [guid]::NewGuid().ToString("N")
$testVariable = "LDGR_INSTALLER_PATH_TEST_$testId"
$testRoot = "C:\Users\ldgr-path-test"
$registrySubKey = "Software\hydra-dynamix\ldgr\installer-tests\$testId"
$registryKey = $null

try {
    [Environment]::SetEnvironmentVariable($testVariable, $testRoot, "Process")
    $registryKey = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($registrySubKey)
    $originalPath = "%$testVariable%\bin;C:\Existing Tools;"
    $registryKey.SetValue(
        "Path",
        $originalPath,
        [Microsoft.Win32.RegistryValueKind]::ExpandString
    )
    $registryKey.Dispose()
    $registryKey = $null

    $installDirectory = "C:\New LDGR\bin"
    $updated = Add-InstallDirectoryToUserPath `
        -InstallDirectory $installDirectory `
        -RegistrySubKey $registrySubKey `
        -SkipBroadcast
    if (-not $updated) {
        throw "The installer did not report adding a missing PATH entry."
    }

    $registryKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($registrySubKey)
    $rawPath = [string]$registryKey.GetValue(
        "Path",
        "",
        [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
    )
    $valueKind = $registryKey.GetValueKind("Path")
    $registryKey.Dispose()
    $registryKey = $null

    $expectedPath = "$installDirectory;$originalPath"
    if ($rawPath -cne $expectedPath) {
        throw "Raw PATH was not preserved. Expected '$expectedPath'; got '$rawPath'."
    }
    if ($valueKind -ne [Microsoft.Win32.RegistryValueKind]::ExpandString) {
        throw "PATH registry kind changed from ExpandString to $valueKind."
    }

    $expandedExistingDirectory = "$testRoot\bin"
    $updated = Add-InstallDirectoryToUserPath `
        -InstallDirectory $expandedExistingDirectory `
        -RegistrySubKey $registrySubKey `
        -SkipBroadcast
    if ($updated) {
        throw "An expanded equivalent of an existing raw PATH entry was duplicated."
    }

    $registryKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($registrySubKey)
    $rawPathAfterDuplicateCheck = [string]$registryKey.GetValue(
        "Path",
        "",
        [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames
    )
    if ($rawPathAfterDuplicateCheck -cne $expectedPath) {
        throw "Duplicate detection modified the raw PATH."
    }

    $emptyRegistrySubKey = "$registrySubKey\empty"
    $updated = Add-InstallDirectoryToUserPath `
        -InstallDirectory $installDirectory `
        -RegistrySubKey $emptyRegistrySubKey `
        -SkipBroadcast
    if (-not $updated) {
        throw "The installer did not add PATH when the registry value was absent."
    }
    $registryKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($emptyRegistrySubKey)
    if ($registryKey.GetValueKind("Path") -ne [Microsoft.Win32.RegistryValueKind]::ExpandString) {
        throw "A new user PATH was not created as ExpandString."
    }
    $registryKey.Dispose()
    $registryKey = $null

    $literalRegistrySubKey = "$registrySubKey\literal"
    $registryKey = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($literalRegistrySubKey)
    $registryKey.SetValue(
        "Path",
        "%$testVariable%\bin",
        [Microsoft.Win32.RegistryValueKind]::String
    )
    $registryKey.Dispose()
    $registryKey = $null
    $updated = Add-InstallDirectoryToUserPath `
        -InstallDirectory $expandedExistingDirectory `
        -RegistrySubKey $literalRegistrySubKey `
        -SkipBroadcast
    if (-not $updated) {
        throw "A literal REG_SZ PATH entry was incorrectly treated as expandable."
    }
    $registryKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($literalRegistrySubKey)
    if ($registryKey.GetValueKind("Path") -ne [Microsoft.Win32.RegistryValueKind]::String) {
        throw "A REG_SZ user PATH did not retain its registry kind."
    }
    $registryKey.Dispose()
    $registryKey = $null

    Publish-UserEnvironmentChange
    Write-Host "Windows installer PATH tests passed"
} finally {
    if ($registryKey) {
        $registryKey.Dispose()
    }
    [Environment]::SetEnvironmentVariable($testVariable, $null, "Process")
    $testBase = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey(
        "Software\hydra-dynamix\ldgr\installer-tests",
        $true
    )
    if ($testBase) {
        $testBase.DeleteSubKeyTree($testId, $false)
        $testBase.Dispose()
    }
}
