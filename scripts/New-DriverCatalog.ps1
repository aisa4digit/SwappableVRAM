param(
    [Parameter(Mandatory=$true)][string]$PackageDirectory,
    [string]$ToolDirectory
)
$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($ToolDirectory)) { $ToolDirectory = Join-Path $PSScriptRoot '..\artifacts\toolchain\inf2cat-26100' }
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$package = (Resolve-Path -LiteralPath $PackageDirectory).ProviderPath.TrimEnd('\')
$tool = Join-Path $ToolDirectory 'Inf2Cat.exe'
$proofPath = Join-Path $ToolDirectory 'tool-provenance.json'
if (-not (Test-Path -LiteralPath $proofPath)) { throw 'Run Prepare-Inf2Cat.ps1 first; verified tool provenance is required' }
$proof = Get-Content -LiteralPath $proofPath -Raw | ConvertFrom-Json
if ($proof.PublisherCertificateSha256 -ne '566A31882BE208BE4422F7CFD66ED09F5D4524A5994F50CCC8B05EC0528C1353') { throw 'Unexpected tool publisher proof' }
foreach ($file in $proof.Files) {
    $path = Join-Path $ToolDirectory $file.Name
    if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $file.Sha256) { throw "Tool component changed: $($file.Name)" }
}
foreach ($name in @('svramflt.inf','svramflt.sys')) {
    if (-not (Test-Path -LiteralPath (Join-Path $package $name))) { throw "Missing catalog input: $name" }
}
$logDirectory = Join-Path $workspace 'artifacts\verification'
New-Item -ItemType Directory -Force -Path $logDirectory | Out-Null
$tag = [System.IO.Path]::GetFileName($package)
$stdout = Join-Path $logDirectory "$tag-inf2cat.stdout.txt"
$stderr = Join-Path $logDirectory "$tag-inf2cat.stderr.txt"
$arguments = @('/driver:"' + $package + '"', '/os:10_VB_X64,10_CO_X64,10_NI_X64', '/uselocaltime', '/verbose')
$process = Start-Process -FilePath $tool -ArgumentList $arguments -WindowStyle Hidden -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru -Wait
if ($process.ExitCode -ne 0) {
    Get-Content -LiteralPath $stdout -Tail 30
    Get-Content -LiteralPath $stderr -Tail 15
    throw "Official Inf2Cat failed (exit $($process.ExitCode)); see $logDirectory"
}
$catalog = Join-Path $package 'svramflt.cat'
if (-not (Test-Path -LiteralPath $catalog)) { throw 'Inf2Cat returned without producing a catalog' }
Get-FileHash -LiteralPath $catalog -Algorithm SHA256
Write-Output 'Official Inf2Cat signability/catalog generation passed. Catalog remains unsigned; no driver loaded.'
