# Measures Sightline per state and window width: start time is not covered here (see
# docs/renderer-check.md); this reports memory, CPU, threads and open network connections.
# Sizes and positions are physical pixels. Each run opens the window for about 16 seconds.
param(
    [string]$Exe,
    [int]$OriginX = -3840,
    [int]$OriginY = -338,
    [string[]]$Sizes = @('720x2112', '1920x2112'),
    # 'live' runs against the real client (Closed when the game is not running).
    [string[]]$States = @('live', 'menu', 'lobby', 'match', 'post'),
    # Database file for the 'live' runs, so a measurement never touches the real one.
    [string]$Db = (Join-Path $env:TEMP 'sightline-measure.db')
)

$ErrorActionPreference = 'Stop'
if (-not $Exe) { $Exe = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\sightline.exe' }
$Exe = (Resolve-Path -LiteralPath $Exe).Path
$cores = [Environment]::ProcessorCount
foreach ($size in $Sizes) {
    foreach ($state in $States) {
        $a = @('--exit-after', '16', '--pos', "$OriginX,$OriginY", '--size', $size)
        if ($state -ne 'live') { $a += @('--fixture', $state) } else { $a += @('--db', $Db) }
        $p = Start-Process -FilePath $Exe -ArgumentList $a -PassThru
        Start-Sleep -Seconds 5
        $cpu0 = (Get-Process -Id $p.Id).CPU
        $sw = [Diagnostics.Stopwatch]::StartNew()
        Start-Sleep -Seconds 8
        $g = Get-Process -Id $p.Id
        $cpu1 = $g.CPU
        $wall = $sw.Elapsed.TotalSeconds
        $tcp = @(Get-NetTCPConnection -OwningProcess $p.Id -ErrorAction SilentlyContinue).Count
        [pscustomobject]@{
            Size          = $size
            State         = $state
            PrivateMB     = [math]::Round($g.PrivateMemorySize64 / 1MB, 1)
            WorkingSetMB  = [math]::Round($g.WorkingSet64 / 1MB, 1)
            CpuOneCorePct = [math]::Round(100 * ($cpu1 - $cpu0) / $wall, 2)
            CpuTotalPct   = [math]::Round(100 * ($cpu1 - $cpu0) / $wall / $cores, 3)
            Threads       = $g.Threads.Count
            TcpConns      = $tcp
        }
        $p.WaitForExit(20000) | Out-Null
    }
}
