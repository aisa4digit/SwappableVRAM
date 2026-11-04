param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '..\artifacts\toolchain\inf2cat-26100'),
    [string]$PackagePath = (Join-Path $PSScriptRoot '..\artifacts\toolchain\wdk-10.0.26100.6584.nupkg'),
    [switch]$FetchVerifiedPackage
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem
$url = 'https://api.nuget.org/v3-flatcontainer/microsoft.windows.wdk.x64/10.0.26100.6584/microsoft.windows.wdk.x64.10.0.26100.6584.nupkg'
$publisherFingerprint = '566A31882BE208BE4422F7CFD66ED09F5D4524A5994F50CCC8B05EC0528C1353'
New-Item -ItemType Directory -Force -Path (Split-Path -Parent ([System.IO.Path]::GetFullPath($PackagePath))) | Out-Null
if (-not (Test-Path -LiteralPath $PackagePath)) {
    if (-not $FetchVerifiedPackage) { throw 'Official WDK tool package missing. Rerun with -FetchVerifiedPackage (110 MB, rate capped, no installer).' }
    & curl.exe --silent --show-error --fail --location --limit-rate 1M --output $PackagePath $url
    if ($LASTEXITCODE -ne 0) { throw 'Bounded-rate tool package download failed' }
}
if ((Get-Item -LiteralPath $PackagePath).Length -ne 110872506) { throw 'Incomplete or unexpected tool package; refusing to overwrite an existing file' }
& dotnet nuget verify --all --certificate-fingerprint $publisherFingerprint $PackagePath
if ($LASTEXITCODE -ne 0) { throw 'Trusted Microsoft NuGet publisher signature validation failed' }

# Some official DLLs have an INTERNAL Microsoft build certificate that is not
# trusted as a host root. The complete signed NuGet publisher envelope proves
# their exact bytes. No certificate is imported and no host trust is changed.
$members = @(
    'c/bin/10.0.26100.0/Microsoft.DriverKit.Telemetry.dll',
    'c/bin/10.0.26100.0/x86/Microsoft.UniversalStore.HardwareWorkflow.Cabinets.dll',
    'c/bin/10.0.26100.0/x86/Microsoft.UniversalStore.HardwareWorkflow.Catalogs.dll',
    'c/bin/10.0.26100.0/x86/Microsoft.UniversalStore.HardwareWorkflow.InfReader.dll',
    'c/bin/10.0.26100.0/x86/Microsoft.UniversalStore.HardwareWorkflow.SubmissionBuilder.dll',
    'c/bin/10.0.26100.0/x86/Inf2Cat.exe',
    'c/bin/10.0.26100.0/x86/WindowsProtectedFiles.xml'
)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$archive = [System.IO.Compression.ZipFile]::OpenRead([System.IO.Path]::GetFullPath($PackagePath))
$files = @()
try {
    foreach ($member in $members) {
        $entry = $archive.GetEntry($member)
        if ($null -eq $entry -or $entry.Length -le 0 -or $entry.Length -gt 524288) { throw "Unexpected tool ZIP member: $member" }
        $name = ($member -split '/')[-1]
        $output = Join-Path $OutputDirectory $name
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry,[System.IO.Path]::GetFullPath($output),$true)
        $authenticode = if ($name.EndsWith('.xml')) { 'Data file' } else { (Get-AuthenticodeSignature -LiteralPath $output).Status.ToString() }
        $files += @{
            Name=$name
            Sha256=(Get-FileHash -LiteralPath $output -Algorithm SHA256).Hash
            IndividualAuthenticode=$authenticode
            VerificationBasis='Trusted Microsoft NuGet publisher envelope; exact ZIP member'
        }
    }
} finally { $archive.Dispose() }
$provenance = @{
    Package='Microsoft.Windows.WDK.x64'
    Version='10.0.26100.6584'
    PackageSha256=(Get-FileHash -LiteralPath $PackagePath -Algorithm SHA256).Hash
    PublisherCertificateSha256=$publisherFingerprint
    TrustStoreModified=$false
    Files=$files
}
$provenance | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'tool-provenance.json') -Encoding UTF8
Write-Output "$OutputDirectory - verified official Inf2Cat files prepared; no installer or trust-store modification."
