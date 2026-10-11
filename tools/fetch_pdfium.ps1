# Downloads the pinned PDFium build (bblanchon/pdfium-binaries, chromium/7881) into vendor/pdfium.
# This version matches the `pdfium_latest` bindings of pdfium-render 0.9.4.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root 'vendor\pdfium'
$url = 'https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7881/pdfium-win-x64.tgz'
$sha = '73cc0de638ac2095e7445bf56a38200a5b7c7ca0e9f4ba144598f2457377ac08'
$tmp = Join-Path ([IO.Path]::GetTempPath()) ('pdfium-' + [Guid]::NewGuid())
New-Item -ItemType Directory -Force $tmp, $dest | Out-Null
$tgz = Join-Path $tmp 'pdfium.tgz'
Invoke-WebRequest -Uri $url -OutFile $tgz -UseBasicParsing
if ((Get-FileHash $tgz -Algorithm SHA256).Hash.ToLower() -ne $sha) { throw 'PDFium download hash mismatch' }
tar -xzf $tgz -C $tmp
Copy-Item (Join-Path $tmp 'bin\pdfium.dll') $dest -Force
Copy-Item (Join-Path $tmp 'LICENSE') (Join-Path $dest 'LICENSE') -Force
Remove-Item -Recurse -Force $tmp
Write-Host "PDFium ready in $dest"
