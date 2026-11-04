param(
    [string]$ToolchainRoot = (Join-Path $PSScriptRoot '..\artifacts\toolchain'),
    [switch]$ExperimentalCache,
    [switch]$Analyze
)
$ErrorActionPreference = 'Stop'
$workspace = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$kitVersion = '10.0.28000.2526'
$kitBuild = '10.0.28000.0'
$wdkRoot = Join-Path $ToolchainRoot "wdk-$kitVersion\c"
$sdkRoot = Join-Path $ToolchainRoot "sdk-cpp-$kitVersion\c"
$sdkX64Root = Join-Path $ToolchainRoot "sdk-x64-$kitVersion\c"
$msvcRoot = 'C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Tools\MSVC\14.51.36231'
$compiler = Join-Path $msvcRoot 'bin\Hostx64\x64\cl.exe'
$linker = Join-Path $msvcRoot 'bin\Hostx64\x64\link.exe'
$buildDirectory = Join-Path $workspace 'artifacts\driver'
foreach ($required in @($compiler, $linker, $wdkRoot, $sdkRoot, $sdkX64Root)) {
    if (-not (Test-Path -LiteralPath $required)) { throw "Missing driver build prerequisite: $required" }
}
New-Item -ItemType Directory -Force -Path $buildDirectory | Out-Null
& cargo build --release --manifest-path (Join-Path $workspace 'drivers\paging-core\Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'Rust paging-core build failed' }
$includes = @(
    (Join-Path $wdkRoot "Include\$kitBuild\km"),
    (Join-Path $wdkRoot "Include\$kitBuild\km\crt"),
    (Join-Path $wdkRoot 'Include\wdf\kmdf\1.31'),
    (Join-Path $sdkRoot "Include\$kitBuild\shared"),
    (Join-Path $sdkRoot "Include\$kitBuild\ucrt"),
    (Join-Path $msvcRoot 'include')
)
$compileArguments = @('/nologo','/c','/TC','/kernel','/W4','/WX','/GS','/guard:cf','/Gy','/Zl','/O2','/external:anglebrackets','/external:W0',
    '/D_AMD64_','/DAMD64','/D_WIN64','/D_WIN32_WINNT=0x0A00','/DNTDDI_VERSION=0x0A000008',
    '/DKMDF_VERSION_MAJOR=1','/DKMDF_VERSION_MINOR=31')
$compileArguments += if ($ExperimentalCache) { '/DSV_ENABLE_EXPERIMENTAL_CACHE=1' } else { '/DSV_ENABLE_EXPERIMENTAL_CACHE=0' }
if ($Analyze) { $compileArguments += @('/analyze','/analyze:external-') }
$compileArguments += $includes | ForEach-Object { "/I$_" }
$objectPaths = @()
foreach ($source in @('filter','cache','control')) {
    $objectPath = Join-Path $buildDirectory "$source.obj"
    $objectPaths += $objectPath
    $arguments = $compileArguments + @("/Fo$objectPath", (Join-Path $workspace "drivers\kmdf\$source.c"))
    & $compiler @arguments
    if ($LASTEXITCODE -ne 0) { throw "KMDF $source compilation failed" }
}
$driverName = if ($ExperimentalCache) { 'svramflt-lab.sys' } else { 'svramflt.sys' }
$driverPath = Join-Path $buildDirectory $driverName
$kmLibraries = Join-Path $wdkRoot "Lib\$kitBuild\km\x64"
$wdfLibraries = Join-Path $wdkRoot 'Lib\wdf\kmdf\x64\1.31'
$linkArguments = @('/nologo','/DRIVER','/SUBSYSTEM:NATIVE,10.00','/ENTRY:FxDriverEntry','/NODEFAULTLIB',
    '/MACHINE:X64','/OPT:REF','/OPT:ICF','/INCREMENTAL:NO','/RELEASE','/NXCOMPAT','/DYNAMICBASE',
    '/HIGHENTROPYVA','/INTEGRITYCHECK','/GUARD:CF', "/OUT:$driverPath", "/LIBPATH:$kmLibraries", "/LIBPATH:$wdfLibraries",
    (Join-Path $workspace 'drivers\paging-core\target\release\svram_paging_core.lib'),
    'ntoskrnl.lib','hal.lib','wmilib.lib','BufferOverflowK.lib','wdfdriverentry.lib','wdfldr.lib') + $objectPaths
& $linker @linkArguments
if ($LASTEXITCODE -ne 0) { throw 'KMDF/Rust driver link failed' }
Get-FileHash -LiteralPath $driverPath -Algorithm SHA256
Write-Output "Compiled only: unsigned; experimental cache=$ExperimentalCache; not installed or pagefile-qualified."
