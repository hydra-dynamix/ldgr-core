param(
    [Parameter(Mandatory = $true)]
    [string]$Exe,

    [Parameter(Mandatory = $true)]
    [string]$TestRoot,

    [Parameter(Mandatory = $true)]
    [string]$LegacyFixtureRoot,

    [Parameter(Mandatory = $true)]
    [string]$SourceRoot
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2

$results = [System.Collections.Generic.List[object]]::new()
$ProfileRoot = Join-Path $TestRoot 'profile'
$MainProject = Join-Path $TestRoot 'current'
$MainDb = Join-Path $MainProject '.ldgr\ldgr.db'
$MainArtifacts = Join-Path $MainProject '.ldgr\artifacts'
$FixtureRoot = Join-Path $PSScriptRoot 'fixtures\cli-e2e'
$ResultPath = Join-Path $TestRoot 'result.json'
$OriginalProfileRoot = [Environment]::GetEnvironmentVariable('USERPROFILE')
$CargoToolchainRoot = [Environment]::GetEnvironmentVariable('CARGO_HOME')
$RustupToolchainRoot = [Environment]::GetEnvironmentVariable('RUSTUP_HOME')
if ([string]::IsNullOrWhiteSpace($CargoToolchainRoot) -and -not [string]::IsNullOrWhiteSpace($OriginalProfileRoot)) {
    $CargoToolchainRoot = Join-Path $OriginalProfileRoot '.cargo'
}
if ([string]::IsNullOrWhiteSpace($RustupToolchainRoot) -and -not [string]::IsNullOrWhiteSpace($OriginalProfileRoot)) {
    $RustupToolchainRoot = Join-Path $OriginalProfileRoot '.rustup'
}

function ConvertTo-NativeArgument {
    param([AllowEmptyString()][string]$Value)

    if ($Value.Length -gt 0 -and $Value -notmatch '[\s"]') {
        return $Value
    }

    $builder = [System.Text.StringBuilder]::new()
    [void]$builder.Append('"')
    $backslashes = 0
    foreach ($character in $Value.ToCharArray()) {
        if ($character -eq '\') {
            $backslashes++
            continue
        }
        if ($character -eq '"') {
            [void]$builder.Append(('\' * (($backslashes * 2) + 1)))
            [void]$builder.Append('"')
            $backslashes = 0
            continue
        }
        if ($backslashes -gt 0) {
            [void]$builder.Append(('\' * $backslashes))
            $backslashes = 0
        }
        [void]$builder.Append($character)
    }
    if ($backslashes -gt 0) {
        [void]$builder.Append(('\' * ($backslashes * 2)))
    }
    [void]$builder.Append('"')
    return $builder.ToString()
}

function Get-SemanticJsonFailures {
    param([object]$Value)

    $failures = [System.Collections.Generic.List[string]]::new()
    if ($null -eq $Value) {
        $failures.Add('JSON document is null.')
        return @($failures)
    }
    foreach ($propertyName in @('ok', 'success', 'compatible', 'valid')) {
        $property = $Value.PSObject.Properties[$propertyName]
        if ($null -ne $property -and $property.Value -is [bool] -and -not $property.Value) {
            $failures.Add("$propertyName=false")
        }
    }
    return @($failures)
}

function Add-Assertion {
    param(
        [string]$Name,
        [bool]$Passed,
        [string]$Detail
    )

    $results.Add([pscustomobject]@{
        kind = 'assertion'
        name = $Name
        argv = @()
        exit_code = if ($Passed) { 0 } else { 1 }
        expected_failure = $false
        passed = $Passed
        stdout = $Detail
        stderr = ''
        semantic_failures = @()
        parsed_json = $null
    })
}

function Add-SafetyClassification {
    param(
        [string]$Command,
        [string]$Classification,
        [string]$Rationale
    )

    $results.Add([pscustomobject]@{
        kind = 'safety-classification'
        name = "safety: $Command"
        argv = @($Command)
        exit_code = 0
        expected_failure = $false
        passed = $true
        stdout = "$Classification - $Rationale"
        stderr = ''
        semantic_failures = @()
        parsed_json = $null
    })
}

function Invoke-LdgrCase {
    param(
        [string]$Name,
        [string[]]$Arguments,
        [string]$ProjectRoot = $MainProject,
        [string]$DbPath = $MainDb,
        [string]$ArtifactRoot = $MainArtifacts,
        [switch]$Json,
        [switch]$ExpectFailure,
        [switch]$ExpectJsonFailure,
        [switch]$AllowStderr,
        [string]$ExpectedStderrPattern,
        [scriptblock]$JsonAssertion
    )

    $allArguments = @('--db', $DbPath, '--artifact-root', $ArtifactRoot) + @($Arguments)
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Exe
    $startInfo.Arguments = (($allArguments | ForEach-Object {
        ConvertTo-NativeArgument -Value ([string]$_)
    }) -join ' ')
    $startInfo.WorkingDirectory = $ProjectRoot
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $childEnvironment = $startInfo.Environment
    if ($null -eq $childEnvironment) {
        $childEnvironment = $startInfo.EnvironmentVariables
    }
    if ($null -eq $childEnvironment) {
        throw 'ProcessStartInfo did not expose a mutable child environment.'
    }
    [void]$childEnvironment.Remove('HOME')
    $childEnvironment['USERPROFILE'] = $ProfileRoot
    $childEnvironment['LOCALAPPDATA'] = (Join-Path $ProfileRoot 'AppData\Local')
    $childEnvironment['XDG_STATE_HOME'] = (Join-Path $ProfileRoot '.local\state')
    $childEnvironment['LDGR_HOME'] = (Join-Path $ProfileRoot '.ldgr')
    $childEnvironment['LDGR_ADAPTER_PATH'] = (Join-Path $ProfileRoot '.ldgr\empty-adapters')
    if (Test-Path -LiteralPath $CargoToolchainRoot -PathType Container) {
        $childEnvironment['CARGO_HOME'] = $CargoToolchainRoot
    }
    if (Test-Path -LiteralPath $RustupToolchainRoot -PathType Container) {
        $childEnvironment['RUSTUP_HOME'] = $RustupToolchainRoot
    }

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $startInfo
    [void]$process.Start()
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()
    $stdout = $stdoutTask.GetAwaiter().GetResult().Trim()
    $stderr = $stderrTask.GetAwaiter().GetResult().Trim()
    $exitCode = $process.ExitCode

    $parsedJson = $null
    $semanticFailures = @()
    $jsonParsed = $false
    if ($Json -and $stdout.Length -gt 0) {
        try {
            $parsedJson = $stdout | ConvertFrom-Json
            $jsonParsed = $true
            $semanticFailures = @(Get-SemanticJsonFailures -Value $parsedJson)
            if ($null -ne $JsonAssertion) {
                $assertionResult = & $JsonAssertion $parsedJson
                if ($assertionResult -is [bool]) {
                    if (-not $assertionResult) {
                        $semanticFailures += 'custom JSON assertion failed'
                    }
                } elseif ($null -ne $assertionResult -and ([string]$assertionResult).Length -gt 0) {
                    $semanticFailures += [string]$assertionResult
                }
            }
        } catch {
            $semanticFailures = @("JSON parse failed: $($_.Exception.Message)")
        }
    } elseif ($Json) {
        $semanticFailures = @('JSON command returned empty stdout.')
    }

    $passed = if ($ExpectFailure) {
        $exitCode -ne 0
    } elseif ($ExpectJsonFailure) {
        $exitCode -eq 0 -and $jsonParsed -and $semanticFailures.Count -gt 0
    } else {
        $exitCode -eq 0 -and (-not $Json -or ($jsonParsed -and $semanticFailures.Count -eq 0))
    }

    if ($ExpectedStderrPattern) {
        $passed = $passed -and $stderr -match $ExpectedStderrPattern
    } elseif (-not $AllowStderr -and -not $ExpectFailure) {
        $passed = $passed -and $stderr.Length -eq 0
    }

    $record = [pscustomobject]@{
        kind = 'command'
        name = $Name
        argv = @($allArguments)
        exit_code = $exitCode
        expected_failure = [bool]($ExpectFailure -or $ExpectJsonFailure)
        passed = $passed
        stdout = $stdout
        stderr = $stderr
        semantic_failures = @($semanticFailures)
        parsed_json = $parsedJson
    }
    $results.Add($record)
    return $record
}

function Write-HarnessResult {
    param([string]$FatalMessage = '')

    $failures = @($results | Where-Object { -not $_.passed })
    $expectedFailures = @($results | Where-Object { $_.expected_failure -and $_.passed })
    $classifications = @($results | Where-Object { $_.kind -eq 'safety-classification' })
    $summary = [ordered]@{
        format = 'ldgr.cli-e2e-result.v1'
        binary = $Exe
        test_root = $TestRoot
        legacy_fixture_root = $LegacyFixtureRoot
        source_root = $SourceRoot
        total_cases = $results.Count
        passed_cases = @($results | Where-Object passed).Count
        failed_cases = $failures.Count
        expected_failure_cases = $expectedFailures.Count
        safety_classification_cases = $classifications.Count
        fatal_error = $FatalMessage
        failures = @($failures | ForEach-Object {
            [ordered]@{
                name = $_.name
                argv = $_.argv
                exit_code = $_.exit_code
                stdout = $_.stdout
                stderr = $_.stderr
                semantic_failures = $_.semantic_failures
            }
        })
        cases = @($results | ForEach-Object {
            [ordered]@{
                kind = $_.kind
                name = $_.name
                argv = $_.argv
                exit_code = $_.exit_code
                expected_failure = $_.expected_failure
                passed = $_.passed
                stdout = $_.stdout
                stderr = $_.stderr
                semantic_failures = $_.semantic_failures
            }
        })
    }
    $summary | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $ResultPath -Encoding UTF8
    [pscustomobject]@{
        result = $ResultPath
        total_cases = $summary.total_cases
        passed_cases = $summary.passed_cases
        failed_cases = $summary.failed_cases
        expected_failure_cases = $summary.expected_failure_cases
        safety_classification_cases = $summary.safety_classification_cases
        fatal_error = $summary.fatal_error
    } | ConvertTo-Json -Depth 4
    return $failures.Count
}

trap {
    $message = @(
        $_.Exception.ToString()
        $_.InvocationInfo.PositionMessage
        $_.ScriptStackTrace
    ) -join [Environment]::NewLine
    Add-Assertion -Name 'harness completed without a PowerShell exception' -Passed $false -Detail $message
    $null = Write-HarnessResult -FatalMessage $message
    exit 1
}

if (-not (Test-Path -LiteralPath $Exe -PathType Leaf)) {
    throw "LDGR executable not found: $Exe"
}
if (Test-Path -LiteralPath $TestRoot) {
    throw "TestRoot must be a fresh path: $TestRoot"
}
if (-not (Test-Path -LiteralPath $LegacyFixtureRoot -PathType Container)) {
    throw "Legacy fixture root not found: $LegacyFixtureRoot"
}
if (-not (Test-Path -LiteralPath $SourceRoot -PathType Container)) {
    throw "Source root not found: $SourceRoot"
}

New-Item -ItemType Directory -Path $TestRoot, $ProfileRoot, $MainProject | Out-Null
New-Item -ItemType Directory -Path (Join-Path $ProfileRoot '.ldgr\empty-adapters') -Force | Out-Null
Copy-Item -LiteralPath (Join-Path $FixtureRoot 'schedule.json') -Destination $MainProject
Copy-Item -LiteralPath (Join-Path $FixtureRoot 'prompt-v1.md') -Destination $MainProject
Copy-Item -LiteralPath (Join-Path $FixtureRoot 'prompt-v2.md') -Destination $MainProject
Copy-Item -LiteralPath (Join-Path $FixtureRoot 'evidence.txt') -Destination $MainProject

$helpPaths = @(
    @('compatibility'),
    @('init'),
    @('install'),
    @('install', 'adapter'),
    @('work'),
    @('work', 'list'),
    @('work', 'show'),
    @('work', 'create'),
    @('work', 'edit'),
    @('work', 'dependency'),
    @('work', 'dependency', 'add'),
    @('work', 'dependency', 'remove'),
    @('work', 'graph'),
    @('work', 'audit'),
    @('work', 'status'),
    @('work', 'status', 'set'),
    @('work', 'delete'),
    @('work', 'import'),
    @('work', 'export'),
    @('notice'),
    @('notice', 'list'),
    @('notice', 'add'),
    @('notice', 'edit'),
    @('notice', 'clear'),
    @('run'),
    @('run', 'list'),
    @('run', 'show'),
    @('run', 'start'),
    @('run', 'finish'),
    @('run', 'close'),
    @('observation'),
    @('observation', 'list'),
    @('observation', 'add'),
    @('artifact'),
    @('artifact', 'list'),
    @('artifact', 'show'),
    @('artifact', 'add'),
    @('validation'),
    @('validation', 'list'),
    @('validation', 'record'),
    @('decision'),
    @('decision', 'list'),
    @('decision', 'record'),
    @('error'),
    @('error', 'record'),
    @('error', 'list'),
    @('error', 'show'),
    @('error', 'context'),
    @('error', 'occurrence'),
    @('error', 'occurrence', 'list'),
    @('error', 'occurrence', 'show'),
    @('error', 'disposition'),
    @('error', 'retry-check'),
    @('error', 'acknowledge'),
    @('error', 'resolve'),
    @('error', 'accept'),
    @('error', 'link'),
    @('prompt'),
    @('prompt', 'create'),
    @('prompt', 'import'),
    @('prompt', 'update'),
    @('prompt', 'activate'),
    @('bundle'),
    @('bundle', 'create'),
    @('bundle', 'seal'),
    @('status'),
    @('schema'),
    @('schema', 'doctor'),
    @('migrate'),
    @('workflow'),
    @('config'),
    @('config', 'show'),
    @('config', 'set'),
    @('context'),
    @('web'),
    @('loop'),
    @('loop', 'run'),
    @('adapter'),
    @('adapter', 'install'),
    @('adapter', 'update'),
    @('adapter', 'uninstall'),
    @('adapter', 'reconcile'),
    @('adapter', 'list'),
    @('adapter', 'show'),
    @('adapter', 'dispatch'),
    @('telemetry'),
    @('telemetry', 'status'),
    @('telemetry', 'preview'),
    @('telemetry', 'transmit'),
    @('telemetry', 'enable'),
    @('telemetry', 'disable'),
    @('next'),
    @('rerun')
)

$null = Invoke-LdgrCase -Name 'help: root' -Arguments @('--help')
foreach ($path in $helpPaths) {
    $label = $path -join ' '
    $null = Invoke-LdgrCase -Name "help: $label" -Arguments @($path + '--help')
}

$full = Invoke-LdgrCase -Name 'root full command map' -Arguments @('--full')
$expectedCommandPaths = @($helpPaths | ForEach-Object { $_ -join ' ' })
$missingFromFull = @($expectedCommandPaths | Where-Object {
    $commandPath = $_
    -not (($full.stdout -split "`r?`n") | Where-Object {
        $_ -match ('^\s{2}' + [regex]::Escape($commandPath) + '(?:\s|$)')
    })
})
Add-Assertion -Name 'full command map contains every maintained public path' `
    -Passed ($missingFromFull.Count -eq 0) `
    -Detail $(if ($missingFromFull.Count -eq 0) { 'No omissions found.' } else { $missingFromFull -join ', ' })

Add-SafetyClassification -Command 'compatibility' -Classification 'environment-dependent read-only' `
    -Rationale 'Help is tested; live agentctl negotiation belongs to the paired-release matrix.'
Add-SafetyClassification -Command 'web' -Classification 'long-running local service' `
    -Rationale 'Help is tested; starting a listener is excluded from the bounded CLI process matrix.'
Add-SafetyClassification -Command 'loop run (non-dry)' -Classification 'external process launch' `
    -Rationale 'Stored prompt and bundle dry-runs are tested without launching an agent.'
Add-SafetyClassification -Command 'adapter install (release)' -Classification 'network and trust-store mutation' `
    -Rationale 'The local-source lifecycle is tested; signed release delivery has a separate release gate.'
Add-SafetyClassification -Command 'dynamic adapter namespace execution' -Classification 'adapter-owned external process launch' `
    -Rationale 'Core install/show/dispatch metadata/lifecycle paths are tested; an adapter command may require host compiler and linker state.'
Add-SafetyClassification -Command 'install agentctl' -Classification 'network and user-toolchain mutation' `
    -Rationale 'Install is tested with --no-agentctl inside an isolated profile.'
Add-SafetyClassification -Command 'telemetry transmit (successful remote delivery)' -Classification 'external network write' `
    -Rationale 'Disabled/no-pending and invalid-transport behavior are covered locally.'
Add-SafetyClassification -Command 'error acknowledge/error accept' -Classification 'stateful disposition shortcuts' `
    -Rationale 'Help and the canonical disposition/resolve lifecycle are tested without leaving accepted fixture state.'

$null = Invoke-LdgrCase -Name 'version' -Arguments @('--version')
$null = Invoke-LdgrCase -Name 'help command' -Arguments @('help', 'work')
$null = Invoke-LdgrCase -Name 'init current project' -Arguments @('init')
$null = Invoke-LdgrCase -Name 'init is idempotent' -Arguments @('init')

$statusJson = Invoke-LdgrCase -Name 'status json empty project' -Arguments @('status', '--json') -Json `
    -JsonAssertion { param($value) $null -ne $value.work_items }
$null = Invoke-LdgrCase -Name 'status human' -Arguments @('status')
$null = Invoke-LdgrCase -Name 'status full filtered' -Arguments @('status', '--full', '--program', 'e2e', '--priority', 'P0')
$null = Invoke-LdgrCase -Name 'context human' -Arguments @('context')
$null = Invoke-LdgrCase -Name 'context brief' -Arguments @('context', '--brief', '--recent', '2', '--width', '100')
$null = Invoke-LdgrCase -Name 'context json' -Arguments @('context', '--json') -Json
$null = Invoke-LdgrCase -Name 'next empty' -Arguments @('next')
$null = Invoke-LdgrCase -Name 'next commands empty' -Arguments @('next', '--commands')
$null = Invoke-LdgrCase -Name 'workflow human' -Arguments @('workflow')
$null = Invoke-LdgrCase -Name 'workflow json' -Arguments @('workflow', '--json') -Json
$null = Invoke-LdgrCase -Name 'schema doctor human' -Arguments @('schema', 'doctor')
$null = Invoke-LdgrCase -Name 'schema doctor json' -Arguments @('schema', 'doctor', '--json') -Json `
    -JsonAssertion { param($value) $value.compatible -and $value.active_schema_version -eq 5 }
$null = Invoke-LdgrCase -Name 'migrate no-op human' -Arguments @('migrate')
$null = Invoke-LdgrCase -Name 'migrate no-op json' -Arguments @('migrate', '--json') -Json `
    -JsonAssertion { param($value) $value.to_schema_version -eq 5 }

$null = Invoke-LdgrCase -Name 'config show human before install' -Arguments @('config', 'show')
$null = Invoke-LdgrCase -Name 'config show json before install' -Arguments @('config', 'show', '--json') -Json
$null = Invoke-LdgrCase -Name 'config set interview depth' -Arguments @('config', 'set', 'interview-depth', 'low')
$null = Invoke-LdgrCase -Name 'install isolated harnesses' -Arguments @(
    'install', '--harness', 'codex', '--harness', 'claude', '--yes',
    '--telemetry', 'disable', '--no-agentctl', '--interview-depth', 'low'
)
Add-Assertion -Name 'install wrote canonical toml' `
    -Passed (Test-Path -LiteralPath (Join-Path $ProfileRoot '.ldgr\config.toml')) `
    -Detail 'Checked isolated USERPROFILE.'
Add-Assertion -Name 'install wrote compatibility json' `
    -Passed (Test-Path -LiteralPath (Join-Path $ProfileRoot '.ldgr\config.json')) `
    -Detail 'Checked isolated USERPROFILE.'
$null = Invoke-LdgrCase -Name 'config show after install' -Arguments @('config', 'show', '--json') -Json

$noticeAdd = Invoke-LdgrCase -Name 'notice add' -Arguments @(
    'notice', 'add', '--kind', 'notification', '--body', 'E2E steering notice.', '--source', 'cli-e2e'
)
$noticeId = if ($noticeAdd.stdout -match '(\d+)\s*$') { $Matches[1] } else { 'unparsed' }
Add-Assertion -Name 'notice id parsed' -Passed ($noticeId -ne 'unparsed') -Detail $noticeAdd.stdout
$null = Invoke-LdgrCase -Name 'notice edit' -Arguments @(
    'notice', 'edit', $noticeId, '--body', 'E2E steering notice updated.', '--clear-source'
)
$null = Invoke-LdgrCase -Name 'notice list json' -Arguments @('notice', 'list', '--status', 'all', '--json') -Json
$null = Invoke-LdgrCase -Name 'notice clear' -Arguments @('notice', 'clear', $noticeId, '--reason', 'E2E complete')
$null = Invoke-LdgrCase -Name 'notice list cleared' -Arguments @('notice', 'list', '--status', 'cleared')

$null = Invoke-LdgrCase -Name 'work create base' -Arguments @(
    'work', 'create', 'base', '--title', 'Base', '--description', 'Base E2E work.',
    '--priority', 'P0', '--program', 'e2e', '--group', 'core',
    '--acceptance-criteria', 'CLI evidence exists.'
)
$null = Invoke-LdgrCase -Name 'work create child with dependency' -Arguments @(
    'work', 'create', 'child', '--title', 'Child', '--description', 'Dependent E2E work.',
    '--priority', 'high', '--program', 'e2e', '--depends-on', 'base'
)
$null = Invoke-LdgrCase -Name 'work create gate' -Arguments @(
    'work', 'create', 'gate', '--title', 'Gate', '--description', 'Temporary schedule gate.'
)
$null = Invoke-LdgrCase -Name 'work create deletable' -Arguments @(
    'work', 'create', 'deletable', '--title', 'Delete me', '--description', 'Deletion command fixture.'
)
$null = Invoke-LdgrCase -Name 'work edit' -Arguments @(
    'work', 'edit', 'base', '--title', 'Base updated', '--description', 'Base E2E work updated.',
    '--priority', 'high', '--group', 'edited', '--acceptance-criteria', 'Updated evidence exists.'
)
$null = Invoke-LdgrCase -Name 'work dependency add' -Arguments @('work', 'dependency', 'add', 'child', 'gate')
$null = Invoke-LdgrCase -Name 'work dependency remove' -Arguments @('work', 'dependency', 'remove', 'child', 'gate')
$null = Invoke-LdgrCase -Name 'work status held' -Arguments @(
    'work', 'status', 'set', 'gate', 'held', '--reason', 'Testing hold state.', '--hold-kind', 'blocked'
)
$null = Invoke-LdgrCase -Name 'work status pending' -Arguments @(
    'work', 'status', 'set', 'gate', 'pending', '--reason', 'Testing resume state.'
)
$null = Invoke-LdgrCase -Name 'work list human' -Arguments @('work', 'list')
$null = Invoke-LdgrCase -Name 'work list json filtered' -Arguments @(
    'work', 'list', '--program', 'e2e', '--json'
) -Json
$null = Invoke-LdgrCase -Name 'work show human' -Arguments @('work', 'show', 'base')
$null = Invoke-LdgrCase -Name 'work show json' -Arguments @('work', 'show', 'base', '--json') -Json
$null = Invoke-LdgrCase -Name 'work graph human' -Arguments @('work', 'graph')
$null = Invoke-LdgrCase -Name 'work graph json' -Arguments @('work', 'graph', '--format', 'json') -Json
$null = Invoke-LdgrCase -Name 'work graph mermaid' -Arguments @('work', 'graph', '--format', 'mermaid')
$null = Invoke-LdgrCase -Name 'work export example' -Arguments @('work', 'export', '--example') -Json
$null = Invoke-LdgrCase -Name 'work export stdout' -Arguments @('work', 'export', '--program', 'e2e') -Json
$exportPath = Join-Path $MainProject 'exported-schedule.json'
$null = Invoke-LdgrCase -Name 'work export file' -Arguments @('work', 'export', '--output', $exportPath)
Add-Assertion -Name 'work export file exists' -Passed (Test-Path -LiteralPath $exportPath) -Detail $exportPath
$schedulePath = Join-Path $MainProject 'schedule.json'
$null = Invoke-LdgrCase -Name 'work import dry run' -Arguments @('work', 'import', $schedulePath, '--dry-run')
$null = Invoke-LdgrCase -Name 'work import' -Arguments @('work', 'import', $schedulePath)
$null = Invoke-LdgrCase -Name 'work import upsert' -Arguments @('work', 'import', $schedulePath, '--upsert')
$null = Invoke-LdgrCase -Name 'work delete' -Arguments @('work', 'delete', 'deletable')
$null = Invoke-LdgrCase -Name 'work show deleted expected error' `
    -Arguments @('work', 'show', 'deletable') -ExpectFailure

$inlineBody = 'Inline prompt context: {{ldgr_context}}'
$null = Invoke-LdgrCase -Name 'prompt create' -Arguments @(
    'prompt', 'create', 'inline', '--role', 'inline-loop', '--body', $inlineBody,
    '--description', 'Inline E2E prompt.'
)
$null = Invoke-LdgrCase -Name 'prompt activate inline' -Arguments @('prompt', 'activate', 'inline')
$null = Invoke-LdgrCase -Name 'prompt import' -Arguments @(
    'prompt', 'import', 'file', '--role', 'file-loop', '--path',
    (Join-Path $MainProject 'prompt-v1.md'), '--description', 'File E2E prompt.'
)
$null = Invoke-LdgrCase -Name 'prompt update' -Arguments @(
    'prompt', 'update', 'file', '--path', (Join-Path $MainProject 'prompt-v2.md'),
    '--description', 'Updated E2E prompt.'
)
$null = Invoke-LdgrCase -Name 'prompt activate file' -Arguments @('prompt', 'activate', 'file')
$null = Invoke-LdgrCase -Name 'bundle create' -Arguments @(
    'bundle', 'create', 'suite', '--prompt', 'inline', '--prompt', 'file'
)
$null = Invoke-LdgrCase -Name 'bundle seal' -Arguments @('bundle', 'seal', 'suite')
$null = Invoke-LdgrCase -Name 'loop dry run stored prompt' `
    -Arguments @('loop', 'run', '--prompt-slug', 'inline', '--dry-run')
$null = Invoke-LdgrCase -Name 'loop dry run bundle' `
    -Arguments @('loop', 'run', '--bundle', 'suite', '--prompt-role', 'file-loop', '--dry-run')

$null = Invoke-LdgrCase -Name 'run start' -Arguments @('run', 'start', 'base', '--command', 'e2e base command')
$null = Invoke-LdgrCase -Name 'run list human' -Arguments @('run', 'list')
$null = Invoke-LdgrCase -Name 'run list json running' -Arguments @('run', 'list', '--status', 'running', '--json') -Json
$null = Invoke-LdgrCase -Name 'run show human by slug' -Arguments @('run', 'show', 'base')
$null = Invoke-LdgrCase -Name 'run show json by slug' -Arguments @('run', 'show', 'base', '--json') -Json
$null = Invoke-LdgrCase -Name 'observation add' -Arguments @(
    'observation', 'add', 'base', '--body', 'Base observation.'
)
$null = Invoke-LdgrCase -Name 'observation alias direct add' -Arguments @(
    'observe', 'base', '--body', 'Alias observation.'
)
$null = Invoke-LdgrCase -Name 'observation list human' -Arguments @('observation', 'list', '--run-id', 'base')
$null = Invoke-LdgrCase -Name 'observation list json' -Arguments @(
    'observation', 'list', '--run-id', 'base', '--json'
) -Json
$artifactAdd = Invoke-LdgrCase -Name 'artifact add' -Arguments @(
    'artifact', 'add', 'base', '--kind', 'report', '--path',
    (Join-Path $MainProject 'evidence.txt'), '--description', 'Benign E2E evidence fixture.'
)
$artifactId = if ($artifactAdd.stdout -match 'added artifact\s+(\d+)') { $Matches[1] } else { 'unparsed' }
Add-Assertion -Name 'artifact id parsed' -Passed ($artifactId -ne 'unparsed') -Detail $artifactAdd.stdout
$null = Invoke-LdgrCase -Name 'artifact list human' -Arguments @('artifact', 'list', '--run-id', 'base')
$null = Invoke-LdgrCase -Name 'artifact list json' -Arguments @(
    'artifact', 'list', '--run-id', 'base', '--json'
) -Json
$null = Invoke-LdgrCase -Name 'artifact show human' -Arguments @('artifact', 'show', $artifactId)
$null = Invoke-LdgrCase -Name 'artifact show json' -Arguments @('artifact', 'show', $artifactId, '--json') -Json
$null = Invoke-LdgrCase -Name 'validation record pass' -Arguments @(
    'validation', 'record', 'base', '--outcome', 'pass', '--command', 'e2e synthetic check',
    '--rationale', 'Command path exercised.'
)
$null = Invoke-LdgrCase -Name 'validation record skipped' -Arguments @(
    'validation', 'record', 'base', '--outcome', 'skipped', '--rationale', 'Intentional E2E skip record.'
)
$null = Invoke-LdgrCase -Name 'validation skipped without rationale rejected' `
    -Arguments @('validation', 'record', 'base', '--outcome', 'skipped') -ExpectFailure
$null = Invoke-LdgrCase -Name 'validation list human' -Arguments @('validation', 'list', '--run-id', 'base')
$null = Invoke-LdgrCase -Name 'validation list json' -Arguments @(
    'validation', 'list', '--run-id', 'base', '--json'
) -Json
$null = Invoke-LdgrCase -Name 'run finish' -Arguments @(
    'run', 'finish', 'base', '--status', 'success', '--notes', 'Base run finished.'
)
$null = Invoke-LdgrCase -Name 'decision record continue existing' -Arguments @(
    'decision', 'record', 'base', '--outcome', 'continue', '--rationale', 'Proceed to child.',
    '--next-slug', 'child'
)
$null = Invoke-LdgrCase -Name 'decision list human' -Arguments @('decision', 'list', '--work-slug', 'base')
$null = Invoke-LdgrCase -Name 'decision list json' -Arguments @(
    'decision', 'list', '--work-slug', 'base', '--json'
) -Json

$null = Invoke-LdgrCase -Name 'work delete imported child' -Arguments @('work', 'delete', 'imported-child')
$null = Invoke-LdgrCase -Name 'work delete imported base' -Arguments @('work', 'delete', 'imported-base')
$null = Invoke-LdgrCase -Name 'work delete gate' -Arguments @('work', 'delete', 'gate')
$null = Invoke-LdgrCase -Name 'run start child' -Arguments @(
    'run', 'start', 'child', '--command', 'e2e child command'
)
$null = Invoke-LdgrCase -Name 'run close continue create next' -Arguments @(
    'run', 'close', 'child', '--status', 'success', '--outcome', 'continue',
    '--rationale', 'Exercise atomic close.', '--next-slug', 'final',
    '--next-title', 'Final', '--next-description', 'Final E2E decision slice.'
)
$null = Invoke-LdgrCase -Name 'work dependency final after child' `
    -Arguments @('work', 'dependency', 'add', 'final', 'child')
$null = Invoke-LdgrCase -Name 'run start final' -Arguments @(
    'run', 'start', 'final', '--command', 'e2e partial command'
)
$null = Invoke-LdgrCase -Name 'run finish partial' -Arguments @(
    'run', 'finish', 'final', '--status', 'partial', '--notes', 'Intentional partial state.'
)
$null = Invoke-LdgrCase -Name 'decision record inconclusive create retry' -Arguments @(
    'decision', 'record', 'final', '--outcome', 'inconclusive',
    '--rationale', 'Exercise inconclusive transition.', '--next-slug', 'retry',
    '--next-title', 'Retry', '--next-description', 'Final successful E2E slice.'
)
$null = Invoke-LdgrCase -Name 'work dependency retry after final' `
    -Arguments @('work', 'dependency', 'add', 'retry', 'final')
$null = Invoke-LdgrCase -Name 'run start retry' -Arguments @(
    'run', 'start', 'retry', '--command', 'e2e retry command'
)
$null = Invoke-LdgrCase -Name 'run close stop' -Arguments @(
    'run', 'close', 'retry', '--status', 'success', '--outcome', 'stop',
    '--rationale', 'All isolated E2E work is terminal.'
)

$null = Invoke-LdgrCase -Name 'telemetry status' -Arguments @('telemetry', 'status')
$null = Invoke-LdgrCase -Name 'telemetry preview disabled' -Arguments @('telemetry', 'preview')
$null = Invoke-LdgrCase -Name 'telemetry enable' -Arguments @('telemetry', 'enable')
$null = Invoke-LdgrCase -Name 'telemetry preview enabled' -Arguments @('telemetry', 'preview')
$null = Invoke-LdgrCase -Name 'telemetry transmit no pending arrays' -Arguments @(
    'telemetry', 'transmit', '--collector', 'https://127.0.0.1:9',
    '--max-delay-ms', '0', '--timeout-ms', '50'
)
$null = Invoke-LdgrCase -Name 'telemetry reject non-https collector' -Arguments @(
    'telemetry', 'transmit', '--collector', 'http://127.0.0.1:9',
    '--max-delay-ms', '0', '--timeout-ms', '50'
) -ExpectFailure
$null = Invoke-LdgrCase -Name 'telemetry disable' -Arguments @('telemetry', 'disable')

$null = Invoke-LdgrCase -Name 'adapter install catalog list' -Arguments @('adapter', 'install', 'list')
$null = Invoke-LdgrCase -Name 'adapter list empty json' -Arguments @('adapter', 'list', '--json') -Json
$null = Invoke-LdgrCase -Name 'adapter show absent expected error' `
    -Arguments @('adapter', 'show', 'example', '--json') -ExpectFailure
$null = Invoke-LdgrCase -Name 'adapter dispatch absent expected error' `
    -Arguments @('adapter', 'dispatch', 'example-manifest-summary', '--json') -ExpectFailure
$null = Invoke-LdgrCase -Name 'adapter install example from source' -Arguments @(
    'adapter', 'install', 'example', '--source-root', $SourceRoot, '--yes'
) -AllowStderr
$null = Invoke-LdgrCase -Name 'adapter list installed json' -Arguments @('adapter', 'list', '--json') -Json
$null = Invoke-LdgrCase -Name 'adapter show installed json' -Arguments @('adapter', 'show', 'example', '--json') -Json
$null = Invoke-LdgrCase -Name 'adapter dispatch metadata' `
    -Arguments @('adapter', 'dispatch', 'example-manifest-summary', '--json') -Json
$null = Invoke-LdgrCase -Name 'adapter update check' -Arguments @('adapter', 'update', 'example', '--check')
$null = Invoke-LdgrCase -Name 'adapter reconcile one' -Arguments @('adapter', 'reconcile', 'example')
$null = Invoke-LdgrCase -Name 'adapter reconcile all' -Arguments @('adapter', 'reconcile')
$null = Invoke-LdgrCase -Name 'adapter uninstall' -Arguments @('adapter', 'uninstall', 'example')
$null = Invoke-LdgrCase -Name 'install adapter compatibility command' -Arguments @(
    'install', 'adapter', 'example', '--source-root', $SourceRoot, '--yes'
) -AllowStderr
$null = Invoke-LdgrCase -Name 'adapter uninstall compatibility install' `
    -Arguments @('adapter', 'uninstall', 'example')

$AuditProject = Join-Path $TestRoot 'intentional-audit'
$AuditDb = Join-Path $AuditProject '.ldgr\ldgr.db'
$AuditArtifacts = Join-Path $AuditProject '.ldgr\artifacts'
New-Item -ItemType Directory -Path $AuditProject | Out-Null
$null = Invoke-LdgrCase -Name 'audit fixture init' -Arguments @('init') `
    -ProjectRoot $AuditProject -DbPath $AuditDb -ArtifactRoot $AuditArtifacts
$null = Invoke-LdgrCase -Name 'audit fixture base' -Arguments @(
    'work', 'create', 'audit-base', '--title', 'Audit base', '--description', 'Intentional finding base.'
) -ProjectRoot $AuditProject -DbPath $AuditDb -ArtifactRoot $AuditArtifacts
$null = Invoke-LdgrCase -Name 'audit fixture child' -Arguments @(
    'work', 'create', 'audit-child', '--title', 'Audit child',
    '--description', 'Intentional finding child.', '--depends-on', 'audit-base'
) -ProjectRoot $AuditProject -DbPath $AuditDb -ArtifactRoot $AuditArtifacts
$null = Invoke-LdgrCase -Name 'audit fixture canceled dependency' -Arguments @(
    'work', 'status', 'set', 'audit-base', 'canceled', '--reason', 'Create isolated audit finding.'
) -ProjectRoot $AuditProject -DbPath $AuditDb -ArtifactRoot $AuditArtifacts
$null = Invoke-LdgrCase -Name 'intentional audit finding human output' `
    -Arguments @('work', 'audit') -ProjectRoot $AuditProject -DbPath $AuditDb `
    -ArtifactRoot $AuditArtifacts
$null = Invoke-LdgrCase -Name 'intentional audit finding is semantic failure' `
    -Arguments @('work', 'audit', '--json') -ProjectRoot $AuditProject -DbPath $AuditDb `
    -ArtifactRoot $AuditArtifacts -Json -ExpectJsonFailure

$ErrorProject = Join-Path $TestRoot 'error-lifecycle'
$ErrorDb = Join-Path $ErrorProject '.ldgr\ldgr.db'
$ErrorArtifacts = Join-Path $ErrorProject '.ldgr\artifacts'
New-Item -ItemType Directory -Path $ErrorProject | Out-Null
$null = Invoke-LdgrCase -Name 'error fixture init' -Arguments @('init') `
    -ProjectRoot $ErrorProject -DbPath $ErrorDb -ArtifactRoot $ErrorArtifacts
$errorRecord = Invoke-LdgrCase -Name 'error record json' -Arguments @(
    'error', 'record',
    '--occurrence-id', '0198f100-0000-7000-8000-000000000508',
    '--producer', 'cli-e2e',
    '--idempotency-key', 'cli-e2e:error-1',
    '--operation-id', '0198f100-0000-7000-8000-000000000508',
    '--attempt-id', '0198f100-0000-7000-8000-000000000509',
    '--boundary', 'validation',
    '--component', 'cli-e2e',
    '--subject', 'error-lifecycle',
    '--class', 'validation-failure',
    '--domain', 'test.cli-e2e',
    '--code', 'expected-fixture',
    '--severity', 'error',
    '--retryability', 'after-change',
    '--source', 'cli-e2e',
    '--summary', 'Intentional isolated error lifecycle fixture.',
    '--observed-at', '2026-07-31T00:00:00Z',
    '--json'
) -ProjectRoot $ErrorProject -DbPath $ErrorDb -ArtifactRoot $ErrorArtifacts -Json
$errorId = if ($null -ne $errorRecord.parsed_json) { [string]$errorRecord.parsed_json.error.id } else { 'unparsed' }
Add-Assertion -Name 'error id parsed' -Passed ($errorId -ne 'unparsed') -Detail $errorRecord.stdout
$null = Invoke-LdgrCase -Name 'error list json' -Arguments @('error', 'list', '--json') `
    -ProjectRoot $ErrorProject -DbPath $ErrorDb -ArtifactRoot $ErrorArtifacts -Json
$null = Invoke-LdgrCase -Name 'error show json' -Arguments @('error', 'show', $errorId, '--json') `
    -ProjectRoot $ErrorProject -DbPath $ErrorDb -ArtifactRoot $ErrorArtifacts -Json
$null = Invoke-LdgrCase -Name 'error context json' -Arguments @('error', 'context', $errorId, '--json') `
    -ProjectRoot $ErrorProject -DbPath $ErrorDb -ArtifactRoot $ErrorArtifacts -Json
$null = Invoke-LdgrCase -Name 'error occurrence list json' `
    -Arguments @('error', 'occurrence', 'list', '--error-id', $errorId, '--json') `
    -ProjectRoot $ErrorProject -DbPath $ErrorDb -ArtifactRoot $ErrorArtifacts -Json
$null = Invoke-LdgrCase -Name 'error occurrence show json' `
    -Arguments @('error', 'occurrence', 'show', '0198f100-0000-7000-8000-000000000508', '--json') `
    -ProjectRoot $ErrorProject -DbPath $ErrorDb -ArtifactRoot $ErrorArtifacts -Json
$null = Invoke-LdgrCase -Name 'error retry-check rejected before disposition' `
    -Arguments @('error', 'retry-check', $errorId, '--json') `
    -ProjectRoot $ErrorProject -DbPath $ErrorDb -ArtifactRoot $ErrorArtifacts -ExpectFailure
$null = Invoke-LdgrCase -Name 'error link json' -Arguments @(
    'error', 'link', $errorId, '--kind', 'related',
    '--entity-type', 'external', '--entity-id', 'cli-e2e:fixture',
    '--source', 'cli-e2e', '--json'
) -ProjectRoot $ErrorProject -DbPath $ErrorDb -ArtifactRoot $ErrorArtifacts -Json
$null = Invoke-LdgrCase -Name 'error disposition workaround json' -Arguments @(
    'error', 'disposition', $errorId, '--action', 'workaround',
    '--actor', 'cli-e2e', '--source', 'cli-e2e',
    '--rationale', 'Exercise the canonical disposition command.', '--json'
) -ProjectRoot $ErrorProject -DbPath $ErrorDb -ArtifactRoot $ErrorArtifacts -Json
$null = Invoke-LdgrCase -Name 'error resolve shortcut json' -Arguments @(
    'error', 'resolve', $errorId, '--actor', 'cli-e2e', '--source', 'cli-e2e',
    '--rationale', 'Close the isolated error fixture.', '--json'
) -ProjectRoot $ErrorProject -DbPath $ErrorDb -ArtifactRoot $ErrorArtifacts -Json

$RerunProject = Join-Path $TestRoot 'rerun-receipt'
$RerunDb = Join-Path $RerunProject '.ldgr\ldgr.db'
$RerunArtifacts = Join-Path $RerunProject '.ldgr\artifacts'
New-Item -ItemType Directory -Path $RerunProject | Out-Null
$null = Invoke-LdgrCase -Name 'rerun fixture init' -Arguments @('init') `
    -ProjectRoot $RerunProject -DbPath $RerunDb -ArtifactRoot $RerunArtifacts
$null = Invoke-LdgrCase -Name 'unique typo produces rerun receipt' -Arguments @('statuz', '--json') `
    -ProjectRoot $RerunProject -DbPath $RerunDb -ArtifactRoot $RerunArtifacts -ExpectFailure
$null = Invoke-LdgrCase -Name 'rerun executes correction' -Arguments @('rerun') `
    -ProjectRoot $RerunProject -DbPath $RerunDb -ArtifactRoot $RerunArtifacts
$null = Invoke-LdgrCase -Name 'rerun is one shot' -Arguments @('rerun') `
    -ProjectRoot $RerunProject -DbPath $RerunDb -ArtifactRoot $RerunArtifacts -ExpectFailure

foreach ($version in 1..4) {
    $fixtureProject = Join-Path $LegacyFixtureRoot "v$version"
    $expectedId = 4500 + $version
    foreach ($entrypoint in @('init', 'status', 'context')) {
        $scenarioProject = Join-Path $TestRoot "migration-v$version-$entrypoint"
        New-Item -ItemType Directory -Path $scenarioProject | Out-Null
        Copy-Item -LiteralPath (Join-Path $fixtureProject '.ldgr') -Destination $scenarioProject -Recurse
        $scenarioDb = Join-Path $scenarioProject '.ldgr\ldgr.db'
        $scenarioArtifacts = Join-Path $scenarioProject '.ldgr\artifacts'
        $entrypointArguments = if ($entrypoint -eq 'init') {
            @('init')
        } else {
            @($entrypoint, '--json')
        }
        $null = Invoke-LdgrCase -Name "migration v$version through $entrypoint" `
            -Arguments $entrypointArguments -ProjectRoot $scenarioProject -DbPath $scenarioDb `
            -ArtifactRoot $scenarioArtifacts -Json:($entrypoint -ne 'init') `
            -ExpectedStderrPattern "migration: LDGR Core upgraded schema v$version -> v5; verified backup:"
        $null = Invoke-LdgrCase -Name "migration v$version $entrypoint schema doctor" `
            -Arguments @('schema', 'doctor', '--json') -ProjectRoot $scenarioProject `
            -DbPath $scenarioDb -ArtifactRoot $scenarioArtifacts -Json `
            -JsonAssertion { param($value) $value.compatible -and $value.active_schema_version -eq 5 }
        $null = Invoke-LdgrCase -Name "migration v$version $entrypoint preserved causal id" `
            -Arguments @('work', 'show', "preserved-v$version", '--json') `
            -ProjectRoot $scenarioProject -DbPath $scenarioDb -ArtifactRoot $scenarioArtifacts -Json `
            -JsonAssertion { param($value) $value.id -eq $expectedId }
        $null = Invoke-LdgrCase -Name "migration v$version $entrypoint error schema usable" `
            -Arguments @('error', 'list', '--json') -ProjectRoot $scenarioProject `
            -DbPath $scenarioDb -ArtifactRoot $scenarioArtifacts -Json
        $backupDatabases = @(Get-ChildItem -LiteralPath (Join-Path $scenarioProject '.ldgr') -File |
            Where-Object { $_.Name -like "*backup-schema-v$version-to-v5*.sqlite3" })
        $backupMetadata = @(Get-ChildItem -LiteralPath (Join-Path $scenarioProject '.ldgr') -File |
            Where-Object { $_.Name -like "*backup-schema-v$version-to-v5*.json" })
        Add-Assertion -Name "migration v$version $entrypoint preserved verified backup evidence" `
            -Passed ($backupDatabases.Count -eq 1 -and $backupMetadata.Count -eq 1) `
            -Detail "databases=$($backupDatabases.Count) metadata=$($backupMetadata.Count)"
    }
}

$null = Invoke-LdgrCase -Name 'final status human' -Arguments @('status', '--full')
$finalStatusJson = Invoke-LdgrCase -Name 'final status json' -Arguments @('status', '--json') -Json `
    -JsonAssertion {
        param($value)
        $value.work_items.pending -eq 0 -and
        $value.work_items.running -eq 0 -and
        $value.work_items.held -eq 0 -and
        $null -eq $value.next -and
        $value.errors.counts.unresolved -eq 0 -and
        $value.errors.counts.disposition_pending -eq 0
    }
$null = Invoke-LdgrCase -Name 'final schema doctor clean' -Arguments @('schema', 'doctor', '--json') -Json `
    -JsonAssertion {
        param($value)
        $value.compatible -and
        $value.active_schema_version -eq 5 -and
        @($value.pending_migrations).Count -eq 0 -and
        $null -eq $value.problem
    }
$null = Invoke-LdgrCase -Name 'final work audit clean' -Arguments @('work', 'audit', '--json') -Json `
    -JsonAssertion { param($value) $value.ok -and @($value.findings).Count -eq 0 }

$failureCount = Write-HarnessResult
if ($failureCount -gt 0) {
    exit 1
}
