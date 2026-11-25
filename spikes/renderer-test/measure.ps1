# Measures the renderer test: start time, memory, CPU and frame rate per window size and mode.
# Sizes and positions are physical pixels. Run from a normal desktop session; each run opens the
# test window for -Seconds seconds.
param(
    [string]$Exe = "$PSScriptRoot\target\release\renderer-test.exe",
    [int]$OriginX = -3840,
    [int]$OriginY = -338,
    [string[]]$Sizes = @('720x2112', '1920x2112', '3840x2112'),
    [int[]]$Modes = @(0, 1, 2),
    [int]$Seconds = 16
)

$out = Join-Path $PSScriptRoot 'out'
New-Item -ItemType Directory -Force $out | Out-Null
$cores = [Environment]::ProcessorCount

function Get-GpuPercent([int]$ProcessId) {
    # Sum of all GPU engine utilisation for this process; 0 when the process has no GPU engine at all.
    try {
        $s = (Get-Counter '\GPU Engine(*)\Utilization Percentage' -ErrorAction Stop).CounterSamples |
            Where-Object { $_.InstanceName -like "pid_${ProcessId}_*" }
        if (-not $s) { return 0 }
        return ($s | Measure-Object CookedValue -Sum).Sum
    } catch { return $null }
}

foreach ($size in $Sizes) {
    foreach ($mode in $Modes) {
        $log = Join-Path $out "run_${size}_m${mode}.txt"
        Remove-Item $log -ErrorAction SilentlyContinue
        $launch = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
        $p = Start-Process -FilePath $Exe -ArgumentList @('--mode', $mode, '--size', $size, '--pos', "$OriginX,$OriginY", '--seconds', $Seconds) -RedirectStandardOutput $log -PassThru
        Start-Sleep -Seconds 4
        $cpu0 = (Get-Process -Id $p.Id).CPU
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $gpu = @()
        while ($sw.Elapsed.TotalSeconds -lt 8) { $g = Get-GpuPercent $p.Id; if ($null -ne $g) { $gpu += $g }; Start-Sleep -Milliseconds 700 }
        $gp = Get-Process -Id $p.Id
        $cpu1 = $gp.CPU
        $wall = $sw.Elapsed.TotalSeconds
        $private = $gp.PrivateMemorySize64 / 1MB
        $ws = $gp.WorkingSet64 / 1MB
        $peak = $gp.PeakWorkingSet64 / 1MB
        $p.WaitForExit(20000) | Out-Null
        $lines = Get-Content $log -ErrorAction SilentlyContinue
        $startMs = $null; $sinceMain = $null; $actual = $null
        $first = $lines | Where-Object { $_ -like 'first_frame*' } | Select-Object -First 1
        if ($first -match 'unix_ms=(\d+) since_main_ms=([\d.]+)') { $startMs = [int64]$Matches[1] - $launch; $sinceMain = $Matches[2] }
        $fpsLines = $lines | Where-Object { $_ -like 'fps=*' }
        if (($fpsLines | Select-Object -Last 1) -match 'size=(\S+) scale=(\S+)') { $actual = "$($Matches[1]) @ $($Matches[2])x" }
        $fps = $fpsLines | ForEach-Object { if ($_ -match '^fps=(\d+)') { [int]$Matches[1] } }
        $steady = $fps | Select-Object -Skip 3
        $fpsAvg = if ($steady) { [math]::Round(($steady | Measure-Object -Average).Average, 1) } else { 0 }
        $fpsMin = if ($steady) { ($steady | Measure-Object -Minimum).Minimum } else { 0 }
        $cpuOneCore = 100 * ($cpu1 - $cpu0) / $wall
        $msPerFrame = if ($fpsAvg -gt 0) { [math]::Round(1000 * ($cpu1 - $cpu0) / ($fpsAvg * $wall), 2) } else { 0 }
        [pscustomobject]@{
            Size          = $size
            Actual        = $actual
            Mode          = $mode
            StartMs       = $startMs
            MainToFrameMs = $sinceMain
            PrivateMB     = [math]::Round($private, 1)
            WorkingSetMB  = [math]::Round($ws, 1)
            PeakWsMB      = [math]::Round($peak, 1)
            CpuOneCorePct = [math]::Round($cpuOneCore, 2)
            CpuTotalPct   = [math]::Round($cpuOneCore / $cores, 3)
            FpsAvg        = $fpsAvg
            FpsMin        = $fpsMin
            CpuMsPerFrame = $msPerFrame
            GpuPct        = if ($gpu.Count) { [math]::Round(($gpu | Measure-Object -Average).Average, 2) } else { 'n/a' }
        }
    }
}
