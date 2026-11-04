param([switch]$FrameworkDependent)
$ErrorActionPreference = 'Stop'
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$output = Join-Path $workspace 'artifacts\desktop'
$selfContained = if ($FrameworkDependent) { 'false' } else { 'true' }
& dotnet publish (Join-Path $workspace 'src\SwappableVRAM.App\SwappableVRAM.App.csproj') -c Release -r win-x64 --self-contained $selfContained -o $output
if ($LASTEXITCODE -ne 0) { throw 'Desktop publish failed' }
$brokerTarget=Join-Path $workspace 'artifacts\broker-native'
& cargo build --release --manifest-path (Join-Path $workspace 'native\Cargo.toml') --features wddm --bin svram --target-dir $brokerTarget
if ($LASTEXITCODE -ne 0) { throw 'Interactive Rust broker build failed' }
Copy-Item -LiteralPath (Join-Path $brokerTarget 'release\svram.exe') -Destination (Join-Path $output 'SwappableVRAM.Broker.exe') -Force
Copy-Item -LiteralPath (Join-Path $workspace 'README.md') -Destination $output -Force
Copy-Item -LiteralPath (Join-Path $workspace 'LICENSE') -Destination $output -Force
Copy-Item -LiteralPath (Join-Path $workspace 'THIRD_PARTY_NOTICES.md') -Destination $output -Force
$licenses=Join-Path $workspace 'third_party\rust-licenses'
foreach ($source in @($licenses,(Join-Path $workspace 'docs'))) {
    if (Test-Path -LiteralPath $source) {
        $destination=Join-Path $output ([IO.Path]::GetFileName($source))
        New-Item -ItemType Directory -Force -Path $destination | Out-Null
        Get-ChildItem -LiteralPath $source | Copy-Item -Destination $destination -Recurse -Force
    }
}
Write-Output "$output - development UI; does not activate system paging offload. Not launched."
