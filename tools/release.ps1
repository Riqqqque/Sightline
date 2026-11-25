# Builds a release and, with -Publish, puts it on GitHub.
#
#   tools\release.ps1            build, test, and write dist\release\Sightline.exe + its SHA-256
#   tools\release.ps1 -Publish   the same, then create the GitHub release for this version
#
# The version comes from Cargo.toml. The art must have been fetched (tools\make-art.ps1 -Download),
# or the release would ship without it. The update feature in the app looks for exactly these two
# file names on the newest release: Sightline.exe and Sightline.exe.sha256.
param([switch]$Publish, [string]$Notes)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root
# Paths inside the program are written relative, so an exe never carries the name of the Windows
# account or the folder it was built in.
$env:CARGO_ENCODED_RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE=~" + [char]0x1f + "--remap-path-prefix=$((Get-Location).Path)=."
$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
if (-not (Test-Path 'assets\art\face')) { throw 'No art: run tools\make-art.ps1 -Download first.' }

cargo test --locked
if ($LASTEXITCODE -ne 0) { throw 'tests failed' }
cargo build --release --locked
if ($LASTEXITCODE -ne 0) { throw 'build failed' }

$out = Join-Path $root 'dist\release'
New-Item -ItemType Directory -Force $out | Out-Null
$exe = Join-Path $out 'Sightline.exe'
Copy-Item 'target\release\sightline.exe' $exe -Force
# A last look that nothing personal went in.
$text = [System.Text.Encoding]::GetEncoding(28591).GetString([System.IO.File]::ReadAllBytes($exe))
if ($text.Contains($env:USERNAME) -or $text.Contains($env:USERPROFILE)) { throw 'The exe contains this PC account name: the path remapping did not apply.' }
$hash = (Get-FileHash $exe -Algorithm SHA256).Hash.ToLowerInvariant()
# The same layout sha256sum writes, without a byte-order mark.
[System.IO.File]::WriteAllText("$exe.sha256", "$hash  Sightline.exe`n", (New-Object System.Text.UTF8Encoding $false))
"Sightline $version"
"  $exe ($([math]::Round((Get-Item $exe).Length / 1MB, 1)) MB)"
"  SHA-256 $hash"

if ($Publish) {
    $tag = "v$version"
    if (-not $Notes) { $Notes = "See CHANGELOG.md.`n`nSHA-256 of Sightline.exe: ``$hash``" }
    gh release create $tag $exe "$exe.sha256" --title "Sightline $version" --notes $Notes
    if ($LASTEXITCODE -ne 0) { throw 'gh release create failed' }
    "published $tag"
}
