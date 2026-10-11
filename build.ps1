# Builds ScrubDaddy.exe into .\dist
#   powershell -ExecutionPolicy Bypass -File .\build.ps1
# Needs: Rust (https://rustup.rs) and Visual Studio Build Tools with "Desktop development with C++".
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw 'Rust is not installed. Get it from https://rustup.rs, then run this script again.'
}
if (-not (Test-Path 'vendor\pdfium\pdfium.dll')) {
    Write-Host 'Fetching the PDF engine (PDFium)...'
    & "$PSScriptRoot\tools\fetch_pdfium.ps1"
}

# The MSVC linker chokes on very long paths; build somewhere short if needed.
if ($PSScriptRoot.Length -gt 100 -and -not $env:CARGO_TARGET_DIR) {
    $env:CARGO_TARGET_DIR = Join-Path $env:LOCALAPPDATA 'ScrubDaddy\build'
}
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $PSScriptRoot 'target' }

cargo test --release
if ($LASTEXITCODE -ne 0) { throw 'Tests failed.' }
cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'Build failed.' }

New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item (Join-Path $target 'release\scrubdaddy.exe') 'dist\ScrubDaddy.exe' -Force
$size = [math]::Round((Get-Item 'dist\ScrubDaddy.exe').Length / 1MB, 1)
Write-Host "Built dist\ScrubDaddy.exe ($size MB)"
