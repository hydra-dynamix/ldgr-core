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
    "Copy-InstallerSource",
    "Resolve-PythonCommand",
    "Invoke-CatalogHelper"
)
$definitions = $ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Name -in $functionNames
}, $true)
if ($definitions.Count -ne $functionNames.Count) {
    throw "Could not load the signed catalog installer functions."
}
Invoke-Expression (($definitions | ForEach-Object { $_.Extent.Text }) -join [Environment]::NewLine)

$root = Join-Path ([System.IO.Path]::GetTempPath()) "ldgr-catalog-test-$([guid]::NewGuid())"
try {
    New-Item -ItemType Directory -Path $root | Out-Null
    $source = Join-Path $root "source.txt"
    $destination = Join-Path $root "destination.txt"
    [System.IO.File]::WriteAllText($source, "signed fixture")
    Copy-InstallerSource -Source ([Uri]$source).AbsoluteUri -Destination $destination -Offline
    if ((Get-Content -Raw $destination) -ne "signed fixture") {
        throw "file:// source copy changed the fixture."
    }
    foreach ($case in @(
        @{ Source = "http://example.invalid/core-index.json"; Offline = $false; Pattern = "HTTPS or file" },
        @{ Source = "https://example.invalid/core-index.json"; Offline = $true; Pattern = "Offline" }
    )) {
        try {
            Copy-InstallerSource -Source $case.Source -Destination $destination -Offline:$case.Offline
            throw "Untrusted installer source was accepted."
        } catch {
            if ($_.Exception.Message -notmatch $case.Pattern) { throw }
        }
    }
    Write-Host "Windows signed catalog installer tests passed"
} finally {
    if (Test-Path -LiteralPath $root) {
        Remove-Item -LiteralPath $root -Recurse -Force
    }
}
