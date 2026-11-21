# installs optional bink2 playback dependency binkplay.exe beside percystar.exe
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

[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$tmpExe = Join-Path $env:TEMP "RADTools.exe"
$tmpExt = Join-Path $env:TEMP "radtools_ext"
Write-Host "Downloading official RAD Video Tools..."
Invoke-WebRequest -UseBasicParsing -Uri "https://www.radgametools.com/down/Bink/RADTools.exe" -OutFile $tmpExe
if (Get-Command 7z -ErrorAction SilentlyContinue) {
    7z x $tmpExe "-o$tmpExt" -y | Out-Null
    $bp = Get-ChildItem $tmpExt -Recurse -Filter "binkplay.exe" | Select-Object -First 1
    if ($bp) {
        Copy-Item $bp.FullName "$outDir\binkplay.exe" -Force
        Write-Host "Installed binkplay.exe to $outDir"
    }
} else {
    Write-Host "Downloaded $tmpExe — extract binkplay.exe beside PercyStar.exe"
}
