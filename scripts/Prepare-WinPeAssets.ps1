param([switch]$FetchVerifiedAssets)
$ErrorActionPreference = 'Stop'
if (-not $FetchVerifiedAssets) { throw 'Use -FetchVerifiedAssets to acquire isolated-lab boot assets. No installer or VM is launched.' }
$workspace = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$root = Join-Path $workspace 'artifacts\toolchain'
$output = Join-Path $root 'winpe-assets'
$base = 'https://download.microsoft.com/download/a4a79e7a-f085-41c4-aebf-2538fd000790/adkwinpeaddons/'
$bootstrap = Join-Path $root 'adkwinpesetup-26100.exe'
# Bootstrapper metadata was extracted as data from its Burn CAB, never executed.
# Bind every subsequent payload to this exact signed Microsoft bootstrapper.
New-Item -ItemType Directory -Force -Path $root | Out-Null
if (-not (Test-Path -LiteralPath $bootstrap)) {
    & curl.exe --fail --location --silent --show-error --limit-rate 1M --max-time 90 --output $bootstrap ($base+'adkwinpesetup.exe')
    if ($LASTEXITCODE -ne 0) { throw 'WinPE metadata acquisition failed' }
}
if ((Get-FileHash -LiteralPath $bootstrap -Algorithm SHA256).Hash -ne 'D4DE67ACC83DE253CCC941DD0590808D83726BDD8EA768A374A7BEAFF7949D6A') { throw 'Unexpected WinPE bootstrapper' }
$signature = Get-AuthenticodeSignature -LiteralPath $bootstrap
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notlike '*O=Microsoft Corporation*') { throw 'WinPE bootstrapper publisher verification failed' }
# Also bind the extracted manifest itself. Derive its bytes again from the
# signed container before trusting paths/hashes rather than trusting a loose XML.
$bootstrapBytes = [IO.File]::ReadAllBytes($bootstrap)
$cab = Join-Path $root 'adkwinpe-burn.cab'
$cabBytes = [byte[]]::new(1207903)
[Array]::Copy($bootstrapBytes,733184,$cabBytes,0,$cabBytes.Length)
[IO.File]::WriteAllBytes($cab,$cabBytes)
$freshMetadata = Join-Path $output 'bootstrap-metadata'
New-Item -ItemType Directory -Force -Path $freshMetadata | Out-Null
& "$env:SystemRoot\System32\expand.exe" '-F:0' $cab $freshMetadata | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Cannot extract signed bootstrapper manifest' }
[xml]$manifest = Get-Content -LiteralPath (Join-Path $freshMetadata '0')
$payloads = @{}
foreach ($payload in $manifest.BurnManifest.Payload) { $payloads[$payload.FilePath.Replace('\','/')] = $payload }
function Get-Payload([string]$Relative, [string]$Destination) {
    $relativeUrl = $Relative.Replace('\','/')
    if (-not $payloads.ContainsKey($relativeUrl)) { throw "Payload absent from signed manifest: $Relative" }
    $payload = $payloads[$relativeUrl]
    if (-not (Test-Path -LiteralPath $Destination) -or (Get-Item -LiteralPath $Destination).Length -lt [long]$payload.FileSize) {
        $url = $base + (($relativeUrl.Split('/') | ForEach-Object { [Uri]::EscapeDataString($_) }) -join '/')
        & curl.exe --fail --location --silent --show-error --continue-at - --limit-rate 768K --max-time 1800 --output $Destination $url
        if ($LASTEXITCODE -ne 0) { throw "Payload download failed: $relativeUrl" }
    }
    if ((Get-Item -LiteralPath $Destination).Length -ne [long]$payload.FileSize -or
        (Get-FileHash -LiteralPath $Destination -Algorithm SHA1).Hash -ne $payload.Hash) {
        throw "Payload differs from Microsoft signed manifest: $relativeUrl (partial downloads are not reused)"
    }
}
$installer = New-Object -ComObject WindowsInstaller.Installer
function Read-Table($Database,[string]$Query,[int]$Columns) {
    $view = $Database.OpenView($Query)
    try {
        $null = $view.Execute()
        while ($record = $view.Fetch()) {
            $values = for ($i=1; $i -le $Columns; ++$i) { $record.StringData($i) }
            ,$values
        }
    } finally { $null = $view.Close() }
}
function Extract-MsiPayloads([string]$Relative,[string]$LocalName,[switch]$Amd64WimOnly) {
    $msi = Join-Path $root $LocalName
    Get-Payload $Relative $msi
    $msiSignature = Get-AuthenticodeSignature -LiteralPath $msi
    if ($msiSignature.Status -ne 'Valid' -or $msiSignature.SignerCertificate.Subject -notlike '*O=Microsoft Corporation*') { throw 'MSI publisher verification failed' }
    $database = $installer.OpenDatabase($msi,0)
    $directories = @{}
    foreach ($row in (Read-Table $database 'SELECT `Directory`,`Directory_Parent`,`DefaultDir` FROM `Directory`' 3)) { $directories[$row[0]] = @($row[1],$row[2].Split('|')[-1]) }
    $components = @{}
    foreach ($row in (Read-Table $database 'SELECT `Component`,`Directory_` FROM `Component`' 2)) { $components[$row[0]] = $row[1] }
    $media = @(Read-Table $database 'SELECT `LastSequence`,`Cabinet` FROM `Media` ORDER BY `DiskId`' 2)
    $files = @(Read-Table $database 'SELECT `File`,`FileName`,`Sequence`,`Component_`,`FileSize` FROM `File`' 5)
    foreach ($file in $files) {
        $directory = $components[$file[3]]
        $segments = @()
        for ($depth=0; $directory -ne 'KitsRoot'; ++$depth) {
            if ($depth -gt 32 -or -not $directories.ContainsKey($directory)) { throw 'MSI directory graph does not terminate at KitsRoot' }
            $entry = $directories[$directory]
            if ($entry[1] -ne '.') { $segments = @($entry[1]) + $segments }
            $directory = $entry[0]
        }
        $name = $file[1].Split('|')[-1]
        if ($Amd64WimOnly -and ($name -ne 'winpe.wim' -or $segments -notcontains 'amd64')) { continue }
        foreach ($segment in ($segments + @($name))) {
            if ($segment -in '.', '..' -or $segment.IndexOfAny([char[]]'\/:') -ge 0) { throw 'Unsafe MSI destination component' }
        }
        $cabName = ($media | Where-Object { [int]$_[0] -ge [int]$file[2] -and $_[1] } | Select-Object -First 1)[1]
        if (-not $cabName -or [IO.Path]::GetFileName($cabName) -ne $cabName) { throw 'Unsupported MSI cabinet path' }
        $cabPath = Join-Path $root $cabName
        Get-Payload ('Installers/' + $cabName) $cabPath
        $unpacked = Join-Path $output ('cab-' + [IO.Path]::GetFileNameWithoutExtension($cabName))
        New-Item -ItemType Directory -Force -Path $unpacked | Out-Null
        $source = Join-Path $unpacked $file[0]
        if (-not (Test-Path -LiteralPath $source)) {
            & "$env:SystemRoot\System32\expand.exe" ("-F:" + $file[0]) $cabPath $unpacked | Out-Null
            if ($LASTEXITCODE -ne 0) { throw 'Cabinet extraction failed' }
        }
        if ((Get-Item -LiteralPath $source).Length -ne [long]$file[4]) { throw 'Unexpected extracted file length' }
        $destination = Join-Path (Join-Path $output 'KitsRoot') (($segments + @($name)) -join '\')
        New-Item -ItemType Directory -Force -Path ([IO.Path]::GetDirectoryName($destination)) | Out-Null
        Copy-Item -LiteralPath $source -Destination $destination -Force
    }
}
Extract-MsiPayloads 'Installers/Windows PE wims (DesktopEditions)-x86_en-us.msi' 'winpe-wims.msi' -Amd64WimOnly
Extract-MsiPayloads 'Installers/Windows PE Boot Files (DesktopEditions)-x86_en-us.msi' 'winpe-bootfiles.msi'
Extract-MsiPayloads 'Installers/Windows PE Boot Files (OnecoreUAP)-x86_en-us.msi' 'winpe-onecoreboot.msi'
@{
    Purpose='Isolated engineering reproduction only; not hardware/FPS qualification'
    BootstrapSha256=(Get-FileHash -LiteralPath $bootstrap -Algorithm SHA256).Hash
    InstallerExecuted=$false
    HostBootConfigurationModified=$false
    Files=@(Get-ChildItem -LiteralPath (Join-Path $output 'KitsRoot') -Recurse -File | ForEach-Object { @{ Path=$_.FullName.Substring($output.Length+1); Sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash } })
} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $output 'provenance.json') -Encoding UTF8
Write-Output "$output - verified and extracted as data; nothing installed or launched."
