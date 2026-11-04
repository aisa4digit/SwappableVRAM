param(
    [Parameter(Mandatory=$true)][string]$RunDirectory,
    [ValidateSet('query-status','screendump','quit')][string]$Command='query-status'
)
$ErrorActionPreference='Stop'
$workspace=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$run=[IO.Path]::GetFullPath($RunDirectory)
if (-not $run.StartsWith((Join-Path $workspace 'artifacts\vm-lab\'),[StringComparison]::OrdinalIgnoreCase)) { throw 'Only the owned isolated lab VM is accepted' }
$runtime=Get-Content -LiteralPath (Join-Path $run 'runtime.json') -Raw | ConvertFrom-Json
$process=Get-CimInstance Win32_Process -Filter ("ProcessId="+[int]$runtime.ProcessId)
if (-not $process -or $process.Name -ne 'qemu-system-x86_64.exe' -or $process.CommandLine.IndexOf($run,[StringComparison]::OrdinalIgnoreCase) -lt 0) { throw 'Recorded lab process is not live or does not own this fixture' }
$client=[Net.Sockets.TcpClient]::new()
try {
    $client.Connect('127.0.0.1',[int]$runtime.QmpPort)
    $stream=$client.GetStream(); $stream.ReadTimeout=5000; $stream.WriteTimeout=5000
    $reader=[IO.StreamReader]::new($stream,[Text.Encoding]::UTF8,$false,4096,$true)
    $writer=[IO.StreamWriter]::new($stream,[Text.UTF8Encoding]::new($false),4096,$true); $writer.AutoFlush=$true
    $null=$reader.ReadLine() # QMP greeting, not a command reply.
    foreach ($item in @(@{execute='qmp_capabilities';id='capabilities'},@{execute=$Command;id='result'})) {
        if ($item.execute -eq 'screendump') { $item.arguments=@{filename=(Join-Path $run 'guest-screen.png');format='png'} }
        $writer.WriteLine(($item | ConvertTo-Json -Compress -Depth 4))
        do {
            $line=$reader.ReadLine()
            if ($null -eq $line) { throw 'QMP closed without a reply' }
            $reply=$line | ConvertFrom-Json
        } while ($reply.id -ne $item.id)
        if ($reply.error) { throw ($reply.error | ConvertTo-Json -Compress) }
        if ($item.id -eq 'result') { $reply | ConvertTo-Json -Depth 6 }
    }
} finally { $client.Dispose() }
