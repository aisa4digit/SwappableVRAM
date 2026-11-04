param([switch]$Driver,[switch]$Wddm)
$ErrorActionPreference = 'Stop'
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
foreach ($manifest in @('native\Cargo.toml','drivers\paging-core\Cargo.toml')) {
    $path = Join-Path $workspace $manifest
    & cargo fmt --manifest-path $path --check
    if ($LASTEXITCODE -ne 0) { throw "Rust formatting failed: $manifest" }
    & cargo test --manifest-path $path
    if ($LASTEXITCODE -ne 0) { throw "Rust tests failed: $manifest" }
    & cargo clippy --manifest-path $path --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw "Clippy failed: $manifest" }
}
if ($Wddm) {
    $path=Join-Path $workspace 'native\Cargo.toml'
    & cargo test --manifest-path $path --features wddm
    if ($LASTEXITCODE -ne 0) { throw 'Optional WDDM tests failed' }
    & cargo clippy --manifest-path $path --features wddm --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'Optional WDDM clippy failed' }
}
if ($Driver) {
    & (Join-Path $PSScriptRoot 'Build-Driver.ps1') -ExperimentalCache -Analyze
    & (Join-Path $PSScriptRoot 'Prepare-LabPackage.ps1') -ExperimentalCache
    & (Join-Path $PSScriptRoot 'Prepare-LabPackage.ps1')
}
Write-Output 'Source checks passed. No driver loaded, pagefile activated, or game performance claim verified.'
