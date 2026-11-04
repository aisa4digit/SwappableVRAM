param([string]$OutputPath = (Join-Path $PSScriptRoot '..\artifacts\lab-readiness.json'))
$ErrorActionPreference = 'Stop'
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$computer = Get-CimInstance Win32_ComputerSystem
$os = Get-CimInstance Win32_OperatingSystem
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [System.Security.Principal.WindowsPrincipal]::new($identity)
$elevated = $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)
$virtual = (($computer.Manufacturer -match 'QEMU|VMware|innotek|Xen|Parallels') -or
    ($computer.Manufacturer -eq 'Microsoft Corporation' -and $computer.Model -eq 'Virtual Machine') -or
    $computer.Model -eq 'VirtualBox')
$driver = @(Get-CimInstance Win32_SystemDriver -Filter "Name='svramflt'")
$sys = Join-Path $workspace 'artifacts\driver\svramflt-lab.sys'
$signature = if (Test-Path -LiteralPath $sys) { (Get-AuthenticodeSignature -LiteralPath $sys).Status.ToString() } else { 'MissingBinary' }
$inf2cat = Get-ChildItem -LiteralPath 'C:\Program Files (x86)\Windows Kits\10\bin' -Recurse -Filter 'inf2cat.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
if ($null -eq $inf2cat) {
    $localTool = Join-Path $workspace 'artifacts\toolchain\inf2cat-26100\Inf2Cat.exe'
    if ((Test-Path -LiteralPath $localTool) -and (Test-Path -LiteralPath (Join-Path (Split-Path $localTool) 'tool-provenance.json'))) { $inf2cat = Get-Item -LiteralPath $localTool }
}
$blockers = @()
if (-not $virtual) { $blockers += 'This is a physical host; experimental driver load tests require an isolated Windows guest or dedicated test machine.' }
if (-not $elevated) { $blockers += 'The current token is not elevated. This script never triggers UAC or elevates itself.' }
if ($signature -ne 'Valid') { $blockers += "The kernel binary has no production trust proof: $signature." }
if ($null -eq $inf2cat) { $blockers += 'Official Inf2Cat is not available in the installed Windows Kits tool path; catalog generation is pending.' }
$result = [ordered]@{
    SchemaVersion=1
    TimeUtc=[DateTimeOffset]::UtcNow.ToString('o')
    Manufacturer=$computer.Manufacturer
    Model=$computer.Model
    OperatingSystem=$os.Version
    HypervisorPresent=[bool]$computer.HypervisorPresent
    GuestModelDetected=[bool]$virtual
    Elevated=$elevated
    AvailableRamMiB=[math]::Round($os.FreePhysicalMemory / 1024)
    DriverServicePresent=($driver.Count -gt 0)
    DriverServiceState=@($driver | Select-Object Name,State,PathName)
    KernelAuthenticode=$signature
    Inf2CatPath=$(if ($null -ne $inf2cat) { $inf2cat.FullName } else { $null })
    KernelPagingPathQualified=$false
    MayLoadExperimentalDriver=$false
    Blockers=$blockers
    Note='Read-only preflight, not authorization or a substitute for a snapshot/driver-verifier qualification campaign. No settings changed.'
}
$directory = Split-Path -Parent ([System.IO.Path]::GetFullPath($OutputPath))
New-Item -ItemType Directory -Force -Path $directory | Out-Null
$result | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $OutputPath -Encoding UTF8
$result | ConvertTo-Json -Depth 5
