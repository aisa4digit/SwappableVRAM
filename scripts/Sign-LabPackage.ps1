param(
    [Parameter(Mandatory=$true)][string]$PackageDirectory,
    [switch]$TestOnly
)
$ErrorActionPreference = 'Stop'
if (-not $TestOnly) { throw 'This command requires -TestOnly and cannot produce a production-trusted signature.' }
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$package = (Resolve-Path -LiteralPath $PackageDirectory).ProviderPath
$manifestPath = Join-Path $package 'manifest.json'
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ($manifest.Mode -notin @('bypass','experimental') -or $manifest.HardwareId -notmatch 'SVRAMLAB') { throw 'Only the custom isolated-lab package can be test-signed' }
$signtool = 'C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64\signtool.exe'
if (-not (Test-Path -LiteralPath $signtool)) { throw 'The existing Windows SDK SignTool is required' }
$signerToolSignature = Get-AuthenticodeSignature -LiteralPath $signtool
if ($signerToolSignature.Status -ne 'Valid' -or $signerToolSignature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') { throw 'SignTool Microsoft signature validation failed' }
$keyDirectory = Join-Path $workspace 'artifacts\lab-signing'
New-Item -ItemType Directory -Force -Path $keyDirectory | Out-Null
$publicPath = Join-Path $keyDirectory 'svram-disposable-lab.cer'
$pfxPath = Join-Path $keyDirectory 'svram-disposable-lab.pfx'
$passwordPath = Join-Path $keyDirectory 'test-only-password.txt'
# These are disposable development keys, not a user's signing identity. They
# remain in ignored artifacts and are NEVER imported into a host cert store.
if (-not (Test-Path -LiteralPath $pfxPath)) {
    $rsa = [System.Security.Cryptography.RSA]::Create(2048)
    $certificate = $null
    $random = [System.Security.Cryptography.RandomNumberGenerator]::Create()
    try {
        $request = [System.Security.Cryptography.X509Certificates.CertificateRequest]::new(
            'CN=SwappableVRAM Disposable Lab - NOT PRODUCTION', $rsa,
            [System.Security.Cryptography.HashAlgorithmName]::SHA256, [System.Security.Cryptography.RSASignaturePadding]::Pkcs1)
        $oids = [System.Security.Cryptography.OidCollection]::new()
        $oids.Add([System.Security.Cryptography.Oid]::new('1.3.6.1.5.5.7.3.3')) | Out-Null
        $request.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509EnhancedKeyUsageExtension]::new($oids,$false))
        $request.CertificateExtensions.Add([System.Security.Cryptography.X509Certificates.X509KeyUsageExtension]::new(
            [System.Security.Cryptography.X509Certificates.X509KeyUsageFlags]::DigitalSignature,$true))
        $certificate = $request.CreateSelfSigned([DateTimeOffset]::UtcNow.AddMinutes(-5),[DateTimeOffset]::UtcNow.AddDays(7))
        $passwordBytes = [byte[]]::new(32); $random.GetBytes($passwordBytes)
        $password = [Convert]::ToBase64String($passwordBytes)
        [System.IO.File]::WriteAllBytes($pfxPath,$certificate.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Pfx,$password))
        [System.IO.File]::WriteAllBytes($publicPath,$certificate.Export([System.Security.Cryptography.X509Certificates.X509ContentType]::Cert))
        [System.IO.File]::WriteAllText($passwordPath,$password,[System.Text.Encoding]::ASCII)
    } finally {
        if ($null -ne $certificate) { $certificate.Dispose() }
        $random.Dispose(); $rsa.Dispose()
    }
}
$password = [System.IO.File]::ReadAllText($passwordPath)
$public = [System.Security.Cryptography.X509Certificates.X509Certificate2]::new($publicPath)
if ($public.NotAfter.ToUniversalTime() -le [DateTime]::UtcNow) { throw 'Disposable lab certificate expired; prepare new disposable key artifacts before continuing' }
$logDirectory = Join-Path $workspace 'artifacts\verification'
New-Item -ItemType Directory -Force -Path $logDirectory | Out-Null
function Sign-TestFile([string]$Path,[string]$Tag) {
    $arguments = @('sign','/fd','SHA256','/f',('"'+$pfxPath+'"'),'/p',$password,('"'+$Path+'"'))
    $process = Start-Process -FilePath $signtool -ArgumentList $arguments -WindowStyle Hidden -RedirectStandardOutput (Join-Path $logDirectory "$Tag-sign.stdout.txt") -RedirectStandardError (Join-Path $logDirectory "$Tag-sign.stderr.txt") -PassThru -Wait
    if ($process.ExitCode -ne 0) { throw "Disposable test signature failed: $Tag (see verification logs)" }
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.SignerCertificate.Thumbprint -ne $public.Thumbprint -or $signature.Status -in @('HashMismatch','NotSigned','NotSupportedFileFormat')) {
        throw "Test signature verification failed: $Tag"
    }
}
$sys = Join-Path $package 'svramflt.sys'
Sign-TestFile $sys 'svram-sys'
# A catalog must be regenerated after the image is signed; never keep an old
# catalog for a modified SYS, even when PE SIP hashing might ignore its cert.
& (Join-Path $PSScriptRoot 'New-DriverCatalog.ps1') -PackageDirectory $package
$catalog = Join-Path $package 'svramflt.cat'
Sign-TestFile $catalog 'svram-cat'
try { Add-Type -AssemblyName System.Security.Cryptography.Pkcs } catch { Add-Type -AssemblyName System.Security }
$cms = [System.Security.Cryptography.Pkcs.SignedCms]::new()
$cms.Decode([System.IO.File]::ReadAllBytes($catalog))
$cms.CheckSignature($true)
if ($cms.SignerInfos.Count -ne 1 -or $cms.SignerInfos[0].Certificate.Thumbprint -ne $public.Thumbprint) { throw 'Catalog signer is not the disposable lab certificate' }
Copy-Item -LiteralPath $publicPath -Destination (Join-Path $package 'svram-lab.cer') -Force
$manifest | Add-Member -NotePropertyName SignatureKind -NotePropertyValue 'DisposableTestOnly' -Force
$manifest | Add-Member -NotePropertyName ProductionTrusted -NotePropertyValue $false -Force
$manifest | Add-Member -NotePropertyName HostTrustStoreModified -NotePropertyValue $false -Force
$manifest | Add-Member -NotePropertyName TestSignerThumbprint -NotePropertyValue $public.Thumbprint -Force
$manifest.Signed = $true
$manifest.CatalogGenerated = $true
$manifest.Files = @(Get-ChildItem -LiteralPath $package -File | Where-Object Extension -in '.sys','.inf','.cat','.cer' | ForEach-Object {
    @{Name=$_.Name; Sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash}
})
$manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $manifestPath -Encoding UTF8
$public.Dispose()
Write-Output 'Disposable VM-only test signatures created; not production-trusted, not installed, no host certificate or boot setting changed.'
