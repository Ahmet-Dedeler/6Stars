param([string]$Destination = (Join-Path $PSScriptRoot '..\target\release'))

$ErrorActionPreference = 'Stop'
$archiveHash = 'c5b9128db8fe7c5bef85bff711da5ff4fed313a8e4ae49d9933a324fe34313aa'
$playerHash = 'a5e30be7409ba2ad6dede57de7a44df14fce94d3e2660e71ff4d2240c45fa78f'
$sevenZip = Get-Command 7z.exe, 7zz.exe -CommandType Application -ErrorAction SilentlyContinue |
    Select-Object -First 1 -ExpandProperty Source
if (!$sevenZip) {
    $sevenZip = Join-Path $env:ProgramFiles '7-Zip\7z.exe'
}
if (!(Test-Path -LiteralPath $sevenZip -PathType Leaf)) {
    throw '7-Zip is required at build time to extract the RAD player.'
}

$temp = New-Item -ItemType Directory -Path (Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid().ToString()))
try {
    $archive = Join-Path $temp.FullName 'RADTools.7z'
    $installer = Join-Path $temp.FullName 'radtools.exe'
    $player = Join-Path $temp.FullName 'binkplay.exe'
    Invoke-WebRequest -UseBasicParsing -Uri 'https://www.radgametools.com/down/Bink/RADTools.7z' -OutFile $archive
    if ((Get-FileHash -LiteralPath $archive).Hash -ne $archiveHash) {
        throw 'The RAD download has changed. Update the pinned checksums before building.'
    }
    # The outer 7z contains an NSIS installer. Extract the player without running it.
    & $sevenZip x $archive '-pRAD' "-o$($temp.FullName)" 'radtools.exe' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not extract radtools.exe.' }
    & $sevenZip x $installer "-o$($temp.FullName)" 'binkplay.exe' | Out-Null
    if ($LASTEXITCODE -ne 0 -or (Get-FileHash -LiteralPath $player).Hash -ne $playerHash) {
        throw 'Could not verify the extracted binkplay.exe.'
    }
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null
    Copy-Item -LiteralPath $player -Destination (Join-Path $Destination 'binkplay.exe')
    Write-Host "Added the RAD player to $Destination"
} finally {
    Remove-Item -LiteralPath $temp.FullName -Recurse -Force
}
