# installs optional bink2 playback depndency binkplay exe beside percystar exe
$ErrorActionPreference = "Stop"
$dir = Split-Path -Parent $MyInvocation.MyCommand.Path
$root = Split-Path -Parent $dir
$outDir = if (Test-Path "$root\target\release\PercyStar.exe") { "$root\target\release" } else { $root }

$radDefault = "C:\Program Files (x86)\RADVideo\binkplay.exe"
if (Test-Path $radDefault) {
    Copy-Item $radDefault "$outDir\binkplay.exe" -Force
    Write-Host "Copied binkplay.exe from RADVideo to $outDir"
    exit 0
}

$tmpZip = Join-Path $env:TEMP "RADTools.7z"
$tmpExt = Join-Path $env:TEMP "radtools_ext"
Write-Host "Downloading official RAD Video Tools..."
Invoke-WebRequest -Uri "https://www.radgametools.com/down/Bink/RADTools.7z" -OutFile $tmpZip
if (Get-Command 7z -ErrorAction SilentlyContinue) {
    7z x $tmpZip "-pRAD" "-o$tmpExt" -y | Out-Null
    $bp = Get-ChildItem $tmpExt -Recurse -Filter "binkplay.exe" | Select-Object -First 1
    if ($bp) {
        Copy-Item $bp.FullName "$outDir\binkplay.exe" -Force
        Write-Host "Installed binkplay.exe to $outDir"
    }
} else {
    Write-Host "Downloaded $tmpZip password RAD extract binkplay.exe beside PercyStar.exe"
}
