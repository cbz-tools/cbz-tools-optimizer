param(
  [Parameter(Mandatory = $true)]
  [string]$Version,

  [Parameter(Mandatory = $true)]
  [string]$Workspace,

  [Parameter(Mandatory = $true)]
  [string]$OutputDir
)

$ErrorActionPreference = "Stop"

$packageName = "cbz-tools-optimizer-$Version-windows-x64"
$stageDir = Join-Path $OutputDir $packageName
$zipPath = Join-Path $OutputDir "$packageName.zip"
$shaPath = "$zipPath.sha256"

function Copy-File {
  param(
    [Parameter(Mandatory = $true)]
    [string]$Source,

    [Parameter(Mandatory = $true)]
    [string]$Destination
  )

  $parent = Split-Path $Destination -Parent
  if ($parent) {
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
  }
  Copy-Item $Source $Destination -Force
}

if (Test-Path $stageDir) {
  Remove-Item $stageDir -Recurse -Force
}
if (Test-Path $zipPath) {
  Remove-Item $zipPath -Force
}
if (Test-Path $shaPath) {
  Remove-Item $shaPath -Force
}

New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

Copy-File (Join-Path $Workspace "target/x86_64-pc-windows-msvc/release/cbz-opt.exe") (Join-Path $stageDir "cbz-opt.exe")
Copy-File (Join-Path $Workspace "target/x86_64-pc-windows-msvc/release/cbz-opt-gui.exe") (Join-Path $stageDir "cbz-opt-gui.exe")
Copy-File (Join-Path $Workspace "third_party/dav1d/dav1d.dll") (Join-Path $stageDir "dav1d.dll")
Copy-File (Join-Path $Workspace "README.md") (Join-Path $stageDir "README.md")
Copy-File (Join-Path $Workspace "LICENSE") (Join-Path $stageDir "LICENSE")
Copy-File (Join-Path $Workspace "THIRDPARTY_LICENSES.md") (Join-Path $stageDir "THIRDPARTY_LICENSES.md")
Copy-File (Join-Path $Workspace "third_party/dav1d/LICENSE") (Join-Path $stageDir "third_party/dav1d/LICENSE")
Copy-File (Join-Path $Workspace "third_party/svt-av1/LICENSE") (Join-Path $stageDir "third_party/svt-av1/LICENSE")
Copy-File (Join-Path $Workspace "third_party/shiguredo_svt_av1/LICENSE") (Join-Path $stageDir "third_party/shiguredo_svt_av1/LICENSE")

Compress-Archive -Path $stageDir -DestinationPath $zipPath

$hash = Get-FileHash -Algorithm SHA256 -Path $zipPath
"$($hash.Hash.ToLowerInvariant())  $($hash.Path | Split-Path -Leaf)" | Set-Content -Path $shaPath
