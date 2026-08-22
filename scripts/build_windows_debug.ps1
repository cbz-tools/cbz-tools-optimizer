param(
  [string]$Workspace = (Get-Location).Path
)

$ErrorActionPreference = "Stop"

Push-Location $Workspace
try {
  cargo build --locked -p cbz-tools-optimizer-cli
  if ($LASTEXITCODE -ne 0) { throw "CLI core build failed" }

  cargo build --locked -p cbz-tools-optimizer-gui
  if ($LASTEXITCODE -ne 0) { throw "GUI core build failed" }

  cargo build --locked -p cbz-tools-optimizer-launcher --features windows-launcher
  if ($LASTEXITCODE -ne 0) { throw "launcher build failed" }

  $debugDir = Join-Path $Workspace "target/debug"
  foreach ($name in @("cbz-opt-core.exe", "cbz-opt-gui-core.exe", "cbz-opt.exe", "cbz-opt-gui.exe", "dav1d.dll")) {
    $path = Join-Path $debugDir $name
    if (!(Test-Path -LiteralPath $path -PathType Leaf)) {
      throw "Expected Windows debug input was not produced: $path"
    }
  }
  $stagedUnrar = @(Get-ChildItem -LiteralPath $debugDir -File -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match '^UnRAR(?:64)?\.dll$' })
  if ($stagedUnrar.Count -gt 0) {
    throw "UnRAR DLLs must not be staged: $($stagedUnrar.Name -join ', ')"
  }
}
finally {
  Pop-Location
}
