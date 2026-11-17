# Builds the WebAssembly core and copies the artifact to /public/core.
# Usage: powershell -ExecutionPolicy Bypass -File core/build.ps1
$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
if (Test-Path $cargoBin) { $cargo = $cargoBin } else { $cargo = 'cargo' }

& $cargo build --release --target wasm32-unknown-unknown --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

$outDir = Join-Path $root 'public\core'
New-Item -ItemType Directory -Force $outDir | Out-Null
Copy-Item (Join-Path $PSScriptRoot 'target\wasm32-unknown-unknown\release\ttfont_core.wasm') (Join-Path $outDir 'ttfont_core.wasm') -Force

$size = (Get-Item (Join-Path $outDir 'ttfont_core.wasm')).Length
Write-Host "WASM built: public/core/ttfont_core.wasm ($size bytes)"
