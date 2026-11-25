# Starts Sightline on fixture data and saves a picture of its window. For checking the UI.
# The picture is taken from the window itself, not from the screen, so nothing else that happens
# to be on the monitor can end up in it.
param(
    [string]$Exe,
    [string]$State = 'match',
    [string]$Size = '1920x1300',
    [int]$OriginX = -3840,
    [int]$OriginY = -338,
    # Seconds to wait before the picture; the post-game result takes a few to arrive.
    [int]$Wait = 6,
    # Further switches for the app, e.g. '--settings' or '--select','a1'.
    [string[]]$Extra = @(),
    [Parameter(Mandatory)][string]$Out
)

$ErrorActionPreference = 'Stop'
# $PSScriptRoot is empty inside param() defaults when a parameter is mandatory, so resolve here.
if (-not $Exe) { $Exe = Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\sightline.exe' }
$Exe = (Resolve-Path -LiteralPath $Exe).Path
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Runtime.InteropServices;
public static class Shot {
  [DllImport("user32.dll")] public static extern IntPtr SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L,T,R,B; }
}
"@
[void][Shot]::SetProcessDpiAwarenessContext([IntPtr](-4))

$a = @('--fixture', $State, '--exit-after', "$($Wait + 3)", '--pos', "$OriginX,$OriginY", '--size', $Size) + $Extra
$p = Start-Process -FilePath $Exe -ArgumentList $a -PassThru
Start-Sleep -Seconds $Wait
$h = (Get-Process -Id $p.Id).MainWindowHandle
$r = New-Object Shot+RECT; [void][Shot]::GetClientRect($h, [ref]$r)
$bmp = New-Object System.Drawing.Bitmap $r.R, $r.B
$g = [System.Drawing.Graphics]::FromImage($bmp)
$dc = $g.GetHdc()
# 1 = client area only, 2 = ask the window to draw its full content
$ok = [Shot]::PrintWindow($h, $dc, 3)
$g.ReleaseHdc($dc)
$g.Dispose(); $bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png); $bmp.Dispose()
$p.WaitForExit(15000) | Out-Null
"saved $Out ($($r.R)x$($r.B)) drawn=$ok"
