# Fetches VALORANT art from media.valorant-api.com (agent faces, rank badges, agent portraits, map
# banners), shrinks it to the sizes the window draws, and writes ui\art.slint, which maps an
# agent name, a rank tier or a map name to its picture.
#
#   tools\make-art.ps1 -Download   fetch anything missing into assets\art\src, then build
#   tools\make-art.ps1             rebuild from what is already there (no network)
#
# The originals stay in assets\art\src (about 42 MB, git-ignored). Opens no window.
param([switch]$Download)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$src = Join-Path $root 'assets\art\src'
$out = Join-Path $root 'assets\art'
foreach ($d in 'faces', 'ranks', 'portraits', 'maps') { New-Item -ItemType Directory -Force (Join-Path $src $d) | Out-Null }
foreach ($d in 'face', 'rank', 'panel', 'map') { New-Item -ItemType Directory -Force (Join-Path $out $d) | Out-Null }
Add-Type -AssemblyName System.Drawing

function Key([string]$name) { ($name.ToLowerInvariant() -replace '[^a-z0-9]', '') }
function Fetch([string]$url, [string]$path) {
    if (Test-Path $path) { return }
    Invoke-WebRequest -Uri $url -OutFile $path -UseBasicParsing -TimeoutSec 90
}

$index = Join-Path $src 'index.json'
if ($Download) {
    $agents = (Invoke-RestMethod 'https://valorant-api.com/v1/agents?isPlayableCharacter=true' -TimeoutSec 30).data |
        Sort-Object displayName | ForEach-Object { [pscustomobject]@{ name = $_.displayName; uuid = $_.uuid; tint = $_.backgroundGradientColors[0].Substring(0, 6) } }
    $set = (Invoke-RestMethod 'https://valorant-api.com/v1/competitivetiers' -TimeoutSec 30).data[-1]
    $tiers = $set.tiers | Where-Object { $_.largeIcon } | ForEach-Object { [pscustomobject]@{ tier = $_.tier; url = $_.largeIcon } }
    $maps = (Invoke-RestMethod 'https://valorant-api.com/v1/maps' -TimeoutSec 30).data | Where-Object { $_.listViewIcon } |
        Sort-Object displayName | ForEach-Object { [pscustomobject]@{ name = $_.displayName; uuid = $_.uuid } }
    # Two maps can share a display name (a range and its variant): keep the first of each.
    $maps = $maps | Group-Object name | ForEach-Object { $_.Group[0] }
    [pscustomobject]@{ agents = @($agents); tiers = @($tiers); maps = @($maps) } | ConvertTo-Json -Depth 4 | Set-Content $index -Encoding utf8
    foreach ($a in $agents) {
        Fetch "https://media.valorant-api.com/agents/$($a.uuid)/displayicon.png" (Join-Path $src "faces\$(Key $a.name).png")
        Fetch "https://media.valorant-api.com/agents/$($a.uuid)/fullportrait.png" (Join-Path $src "portraits\$(Key $a.name).png")
    }
    foreach ($t in $tiers) { Fetch $t.url (Join-Path $src "ranks\$($t.tier).png") }
    foreach ($m in $maps) { Fetch "https://media.valorant-api.com/maps/$($m.uuid)/listviewicon.png" (Join-Path $src "maps\$(Key $m.name).png") }
}
if (-not (Test-Path $index)) { throw "No art yet. Run with -Download first." }
$data = Get-Content $index -Raw | ConvertFrom-Json

$jpeg = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq 'image/jpeg' }
function SaveJpeg($bmp, [string]$path, [int]$quality) {
    $p = New-Object System.Drawing.Imaging.EncoderParameters 1
    $p.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality, [long]$quality)
    $bmp.Save($path, $jpeg, $p)
}
function Canvas([int]$w, [int]$h) {
    $bmp = New-Object System.Drawing.Bitmap $w, $h, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = 'HighQualityBicubic'; $g.PixelOffsetMode = 'HighQuality'; $g.SmoothingMode = 'HighQuality'; $g.CompositingQuality = 'HighQuality'
    , @($bmp, $g)
}
function Rgb([string]$hex, [int]$alpha = 255) {
    [System.Drawing.Color]::FromArgb($alpha, [Convert]::ToInt32($hex.Substring(0, 2), 16), [Convert]::ToInt32($hex.Substring(2, 2), 16), [Convert]::ToInt32($hex.Substring(4, 2), 16))
}
# Draws the image so it covers the whole area; $focusY says which part to keep when cropping (0 top, 1 bottom).
function Cover($g, $img, [int]$w, [int]$h, [double]$focusY) {
    $scale = [math]::Max($w / $img.Width, $h / $img.Height)
    $dw = $img.Width * $scale; $dh = $img.Height * $scale
    $g.DrawImage($img, [single](($w - $dw) / 2), [single](($h - $dh) * $focusY), [single]$dw, [single]$dh)
}
function Fade($g, [int]$x, [int]$y, [int]$w, [int]$h, [bool]$vertical, $colors, [single[]]$stops) {
    $rect = New-Object System.Drawing.Rectangle $x, $y, $w, $h
    $mode = if ($vertical) { [System.Drawing.Drawing2D.LinearGradientMode]::Vertical } else { [System.Drawing.Drawing2D.LinearGradientMode]::Horizontal }
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, ([System.Drawing.Color]::Black), ([System.Drawing.Color]::Black), $mode
    $blend = New-Object System.Drawing.Drawing2D.ColorBlend $colors.Count
    $blend.Colors = $colors; $blend.Positions = $stops
    $brush.InterpolationColors = $blend
    $g.FillRectangle($brush, $rect); $brush.Dispose()
}

$navy = '0f1923'; $panel = '141e29'
$made = @{ face = 0; rank = 0; panel = 0; map = 0 }
foreach ($a in $data.agents) {
    $k = Key $a.name
    # Face: 52 x 58 on screen, kept at twice that for sharp text-size scaling.
    $file = Join-Path $src "faces\$k.png"
    if (Test-Path $file) {
        $img = [System.Drawing.Image]::FromFile($file); $c = Canvas 104 116
        $c[1].Clear((Rgb '1b2734')); Cover $c[1] $img 104 116 0.5
        SaveJpeg $c[0] (Join-Path $out "face\$k.jpg") 90
        $c[1].Dispose(); $c[0].Dispose(); $img.Dispose(); $made.face++
    }
    # Side panel: the agent over their own colour, fading into the panel. 344 x 560 on screen.
    $file = Join-Path $src "portraits\$k.png"
    if (Test-Path $file) {
        $img = [System.Drawing.Image]::FromFile($file); $w = 516; $h = 840; $c = Canvas $w $h
        Fade $c[1] 0 0 $w $h $true @((Rgb $a.tint), (Rgb $panel), (Rgb $panel)) @(0.0, 0.58, 1.0)
        $ph = 1230.0; $pw = $img.Width * ($ph / $img.Height)
        $c[1].DrawImage($img, [single](($w - $pw) / 2), [single](-144), [single]$pw, [single]$ph)
        Fade $c[1] 0 225 $w ($h - 225) $true @((Rgb $navy 0), (Rgb $navy 242), (Rgb $navy 255), (Rgb $navy 255)) @(0.0, 0.34, 0.6, 1.0)
        SaveJpeg $c[0] (Join-Path $out "panel\$k.jpg") 86
        $c[1].Dispose(); $c[0].Dispose(); $img.Dispose(); $made.panel++
    }
}
foreach ($t in $data.tiers) {
    $file = Join-Path $src "ranks\$($t.tier).png"
    if (Test-Path $file) {
        $img = [System.Drawing.Image]::FromFile($file); $c = Canvas 80 80
        $c[1].Clear([System.Drawing.Color]::Transparent); $c[1].DrawImage($img, 0, 0, 80, 80)
        $c[0].Save((Join-Path $out "rank\$($t.tier).png"), [System.Drawing.Imaging.ImageFormat]::Png)
        $c[1].Dispose(); $c[0].Dispose(); $img.Dispose(); $made.rank++
    }
}
foreach ($m in $data.maps) {
    $k = Key $m.name; $file = Join-Path $src "maps\$k.png"
    if (Test-Path $file) {
        # Header banner: 1280 x 96 on screen, darkened towards the left where the map name sits.
        $img = [System.Drawing.Image]::FromFile($file); $w = 1920; $h = 144; $c = Canvas $w $h
        $c[1].Clear((Rgb $navy)); Cover $c[1] $img $w $h 0.42
        Fade $c[1] 0 0 $w $h $false @((Rgb $navy 255), (Rgb $navy 232), (Rgb $navy 150)) @(0.0, 0.34, 1.0)
        SaveJpeg $c[0] (Join-Path $out "map\$k.jpg") 84
        $c[1].Dispose(); $c[0].Dispose(); $img.Dispose(); $made.map++
    }
}
# What is shown when there is no picture for a name: nothing.
$c = Canvas 4 4; $c[1].Clear([System.Drawing.Color]::Transparent)
$c[0].Save((Join-Path $out 'none.png'), [System.Drawing.Imaging.ImageFormat]::Png); $c[1].Dispose(); $c[0].Dispose()

# The lookup the window uses. One line per picture that exists.
$nl = "`n"
$slint = "// Written by tools\make-art.ps1 from the files in assets\art. Do not edit by hand.$nl$nl" + "export global Art {$nl"
function Lookup([string]$name, [string]$arg, [string]$type, $pairs) {
    $s = "    pure public function has-$name(${arg}: $type) -> bool {$nl        " + ((@($pairs | ForEach-Object { "$arg == $($_.test)" }) + 'false') -join "$nl            || ") + ";$nl    }$nl"
    $s += "    pure public function $name(${arg}: $type) -> image {$nl"
    foreach ($p in $pairs) { $s += "        $arg == $($p.test) ? @image-url(`"../assets/art/$($p.file)`") :$nl" }
    $s + "        @image-url(`"../assets/art/none.png`");$nl    }$nl"
}
$faces = $data.agents | Where-Object { Test-Path (Join-Path $out "face\$(Key $_.name).jpg") } | ForEach-Object { @{ test = "`"$($_.name)`""; file = "face/$(Key $_.name).jpg" } }
$panels = $data.agents | Where-Object { Test-Path (Join-Path $out "panel\$(Key $_.name).jpg") } | ForEach-Object { @{ test = "`"$($_.name)`""; file = "panel/$(Key $_.name).jpg" } }
$ranks = $data.tiers | Where-Object { Test-Path (Join-Path $out "rank\$($_.tier).png") } | ForEach-Object { @{ test = "$($_.tier)"; file = "rank/$($_.tier).png" } }
$maps = $data.maps | Where-Object { Test-Path (Join-Path $out "map\$(Key $_.name).jpg") } | ForEach-Object { @{ test = "`"$($_.name)`""; file = "map/$(Key $_.name).jpg" } }
$slint += (Lookup 'face' 'agent' 'string' $faces) + (Lookup 'panel' 'agent' 'string' $panels) + (Lookup 'rank' 'tier' 'int' $ranks) + (Lookup 'map' 'name' 'string' $maps) + "}$nl"
[System.IO.File]::WriteAllText((Join-Path $root 'ui\art.slint'), $slint, (New-Object System.Text.UTF8Encoding $false))

$size = { param($d) [math]::Round(((Get-ChildItem (Join-Path $out $d) -File | Measure-Object Length -Sum).Sum) / 1KB) }
"faces $($made.face) ($(& $size 'face') KB)  ranks $($made.rank) ($(& $size 'rank') KB)  panels $($made.panel) ($(& $size 'panel') KB)  maps $($made.map) ($(& $size 'map') KB)"
"originals: $([math]::Round(((Get-ChildItem $src -Recurse -File | Measure-Object Length -Sum).Sum) / 1MB, 1)) MB in assets\art\src"
