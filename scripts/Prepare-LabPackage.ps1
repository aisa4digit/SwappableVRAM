param(
    [string]$HardwareId = 'SVRAMLAB\DisposableDisk',
    [switch]$ExperimentalCache,
    [switch]$Catalog,
    [string]$ToolchainRoot = (Join-Path $PSScriptRoot '..\artifacts\toolchain')
)
$ErrorActionPreference = 'Stop'
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
# No generic/physical disk matching. The development target must use a custom
# virtual-disk ID. INF HWID matching is NOT an exact device-instance binding.
if ($HardwareId -notmatch '^(SVRAMLAB\\[A-Za-z0-9_-]+|SCSI\\DiskSVRAMLAB[A-Za-z0-9_& -]+)$') {
    throw 'Only a custom SVRAMLAB virtual-device HWID is accepted. Generic Disk/Msft/QEMU/GenDisk/physical hardware IDs are refused.'
}
$buildArguments = @{ ToolchainRoot=$ToolchainRoot; ExperimentalCache=$ExperimentalCache }
& (Join-Path $PSScriptRoot 'Build-Driver.ps1') @buildArguments
$kind = if ($ExperimentalCache) { 'experimental' } else { 'bypass' }
$packageDirectory = Join-Path $workspace "artifacts\driver-package-$kind"
New-Item -ItemType Directory -Force -Path $packageDirectory | Out-Null
$catalogPath = Join-Path $packageDirectory 'svramflt.cat'
if (Test-Path -LiteralPath $catalogPath) { Remove-Item -LiteralPath $catalogPath }
$name = if ($ExperimentalCache) { 'svramflt-lab.sys' } else { 'svramflt.sys' }
Copy-Item -LiteralPath (Join-Path $workspace "artifacts\driver\$name") -Destination (Join-Path $packageDirectory 'svramflt.sys') -Force
Copy-Item -LiteralPath (Join-Path $workspace 'third_party\driver-samples\LICENSE-MS-PL.txt') -Destination (Join-Path $packageDirectory 'LICENSE-MS-PL.txt') -Force
$template = Get-Content -LiteralPath (Join-Path $workspace 'drivers\kmdf\svramflt.inf.in') -Raw
$infPath = Join-Path $packageDirectory 'svramflt.inf'
[System.IO.File]::WriteAllText($infPath, $template.Replace('@@HARDWARE_ID@@',$HardwareId), [System.Text.Encoding]::ASCII)
$verifier = Join-Path $ToolchainRoot 'wdk-10.0.28000.2526\c\tools\10.0.28000.0\x64\infverif.exe'
& $verifier /w /v $infPath
if ($LASTEXITCODE -ne 0) { throw 'INF validation failed; package is not ready for signing or load testing.' }
if ($Catalog) { & (Join-Path $PSScriptRoot 'New-DriverCatalog.ps1') -PackageDirectory $packageDirectory }
$manifest = [ordered]@{
    Version=1
    Mode=$kind
    HardwareId=$HardwareId
    InfMatching='All devices sharing this HWID; not an exact instance ID'
    Signed=$false
    CatalogGenerated=(Test-Path -LiteralPath $catalogPath)
    Installed=$false
    PagingPathQualified=$false
    DefaultSyntheticTarget=($HardwareId -eq 'SVRAMLAB\DisposableDisk')
    Files=@(Get-ChildItem -LiteralPath $packageDirectory -File | Where-Object Extension -in '.sys','.inf','.cat' | ForEach-Object {
        @{ Name=$_.Name; Sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash }
    })
}
$manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $packageDirectory 'manifest.json') -Encoding UTF8
Write-Output "$packageDirectory - source prototype only; unsigned, not installed, not qualified."
