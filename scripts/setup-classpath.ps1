#!/usr/bin/env pwsh
<#
.SYNOPSIS
  Generate <workspace>/.ld-java-analyze/classpath.txt for ld-java-analyze.

.DESCRIPTION
  Run from the root of the Maven project that VS Code opens as workspace.
  The current directory becomes the workspace root; every Maven output is
  written there with an absolute path, so multi-module reactors never scatter
  classpath.txt into submodules.

  Steps: create the state directory, install reactor SNAPSHOT modules,
  download sources jars, then keep the widest module classpath.

.PARAMETER Module
  Final module to use. When omitted, every module declared in the root pom is
  probed and the one with the most jars wins.

.PARAMETER SkipInstall
  Skip `mvn -U install`. Use when internal SNAPSHOT modules are already installed.

.PARAMETER SkipSources
  Skip `mvn dependency:sources`.

.PARAMETER DryRun
  Print the commands without running Maven or writing files.

.EXAMPLE
  cd E:\projects\vsjavapro\demo1
  & E:\projects\vsjavapro\demo1\ld_java_analyze\scripts\setup-classpath.ps1

.EXAMPLE
  cd E:\projects\vsjavapro\demo1
  & .\scripts\setup-classpath.ps1 -Module demo1-web -DryRun
#>
[CmdletBinding()]
param(
    [string] $Module = '',
    [switch] $SkipInstall,
    [switch] $SkipSources,
    [switch] $DryRun
)

$ErrorActionPreference = 'Stop'

$root = (Get-Location).Path
$stateDir = Join-Path $root '.ld-java-analyze'
$classpath = Join-Path $stateDir 'classpath.txt'

function Assert-Maven {
    if (-not (Get-Command mvn -ErrorAction SilentlyContinue)) {
        throw 'mvn not found in PATH'
    }
}

function Invoke-Maven {
    param(
        [string] $Label,
        [string[]] $Arguments
    )
    Write-Host "==> $Label"
    Write-Host "    mvn $($Arguments -join ' ')"
    if ($DryRun) {
        return
    }
    & mvn @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Label failed (exit $LASTEXITCODE)"
    }
}

function Get-Jars {
    param([string] $File)
    if (-not (Test-Path -LiteralPath $File)) {
        return @()
    }
    $text = Get-Content -LiteralPath $File -Raw -Encoding UTF8
    return @(
        $text -split '[;:]' |
            ForEach-Object { $_.Trim() } |
            Where-Object { $_ -and -not $_.StartsWith('#') }
    )
}

function Get-DeclaredModules {
    $pom = Join-Path $root 'pom.xml'
    if (-not (Test-Path -LiteralPath $pom)) {
        throw "pom.xml not found in $root - run this script from the project root"
    }
    [xml] $doc = Get-Content -LiteralPath $pom -Raw -Encoding UTF8
    return @(
        $doc.SelectNodes("//*[local-name()='module']") |
            ForEach-Object { $_.InnerText.Trim() } |
            Where-Object { $_ }
    )
}

Assert-Maven
Write-Host "Project root : $root"
Write-Host "State dir    : $stateDir"
Write-Host "Output       : $classpath"
if ($DryRun) {
    Write-Host 'Mode         : dry run (no Maven calls, no files written)'
}

if (-not $DryRun) {
    New-Item -ItemType Directory -Force -Path $stateDir | Out-Null
}

if (-not $SkipInstall) {
    # -U clears Maven's cached "resolution failed" markers.
    Invoke-Maven 'Install reactor SNAPSHOT modules' @('-q', '-U', 'install', '-DskipTests')
}
if (-not $SkipSources) {
    Invoke-Maven 'Download sources jars' @('-q', 'dependency:sources')
}

$modules = Get-DeclaredModules

if (-not $Module -and $modules.Count -eq 0) {
    # Single-module project: no -pl needed.
    Invoke-Maven 'Build classpath (single module)' @(
        '-q', 'dependency:build-classpath', "-Dmdep.outputFile=$classpath"
    )
}
elseif ($Module) {
    Invoke-Maven "Build classpath for $Module" @(
        '-q', '-pl', $Module, 'dependency:build-classpath', "-Dmdep.outputFile=$classpath"
    )
}
else {
    # Probe every declared module, keep the widest classpath. Temporary files
    # use absolute paths so nothing is written into submodule directories.
    $bestFile = $null
    $bestCount = -1
    foreach ($name in $modules) {
        $safe = $name -replace '[\\/:]', '_'
        $probe = Join-Path $stateDir ".probe-$safe.txt"
        try {
            Invoke-Maven "Probe $name" @(
                '-q', '-pl', $name, 'dependency:build-classpath', "-Dmdep.outputFile=$probe"
            )
        }
        catch {
            Write-Warning "Skipping $name : $_"
            Remove-Item -LiteralPath $probe -Force -ErrorAction SilentlyContinue
            continue
        }
        if ($DryRun) {
            continue
        }
        $count = @(Get-Jars $probe).Count
        Write-Host "    $name : $count jars"
        if ($count -gt $bestCount) {
            if ($bestFile) {
                Remove-Item -LiteralPath $bestFile -Force -ErrorAction SilentlyContinue
            }
            $bestFile = $probe
            $bestCount = $count
        }
        else {
            Remove-Item -LiteralPath $probe -Force -ErrorAction SilentlyContinue
        }
    }
    if (-not $DryRun) {
        if (-not $bestFile) {
            throw 'No module produced a classpath'
        }
        Move-Item -LiteralPath $bestFile -Destination $classpath -Force
        Write-Host "==> Widest module classpath kept ($bestCount jars)"
    }
}

if ($DryRun) {
    Write-Host ''
    Write-Host 'Dry run complete. Re-run without -DryRun to generate classpath.txt.'
    exit 0
}

$jars = @(Get-Jars $classpath)
$existing = @($jars | Where-Object { Test-Path -LiteralPath $_ })
$sources = @(
    $jars |
        ForEach-Object { $_ -replace '\.jar$', '-sources.jar' } |
        Where-Object { Test-Path -LiteralPath $_ }
)
Write-Host ''
Write-Host "Done: $($jars.Count) jars listed, $($existing.Count) exist, $($sources.Count) have sources jars"
Write-Host "Written to $classpath"
Write-Host "Next: in VS Code run 'ld-java-analyze: 重建索引', then 'ld-java-analyze: 查看索引状态'."
