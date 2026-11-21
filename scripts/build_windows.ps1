param([string]$Destination = (Join-Path $PSScriptRoot '..\dist\6Stars-Win64'))

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$destinationPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($Destination)
if (Test-Path -LiteralPath $destinationPath) {
    throw "$destinationPath already exists. Use -Destination to keep your previous build."
}
$parent = New-Item -ItemType Directory -Path (Split-Path -Parent $destinationPath) -Force
$stage = New-Item -ItemType Directory -Path (Join-Path $parent.FullName ([Guid]::NewGuid().ToString()))
Push-Location -LiteralPath $root
try {
    $target = 'x86_64-pc-windows-msvc'
    cargo build --locked --release --target $target
    if ($LASTEXITCODE -ne 0) { throw '6Stars build failed.' }
    $metadata = cargo metadata --no-deps --format-version 1 --locked
    if ($LASTEXITCODE -ne 0) { throw 'Could not find the Cargo build directory.' }
    $targetDir = ($metadata | ConvertFrom-Json).target_directory
    Copy-Item -LiteralPath (Join-Path $targetDir "$target\release\6Stars.exe") -Destination $stage.FullName
    & (Join-Path $PSScriptRoot 'setup_deps.ps1') -Destination $stage.FullName
    [IO.Directory]::Move($stage.FullName, $destinationPath)
    Write-Host "Built $destinationPath\6Stars.exe"
} finally {
    Pop-Location
    if (Test-Path -LiteralPath $stage.FullName) {
        Remove-Item -LiteralPath $stage.FullName -Recurse -Force
    }
}
