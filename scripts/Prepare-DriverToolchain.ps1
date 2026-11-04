param([string]$ToolchainRoot = (Join-Path $PSScriptRoot '..\artifacts\toolchain'))
$ErrorActionPreference = 'Stop'
$version = '10.0.28000.2526'
New-Item -ItemType Directory -Force -Path $ToolchainRoot | Out-Null
$packages = @(
    @{ Id='Microsoft.Windows.WDK.x64'; Directory="wdk-$version"; Archive="wdk-$version.nupkg" },
    @{ Id='Microsoft.Windows.SDK.cpp.x64'; Directory="sdk-x64-$version"; Archive="sdk-x64-$version.nupkg" },
    @{ Id='Microsoft.Windows.SDK.cpp'; Directory="sdk-cpp-$version"; Archive="sdk-cpp-$version.nupkg" }
)
foreach ($package in $packages) {
    $id = $package.Id.ToLowerInvariant()
    $archive = Join-Path $ToolchainRoot $package.Archive
    $directory = Join-Path $ToolchainRoot $package.Directory
    if (-not (Test-Path -LiteralPath $archive)) {
        Invoke-WebRequest -Uri "https://api.nuget.org/v3-flatcontainer/$id/$version/$id.$version.nupkg" -OutFile $archive
    }
    & dotnet nuget verify --all $archive
    if ($LASTEXITCODE -ne 0) { throw "NuGet signature verification failed: $($package.Id)" }
    if (-not (Test-Path -LiteralPath $directory)) {
        [System.IO.Compression.ZipFile]::ExtractToDirectory([System.IO.Path]::GetFullPath($archive), [System.IO.Path]::GetFullPath($directory))
    }
    $specPath = Join-Path $directory "$($package.Id).nuspec"
    if (-not (Test-Path -LiteralPath $specPath)) { throw "Incomplete extracted package: $directory" }
    [xml]$spec = Get-Content -LiteralPath $specPath -Raw
    if ($spec.package.metadata.id -ne $package.Id -or $spec.package.metadata.version -ne $version) {
        throw "Unexpected NuGet identity in $specPath"
    }
    Get-FileHash -LiteralPath $archive -Algorithm SHA256
}
Write-Output 'Project-local headers/libraries/tools prepared. No installer, driver registration, boot or security setting changes.'
