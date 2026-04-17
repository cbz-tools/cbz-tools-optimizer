# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.1.1] - 2026-04-18

### Added
- AVIF output support (`--output-format avif`) via pure-Rust ravif encoder
- `--convert-only` flag: format conversion without resizing; same-format files are passed through without re-encoding (zero degradation)

### Changed
- GUI: quality slider now enabled only when output is JPEG (or Original without `--convert-only`)
- GUI: preset/size options disabled in settings when convert-only is active
- GUI: settings summary bar shows "Convert only" instead of preset when `--convert-only` is set
- GUI: settings window centered on the main window
- GUI: window position is remembered across restarts; previous monitor is restored on next launch
- GUI: "Reset to defaults" button added to settings window
- Removed GitHub Sponsors references from all sources

### Fixed
- GUI: `last_window_pos` now correctly initialized from saved config on startup (window position was not restored on the first exit after launch)

---

## [0.1.0] - 2026-04-16

### Added

#### CLI (`cbz-image-optimizer`)
- Parallel image resizing for ZIP/CBZ archives (ZIP-level and image-level parallelism via rayon)
- Size presets: `ipad` (2048×1536, default), `ipad-air` (2360×1640), `ipad-pro` (2732×2048), `kindle` (1264×1680), `hd` (1280×720), `full-hd` (1920×1080), `four-k` (3840×2160), `custom`
- Output format selection: `jpeg` (default), `png`, `webp`, `original`
- Supported input formats: JPEG, PNG, WebP (static), BMP, TIFF
- Animated WebP and GIF detection — archives containing animations are skipped entirely
- `--preset` — size preset selection
- `--max-width` / `--max-height` — custom dimensions (used when `--preset custom`)
- `--quality` — JPEG quality (default: 85)
- `--suffix` — output filename suffix (default: `_new`)
- `--output-dir` — output directory (default: same as input)
- `--threads` — thread count (default: `0` = half of logical CPUs)
- `--output-format` — output image format
- `--log-mode` — log output: `cli` (default), `silent`, `both`, `file`
- `--overwrite-mode` — conflict resolution: `skip` (default), `overwrite`, `rename`
- `--json` — machine-readable JSON progress output (used by GUI)
- Overwrite confirmation prompt when suffix is empty (skipped in `--json` mode)
- Per-run log file: `cbz-image-optimizer_YYYYMMDD_HHMMSS.log` (when `--log-mode both` or `file`)
- Images already within size limits are passed through without re-encoding
- CatmullRom filter for high-quality downscaling
- `catch_unwind` safety: one ZIP failure does not abort the rest

#### GUI (`cbz-image-optimizer-gui`, Windows)
- Drag-and-drop ZIP/CBZ files or folders onto the window
- **Add Files…** button (file dialog via rfd) and **Add Folder…** button
- Bulk Add window for pasting multiple paths at once
- Two-column file list with color-coded status (Pending / Processing / Skipped / Error / Done)
- Settings panel (⚙): preset, format, quality, suffix, output dir, threads, overwrite mode, log mode
- Settings saved automatically to `cbz-image-optimizer-gui.toml` alongside the executable
- Multilingual UI: English / 中文 / 日本語
- Completion summary: `✔ Done | Saved: X MB → Y MB (-Z%) | ⏱ Ns`
- Image processing built in — `cbz-image-optimizer.exe` is not required alongside the GUI

#### CI
- GitHub Actions release workflow: automated cross-platform builds on tag push
  - Windows: CLI + GUI → `cbz-image-optimizer-vX.Y.Z-windows-x64.zip`
  - Linux: CLI only → `cbz-image-optimizer-vX.Y.Z-linux-x64.tar.gz`
  - macOS: CLI only → `cbz-image-optimizer-vX.Y.Z-macos-x64.tar.gz`
