param(
    [Parameter(Mandatory=$true)][string]$RunDirectory,
    [ValidateRange(60,600)][int]$MaximumSeconds=300,
    [switch]$ObserveShutdown
)
$ErrorActionPreference = 'Stop'
$workspace = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$run = [IO.Path]::GetFullPath($RunDirectory)
$allowed = Join-Path $workspace 'artifacts\vm-lab\'
if (-not $run.StartsWith($allowed,[StringComparison]::OrdinalIgnoreCase)) { throw 'Only an owned workspace VM fixture can be launched' }
foreach ($name in @('manifest.json','boot.raw','target.raw','firmware-vars.fd')) { if (-not (Test-Path -LiteralPath (Join-Path $run $name))) { throw "Missing private VM fixture: $name" } }
$runtimePath=Join-Path $run 'runtime.json'
if (Test-Path -LiteralPath $runtimePath) {
    $old=Get-Content -LiteralPath $runtimePath -Raw | ConvertFrom-Json
    $live=Get-CimInstance Win32_Process -Filter ("ProcessId="+[int]$old.ProcessId)
    if ($live -and $live.Name -eq 'qemu-system-x86_64.exe' -and $live.CommandLine.IndexOf($run,[StringComparison]::OrdinalIgnoreCase) -ge 0) { throw 'This private VM is already live; disks and runtime metadata were left untouched' }
    $attempt=Join-Path $run ('attempt-'+[DateTime]::UtcNow.ToString('yyyyMMdd-HHmmss-ffff'))
    New-Item -ItemType Directory -Path $attempt | Out-Null
    foreach ($name in @('runtime.json','qemu.stdout','qemu.stderr','guest-serial.log','guest-screen.png')) {
        $source=Join-Path $run $name
        if (Test-Path -LiteralPath $source) { Copy-Item -LiteralPath $source -Destination (Join-Path $attempt $name) }
    }
}
$available = (Get-CimInstance Win32_OperatingSystem).FreePhysicalMemory * 1KB
if ($available -lt 3GB) { throw 'Less than 3 GiB available RAM; isolated 1536 MiB VM not launched' }
$qemu = Join-Path $workspace 'artifacts\toolchain\qemu-20260811\qemu-system-x86_64.exe'
$firmware = Join-Path $workspace 'artifacts\toolchain\qemu-20260811\share\edk2-x86_64-code.fd'
$listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0)
$listener.Start(); $port=$listener.LocalEndpoint.Port; $listener.Stop()
$arguments = @('-machine','q35,accel=whpx','-cpu','qemu64,+sse4.2,+popcnt','-m','1536','-smp','1',
    '-display','none','-monitor','none','-nic','none',
    '-serial',('file:'+(Join-Path $run 'guest-serial.log')),
    '-qmp',("tcp:127.0.0.1:$port,server=on,wait=off"),
    '-drive',("if=pflash,format=raw,readonly=on,file=$firmware"),
    '-drive',('if=pflash,format=raw,file='+(Join-Path $run 'firmware-vars.fd')),
    '-drive',('if=none,id=boot,format=raw,file='+(Join-Path $run 'boot.raw')),
    '-device','ide-hd,drive=boot,bus=ide.0,bootindex=0',
    '-device','mptsas1068,id=scsi0',
    '-drive',('if=none,id=target,format=raw,file='+(Join-Path $run 'target.raw')),
    '-device','scsi-hd,drive=target,bus=scsi0.0,vendor=SVRAMLAB,product=PAGINGDISK,serial=SVRAMLAB01')
if ($ObserveShutdown) { $arguments += '-no-shutdown' }
# No host drives, GPU, network, audio, visible window, host BCD or certificate
# writes. Guest firmware has no Secure Boot enrollment; its vars are disposable.
$process = Start-Process -FilePath $qemu -ArgumentList $arguments -WorkingDirectory $run -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $run 'qemu.stdout') -RedirectStandardError (Join-Path $run 'qemu.stderr')
$process.PriorityClass=[Diagnostics.ProcessPriorityClass]::Idle
@{ ProcessId=$process.Id; QmpPort=$port; StartedUtc=[DateTime]::UtcNow.ToString('o'); MaximumSeconds=$MaximumSeconds; RunDirectory=$run; Arguments=$arguments } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $run 'runtime.json') -Encoding UTF8
Write-Output "Headless isolated guest PID $($process.Id), RAM 1536 MiB, one vCPU, bounded $MaximumSeconds seconds: $run"
try {
    $deadline=[DateTime]::UtcNow.AddSeconds($MaximumSeconds)
    while (-not $process.WaitForExit(1000)) {
        if ([DateTime]::UtcNow -ge $deadline) { throw 'Guest deadline reached; stopping only this private VM process' }
    }
    if ($process.ExitCode -ne 0) { throw "Guest exited with QEMU error $($process.ExitCode)" }
    # COM1 may not exist in the minimal WinPE PnP set. Read the guest's log from
    # its independent disk only after QEMU exits and has released the image.
    $archiveReader=Join-Path $workspace 'artifacts\toolchain\7zip-26.05\7z.exe'
    & $archiveReader e -y ("-o"+(Join-Path $run 'results')) (Join-Path $run 'boot.raw') GUEST.LOG | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot extract stopped guest log' }
    $guestLog=Get-Content -LiteralPath (Join-Path $run 'results\GUEST.LOG') -Raw
    $lastStart=$guestLog.LastIndexOf('SVRAM_GUEST_STARTED',[StringComparison]::Ordinal)
    if ($lastStart -lt 0) { throw 'No guest-start marker in the current log' }
    $currentLog=$guestLog.Substring($lastStart)
    Write-Output $currentLog
    if ($currentLog -match 'SVRAM_GUEST_ERROR') { throw 'The current guest session reported failure; earlier boot markers are not success evidence' }
    if ($currentLog -match 'SVRAM_INVENTORY_COMPLETE') {
        $manifest=Get-Content -LiteralPath (Join-Path $run 'manifest.json') -Raw | ConvertFrom-Json
        if ($manifest.VolatileVerifier -and $currentLog -notmatch 'SVRAM_GUEST_IO_VERIFIER_ACTIVE') {
            throw 'Guest requested I/O Verifier but its runtime activation was not proven'
        }
        if ($manifest.LabPackageIncluded) {
            & $archiveReader e -y ("-o"+(Join-Path $run 'results')) (Join-Path $run 'boot.raw') 'fixture-*.log' LOADED.SYS SETUPAPI.LOG | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'Cannot extract stopped guest driver evidence' }
            $installedHash=(Get-FileHash -LiteralPath (Join-Path $run 'results\LOADED.SYS') -Algorithm SHA256).Hash
            $expectedHash=(Get-FileHash -LiteralPath (Join-Path $run 'media\DRIVER\svramflt.sys') -Algorithm SHA256).Hash
            if ($installedHash -ne $expectedHash) { throw 'Guest driver-store SYS differs from the fixture package' }
            Write-Output "Guest driver-store file matches package SHA256: $installedHash (not an in-memory image hash)"
        }
        Write-Output 'Inventory completed in isolated guest.'
    }
    elseif ($currentLog -match 'SVRAM_STAGE1_SHUTDOWN') { Write-Output 'Guest stage 1 prepared its BCD. Cold-start this same fixture once more for stage 2.' }
    else { throw 'Guest exited without inventory or stage-1 completion evidence' }
} finally {
    if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
    Get-Content -LiteralPath (Join-Path $run 'qemu.stderr') -ErrorAction SilentlyContinue | Write-Output
}
