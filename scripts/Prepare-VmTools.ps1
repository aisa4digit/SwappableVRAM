param([switch]$FetchVerifiedTools)
$ErrorActionPreference='Stop'
if (-not $FetchVerifiedTools) { throw 'Use -FetchVerifiedTools to acquire project-local archive/VM tooling. No installer is executed.' }
$workspace=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$root=Join-Path $workspace 'artifacts\toolchain'
$qemu=Join-Path $root 'qemu-20260811'
$live=Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'qemu-system-x86_64.exe' -and $_.ExecutablePath -and $_.ExecutablePath.StartsWith($qemu+'\',[StringComparison]::OrdinalIgnoreCase) }
if ($live) { throw 'A project QEMU VM is live. Stop it before replacing any tool/firmware files.' }
New-Item -ItemType Directory -Force -Path $root | Out-Null
$packages=@(
    @{Name='7z2605-x64.exe';Url='https://www.7-zip.org/a/7z2605-x64.exe';Algorithm='SHA256';Hash='FD99E86F57D8F429EF5ED045BF98315AF962E2309B0F1BA87A147A44C71647D0'},
    @{Name='qemu-w64-setup-20260811.exe';Url='https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe';Algorithm='SHA512';Hash='5bcf9eed634e8575a37b74f445af41a2fe4106da512d0c30c368301d4c105037fdfab40a5287367a28a957624cddebbc8c07e16c88ab6634f554cdf3d16bf543'},
    @{Name='wimlib-1.14.5-windows-x86_64-bin.zip';Url='https://wimlib.net/downloads/wimlib-1.14.5-windows-x86_64-bin.zip';Algorithm='SHA256';Hash='2f446d6fa3866582175f1a22a7be198eeee0aec7aba5b4e04ad25c99eae2d265'}
)
foreach ($package in $packages) {
    $path=Join-Path $root $package.Name
    if (-not (Test-Path -LiteralPath $path)) {
        & curl.exe --fail --location --silent --show-error --limit-rate 1M --max-time 1800 --output $path $package.Url
        if ($LASTEXITCODE -ne 0) { throw "Tool acquisition failed: $($package.Name)" }
    }
    if ((Get-FileHash -LiteralPath $path -Algorithm $package.Algorithm).Hash -ne $package.Hash) { throw "Unexpected/partial tool archive: $path" }
}
# The unsigned 7-Zip installer is treated as archive data from its official
# HTTPS site; its observed release bytes are pinned above. It is never run.
$sevenZip=Join-Path $root '7zip-26.05'
New-Item -ItemType Directory -Force -Path $sevenZip | Out-Null
& tar.exe -xf (Join-Path $root '7z2605-x64.exe') -C $sevenZip 7z.exe 7z.dll License.txt
if ($LASTEXITCODE -ne 0) { throw 'Cannot extract archive reader as data' }
& (Join-Path $sevenZip '7z.exe') x -y ("-o"+$qemu) (Join-Path $root 'qemu-w64-setup-20260811.exe') qemu-system-x86_64.exe qemu-img.exe '*.dll' 'share/*' COPYING COPYING.LIB README.rst VERSION | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Cannot extract QEMU as archive data' }
Expand-Archive -LiteralPath (Join-Path $root 'wimlib-1.14.5-windows-x86_64-bin.zip') -DestinationPath (Join-Path $root 'wimlib-1.14.5') -Force
@{ Packages=$packages; InstallerExecuted=$false; HostBootModified=$false; Purpose='Temporary headless Windows engineering repro; not bundled in the desktop product' } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $root 'vm-tools-provenance.json') -Encoding UTF8
Write-Output 'Pinned project-local VM tools prepared. No installer, VM, or visible window launched.'
