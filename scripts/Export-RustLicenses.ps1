$ErrorActionPreference='Stop'
$workspace=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$metadata=(& cargo metadata --manifest-path (Join-Path $workspace 'native\Cargo.toml') --features wddm --format-version 1 --filter-platform x86_64-pc-windows-msvc) | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw 'Cargo dependency metadata failed' }
$output=Join-Path $workspace 'third_party\rust-licenses'
$rows=@('# Optional Rust dependency licenses','','Generated from the locked Windows/WDDM dependency graph. Original license texts follow in each package directory.','')
foreach ($package in ($metadata.packages | Where-Object { $_.source })) {
    $name=$package.name+'-'+$package.version
    if ($name -notmatch '^[A-Za-z0-9_.+-]+$') { throw 'Unsafe dependency directory name' }
    $source=[IO.Path]::GetDirectoryName($package.manifest_path)
    $licenses=@(Get-ChildItem -LiteralPath $source -File | Where-Object { $base=$_.Name.ToLowerInvariant(); $base.StartsWith('license',[StringComparison]::Ordinal) -or $base.StartsWith('copying',[StringComparison]::Ordinal) })
    if ($licenses.Count -eq 0) { throw "Missing license texts for $name" }
    $destination=Join-Path $output $name
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    foreach ($file in $licenses) { Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $destination $file.Name) -Force }
    $rows += '- '+$name+': '+$package.license
}
$rows | Set-Content -LiteralPath (Join-Path $output 'README.md') -Encoding UTF8
Write-Output 'Locked optional WDDM dependency license texts exported.'
