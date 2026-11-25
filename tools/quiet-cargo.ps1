# Runs cargo at low priority on four cores and opens no window, so a game in the foreground
# keeps its frames. Use it for any build or test while VALORANT is running.
#   tools\quiet-cargo.ps1 -CargoArgs 'test'
#   tools\quiet-cargo.ps1 -CargoArgs 'build','--release'
param([Parameter(Mandatory)][string[]]$CargoArgs, [int]$Lines = 25)
Set-Location (Split-Path -Parent $PSScriptRoot)
# Paths inside the program are written relative, so an exe never carries the name of the Windows
# account or the folder it was built in.
$env:CARGO_ENCODED_RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE=~" + [char]0x1f + "--remap-path-prefix=$((Get-Location).Path)=."
$out = Join-Path $env:TEMP 'sightline-cargo-out.txt'
$err = Join-Path $env:TEMP 'sightline-cargo-err.txt'
$p = Start-Process cargo -ArgumentList ($CargoArgs + @('-j', '4')) -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError $err
# Set straight away: the compiler processes cargo starts inherit a below-normal priority.
try { $p.PriorityClass = 'BelowNormal' } catch {}
$p.WaitForExit()
Get-Content $err | Where-Object { $_ -match '^(error|warning)|-->|Finished|panicked|^\s+(left|right):|assert' } | Select-Object -First $Lines
Get-Content $out | Where-Object { $_ -match 'test result|FAILED|failed|panicked|^\s+(left|right):|^---- ' } | Select-Object -First $Lines
