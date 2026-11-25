# Draws Sightline's window into a PNG without opening any window: nothing appears on screen and
# nothing takes focus, so this is safe to run while a game is in the foreground.
#   tools\render.ps1 -State match -Out shot.png
#   tools\render.ps1 -State dm -Size 630x1500 -Out narrow.png
#   tools\render.ps1 -State menu -Extra '--settings' -Out settings.png
param(
    [string]$Exe,
    [string]$State = 'match',
    # Physical pixels, and the display scale they are drawn at (1.5 = the 4K monitor).
    [string]$Size = '1920x1290',
    [string]$Scale = '1.5',
    # Seconds the made-up game runs before the picture; the post-game result needs about 8.
    [int]$Seconds = 5,
    [string[]]$Extra = @(),
    [Parameter(Mandatory)][string]$Out
)
$ErrorActionPreference = 'Stop'
if (-not $Exe) { $Exe = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\debug\sightline.exe' }
$bmp = Join-Path $env:TEMP "sightline-render-$PID.bmp"
$a = @('--fixture', $State, '--render', $bmp, '--size', $Size, '--scale', $Scale, '--exit-after', "$Seconds") + $Extra
$p = Start-Process -FilePath $Exe -ArgumentList $a -NoNewWindow -PassThru
try { $p.PriorityClass = 'BelowNormal' } catch {}
if (-not $p.WaitForExit(($Seconds + 60) * 1000)) { try { $p.Kill() } catch {}; throw 'render did not finish' }
if (-not (Test-Path $bmp)) { throw "no picture was written (exit $($p.ExitCode))" }
Add-Type -AssemblyName System.Drawing
$img = [System.Drawing.Image]::FromFile($bmp)
$img.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
"saved $Out ($($img.Width)x$($img.Height))"
$img.Dispose(); Remove-Item $bmp
