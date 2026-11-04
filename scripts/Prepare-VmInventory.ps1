param([string]$DriverPackageDirectory,[ValidateSet('miss','late','exit','anon')][string]$ExperimentalScenario,[switch]$VolatileVerifier)
$ErrorActionPreference = 'Stop'
if ($VolatileVerifier -and -not $ExperimentalScenario) { throw 'Volatile guest Verifier requires an explicit experimental scenario' }
$workspace = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$tools = Join-Path $workspace 'artifacts\toolchain'
$assets = Join-Path $tools 'winpe-assets\KitsRoot\Assessment and Deployment Kit\Windows Preinstallation Environment\amd64'
$wimlib = Join-Path $tools 'wimlib-1.14.5\wimlib-imagex.exe'
$qemuImg = Join-Path $tools 'qemu-20260811\qemu-img.exe'
$native = Join-Path $workspace 'native\target\release\svram.exe'
& cargo build --release --manifest-path (Join-Path $workspace 'native\Cargo.toml') --no-default-features --bin svram -j 2
if ($LASTEXITCODE -ne 0) { throw 'Minimal WinPE CLI build failed' }
foreach ($path in @($assets,$wimlib,$qemuImg,$native)) { if (-not (Test-Path -LiteralPath $path)) { throw "Missing lab prerequisite: $path" } }
$run = Join-Path $workspace ('artifacts\vm-lab\inventory-' + [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss-ffff'))
$media = Join-Path $run 'media'
New-Item -ItemType Directory -Path $media | Out-Null
Copy-Item -LiteralPath (Join-Path $assets 'Media\Boot') -Destination $media -Recurse
Copy-Item -LiteralPath (Join-Path $assets 'Media\EFI') -Destination $media -Recurse
Copy-Item -LiteralPath (Join-Path $assets 'Media\bootmgr') -Destination $media
New-Item -ItemType Directory -Path (Join-Path $media 'Sources') | Out-Null
$wim = Join-Path $media 'Sources\boot.wim'
Copy-Item -LiteralPath (Join-Path $assets 'en-us\winpe.wim') -Destination $wim
Copy-Item -LiteralPath $native -Destination (Join-Path $media 'svram.exe')
if ($DriverPackageDirectory) {
    $package=[IO.Path]::GetFullPath($DriverPackageDirectory)
    $manifest=Get-Content -LiteralPath (Join-Path $package 'manifest.json') -Raw | ConvertFrom-Json
    $expectedMode=if ($ExperimentalScenario) { 'experimental' } else { 'bypass' }
    if ($manifest.Mode -ne $expectedMode -or -not $manifest.Signed -or $manifest.SignatureKind -ne 'DisposableTestOnly' -or $manifest.HardwareId -ne 'SCSI\DiskSVRAMLABPAGINGDISK______') { throw 'The smoke test requires the exact signed lab fixture and explicitly selected mode' }
    foreach ($file in $manifest.Files) {
        if ([IO.Path]::GetFileName($file.Name) -ne $file.Name -or (Get-FileHash -LiteralPath (Join-Path $package $file.Name) -Algorithm SHA256).Hash -ne $file.Sha256) { throw 'Lab package changed or contains an unsafe file entry' }
    }
    Copy-Item -LiteralPath $package -Destination (Join-Path $media 'DRIVER') -Recurse
}
if ($ExperimentalScenario) {
    if (-not $DriverPackageDirectory) { throw 'Experimental scenario requires its explicit lab package' }
    [IO.File]::WriteAllText((Join-Path $media 'EXP.ID'),$ExperimentalScenario,[Text.Encoding]::ASCII)
}
if ($VolatileVerifier) {
    [IO.File]::WriteAllText((Join-Path $media 'VERIFIER.ID'),'Guest-only DIF IO verification class5 NOW',[Text.Encoding]::ASCII)
}
[IO.File]::WriteAllText((Join-Path $media 'SVRAM.ID'),'Disposable isolated engineering repro',[Text.Encoding]::ASCII)
$startnet = Join-Path $workspace 'tests\lab\startnet.cmd'
$inventory = Join-Path $workspace 'tests\lab\inventory.cmd'
$commands = Join-Path $run 'wim-update.txt'
@(('add "'+$startnet+'" "/Windows/System32/startnet.cmd"'),('add "'+$inventory+'" "/svram-inventory.cmd"')) | Set-Content -LiteralPath $commands -Encoding UTF8
# Direct archive update, not a DISM mount; no host privilege/registry changes.
Get-Content -LiteralPath $commands | & $wimlib update $wim 1 --check --threads=1 --no-acls
if ($LASTEXITCODE -ne 0) { throw 'Private WinPE startup update failed' }
& $wimlib verify $wim
if ($LASTEXITCODE -ne 0) { throw 'Private WinPE integrity verification failed' }
# Convert QEMU's read-only directory FAT into an independent raw disk. The VM
# never sees vvfat, beta rw directory sharing, a host volume, or a physical disk.
& $qemuImg convert -O raw ('fat:' + $media.Replace('\','/')) (Join-Path $run 'boot.raw')
if ($LASTEXITCODE -ne 0) { throw 'Private boot disk creation failed' }
& $qemuImg create -f raw (Join-Path $run 'target.raw') 1024M
if ($LASTEXITCODE -ne 0) { throw 'Private target disk creation failed' }
Copy-Item -LiteralPath (Join-Path $tools 'qemu-20260811\share\edk2-i386-vars.fd') -Destination (Join-Path $run 'firmware-vars.fd')
@{
    Purpose='Temporary Windows kernel inventory/bypass repro; no GPU/FPS qualification'
    LabPackageIncluded=[bool]$DriverPackageDirectory
    ExperimentalScenario=$ExperimentalScenario
    VolatileVerifier=[bool]$VolatileVerifier
    NativeSha256=(Get-FileHash -LiteralPath $native -Algorithm SHA256).Hash
    WimSha256=(Get-FileHash -LiteralPath $wim -Algorithm SHA256).Hash
    BootDiskSha256=(Get-FileHash -LiteralPath (Join-Path $run 'boot.raw') -Algorithm SHA256).Hash
    HostBootModified=$false; DriverInstalled=$false; PagingQualified=$false
} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $run 'manifest.json') -Encoding UTF8
Write-Output "Prepared independent inventory VM disks: $run"
