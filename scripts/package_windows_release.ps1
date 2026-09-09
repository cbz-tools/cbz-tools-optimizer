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
$stageDir = [System.IO.Path]::GetFullPath((Join-Path $OutputDir $packageName))
$zipPath = [System.IO.Path]::GetFullPath((Join-Path $OutputDir "$packageName.zip"))

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

New-Item -ItemType Directory -Force -Path $stageDir | Out-Null

$releaseDir = Join-Path $Workspace "target/release"
Copy-File (Join-Path $releaseDir "cbz-opt.exe") (Join-Path $stageDir "cbz-opt.exe")
Copy-File (Join-Path $releaseDir "cbz-opt-gui.exe") (Join-Path $stageDir "cbz-opt-gui.exe")
Copy-File (Join-Path $Workspace "README.md") (Join-Path $stageDir "README.md")
Copy-File (Join-Path $Workspace "LICENSE") (Join-Path $stageDir "LICENSE")
Copy-File (Join-Path $Workspace "THIRDPARTY_LICENSES.md") (Join-Path $stageDir "THIRDPARTY_LICENSES.md")
Copy-File (Join-Path $Workspace "third_party/dav1d/LICENSE") (Join-Path $stageDir "third_party/dav1d/LICENSE")
Copy-File (Join-Path $Workspace "third_party/svt-av1/LICENSE") (Join-Path $stageDir "third_party/svt-av1/LICENSE")
Copy-File (Join-Path $Workspace "third_party/shiguredo_svt_av1/LICENSE") (Join-Path $stageDir "third_party/shiguredo_svt_av1/LICENSE")

$expectedFiles = @(
  "cbz-opt.exe",
  "cbz-opt-gui.exe",
  "README.md",
  "LICENSE",
  "THIRDPARTY_LICENSES.md",
  "third_party/dav1d/LICENSE",
  "third_party/svt-av1/LICENSE",
  "third_party/shiguredo_svt_av1/LICENSE"
)
$actualFiles = @(Get-ChildItem -LiteralPath $stageDir -Recurse -File |
  ForEach-Object { $_.FullName.Substring($stageDir.Length + 1).Replace('\', '/') } |
  Sort-Object)
if (@(Compare-Object ($expectedFiles | Sort-Object) $actualFiles).Count -gt 0) {
  throw "Windows package staging contents differ from the public allowlist: $($actualFiles -join ', ')"
}
if (@(Get-ChildItem -LiteralPath $stageDir -Recurse -File | Where-Object { $_.Extension -ieq '.dll' -or $_.Name -match 'core\.exe$' }).Count -gt 0) {
  throw "Windows package staging contains a runtime DLL or core executable"
}

Compress-Archive -Path $stageDir -DestinationPath $zipPath

Add-Type -AssemblyName System.IO.Compression.FileSystem
$archive = [System.IO.Compression.ZipFile]::OpenRead($zipPath)
try {
  $packageRoot = "$packageName/"
  $zipFiles = @($archive.Entries | Where-Object { !$_.FullName.EndsWith('/') } |
    ForEach-Object { $_.FullName.Substring($packageRoot.Length) } |
    Sort-Object)
  if (@(Compare-Object ($expectedFiles | Sort-Object) $zipFiles).Count -gt 0) {
    throw "Windows release ZIP contents differ from the public allowlist: $($zipFiles -join ', ')"
  }
}
finally {
  $archive.Dispose()
}
