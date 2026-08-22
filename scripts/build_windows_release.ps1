param(
  [string]$Workspace = (Get-Location).Path
)

$ErrorActionPreference = "Stop"

Push-Location $Workspace
try {
  cargo build --release --locked -p cbz-tools-optimizer-cli
  if ($LASTEXITCODE -ne 0) { throw "CLI core build failed" }

  cargo build --release --locked -p cbz-tools-optimizer-gui
  if ($LASTEXITCODE -ne 0) { throw "GUI core build failed" }

  cargo build --release --locked -p cbz-tools-optimizer-launcher --features windows-launcher
  if ($LASTEXITCODE -ne 0) { throw "launcher build failed" }

  $releaseDir = Join-Path $Workspace "target/release"
  $coreBinaries = @(
    (Join-Path $releaseDir "cbz-opt-core.exe"),
    (Join-Path $releaseDir "cbz-opt-gui-core.exe")
  )
  $launcherBinaries = @(
    (Join-Path $releaseDir "cbz-opt.exe"),
    (Join-Path $releaseDir "cbz-opt-gui.exe")
  )
  foreach ($binary in ($coreBinaries + $launcherBinaries)) {
    if (!(Test-Path -LiteralPath $binary -PathType Leaf)) {
      throw "Expected Windows binary was not produced: $binary"
    }
  }

  $dav1d = Join-Path $releaseDir "dav1d.dll"
  if (!(Test-Path -LiteralPath $dav1d -PathType Leaf)) {
    throw "The staged dav1d runtime is missing: $dav1d"
  }
  $stagedUnrar = @(Get-ChildItem -LiteralPath $releaseDir -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match '^UnRAR(?:64)?\.dll$' })
  if ($stagedUnrar.Count -gt 0) {
    throw "UnRAR DLLs must not be staged: $($stagedUnrar.Name -join ', ')"
  }

  $cacheFiles = @(Get-ChildItem (Join-Path $Workspace "target") -Recurse -File -Filter CMakeCache.txt |
    Where-Object { $_.FullName -match "turbojpeg-sys" -and $_.FullName -match "[\\/]release[\\/]build[\\/]" })
  if (!$cacheFiles) {
    throw "TurboJPEG CMakeCache.txt was not found under the release target directory"
  }
  $crtMatch = $cacheFiles | Select-String -Pattern "WITH_CRT_DLL:BOOL=ON" -SimpleMatch
  $toolchainMatch = $cacheFiles | Select-String -Pattern "CMAKE_TOOLCHAIN_FILE:FILEPATH=.*turbojpeg-msvc-runtime.cmake"
  if (!$crtMatch -or !$toolchainMatch) {
    throw "TurboJPEG CMakeCache does not confirm WITH_CRT_DLL=ON and the repository toolchain file"
  }

  $dumpbinCommand = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
  $dumpbin = if ($dumpbinCommand) { $dumpbinCommand.Source } else { $null }
  if (!$dumpbin) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio/Installer/vswhere.exe"
    if (Test-Path -LiteralPath $vswhere) {
      $installPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1
      if ($installPath) {
        $dumpbin = Get-ChildItem (Join-Path $installPath "VC/Tools/MSVC") -Recurse -File -Filter dumpbin.exe |
          Sort-Object FullName -Descending | Select-Object -First 1 -ExpandProperty FullName
      }
    }
  }
  if (!$dumpbin) { throw "dumpbin.exe was not found" }

  foreach ($binary in ($coreBinaries + $launcherBinaries)) {
    $dependents = (& $dumpbin /dependents $binary 2>&1 | Out-String)
    if ($dependents -match "(?im)^\s*(turbojpeg|libjpeg|jpeg)[^\r\n]*\.dll\s*$") {
      throw "Static TurboJPEG validation failed for $binary"
    }
    if ($dependents -match "(?im)^\s*UnRAR(?:64)?\.dll\s*$") {
      throw "UnRAR DLL import validation failed for $binary"
    }
  }
  foreach ($launcher in $launcherBinaries) {
    $dependents = (& $dumpbin /dependents $launcher 2>&1 | Out-String)
    if ($dependents -match "(?im)^\s*dav1d\.dll\s*$") {
      throw "Launcher must embed dav1d rather than import it: $launcher"
    }
  }
}
finally {
  Pop-Location
}
