# cbz-tools-optimizer

High-performance CBZ optimizer built in Rust — batch resize, compress, and convert images (JPEG/PNG/WebP/AVIF) for Kindle and e-readers, fully offline.  
CLI for Windows / Linux / macOS. Windows GUI included.

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

---

## Download

Download the latest release from [Releases](https://github.com/cbz-tools/cbz-tools-optimizer/releases).

| Archive | Contents |
|---|---|
| `cbz-tools-optimizer-vX.Y.Z-windows-x64.zip` | `cbz-opt.exe` (CLI) + `cbz-opt-gui.exe` (GUI) |
| `cbz-tools-optimizer-vX.Y.Z-linux-x64.tar.gz` | `cbz-opt` (CLI) |
| `cbz-tools-optimizer-vX.Y.Z-macos-x64.tar.gz` | `cbz-opt` (CLI) |

Extract the archive and run the binary directly — no installation required.

---

## Why cbz-tools-optimizer?

| | |
|---|---|
| 📦 **Storage savings** | Significantly reduce file size — real-world result: **9.0 GB → 647.6 MB (-93%)** in 1m 35s |
| 🔄 **Format conversion** | Convert JPEG / PNG / WebP / AVIF in bulk — resize and convert in a single pass |
| ⚡ **Speed** | Parallel processing across ZIPs and images via rayon |
| 🖥️ **Cross-platform** | Windows / Linux / macOS — single binary, no install |
| 🎯 **Device-ready presets** | iPad, Kindle, 4K and more — one flag to optimize for your device |
| 🤖 **Script-friendly** | Batch CLI with JSON output for automation and pipeline integration |
| 🖱️ **GUI included** | Windows drag-and-drop GUI — no CLI knowledge required |

---

## CLI Usage

### Resize

```bash
# Basic (default: ipad preset 2048×1536, JPEG quality 85)
cbz-opt input.cbz

# Multiple files
cbz-opt *.zip

# Kindle preset
cbz-opt --preset kindle --quality 80 --suffix _small *.cbz

# Custom size
cbz-opt --preset custom --max-width 1280 --max-height 720 input.zip

# Specify output directory
cbz-opt --output-dir ./output input.cbz
```

### Format Conversion

```bash
# Convert all images to WebP (no resize)
cbz-opt --output-format webp --convert-only input.cbz

# Convert to AVIF for maximum compression (no resize)
cbz-opt --output-format avif --convert-only *.cbz

# Resize AND convert to WebP in one pass
cbz-opt --preset ipad --output-format webp input.cbz

# Convert PNG to JPEG (no resize)
cbz-opt --output-format jpeg --convert-only input.cbz
```

> **`--convert-only`**: skips resizing entirely. Same-format files are passed through without re-encoding — zero quality loss.

### Options

| Option | Default | Description |
|---|---|---|
| `--preset` | `ipad` | Size preset (see table below) |
| `-W`, `--max-width` | — | Maximum width in pixels (`--preset custom` only) |
| `-H`, `--max-height` | — | Maximum height in pixels (`--preset custom` only) |
| `-q`, `--quality` | 85 | Lossy quality (1–100) — used by JPEG output and resized animated WebP |
| `-s`, `--suffix` | `_new` | Output filename suffix |
| `-o`, `--output-dir` | (same as input) | Output directory |
| `-t`, `--threads` | 0 (auto) | Number of threads (0 = half of logical CPUs) |
| `--output-format` | `jpeg` | Output image format: `jpeg` / `png` / `webp` / `avif` / `original` |
| `--convert-only` | — | Convert format only — skip resize entirely. `--preset` / `-W` / `-H` are ignored. Same-format files are passed through without re-encoding (zero degradation) |
| `--animated-webp-filter` | `bilinear` | Animated WebP resize interpolation: `bilinear` (fast/smooth), `catmull-rom` (sharper bicubic), or `lanczos3` (highest-detail comparison; slowest) |
| `--animated-webp-keyframes` | `bounded` | Animated WebP keyframe policy: `bounded` uses the interval below; `disabled` does not force periodic keyframes and ignores `kmin` / `kmax` |
| `--animated-webp-kmin` / `--animated-webp-kmax` | `3` / `5` | Minimum / maximum distance between animated-WebP key frames (`kmax >= 2`, `0 <= kmin < kmax`, `kmin >= kmax / 2 + 1`) |
| `--animated-webp-output-policy` | `always-use-encoded` | Animated WebP output size policy: write the high-quality resized result (default), or use `keep-original-if-larger` to retain an oversized source entry instead |
| `--log-mode` | `cli` | Log output: `cli` / `silent` / `both` / `file` |
| `--overwrite-mode` | `skip` | Output conflict resolution: `skip` / `overwrite` / `rename` |
| `--json` | — | Output progress as JSON lines (for scripting and automation) |

---

## Size Presets

| Preset | Width | Height | Intended device |
|---|---|---|---|
| `ipad` | 2048 | 1536 | iPad (default) |
| `ipad-air` | 2360 | 1640 | iPad Air |
| `ipad-pro` | 2732 | 2048 | iPad Pro |
| `kindle` | 1264 | 1680 | Kindle Paperwhite |
| `hd` | 1280 | 720 | HD display |
| `full-hd` | 1920 | 1080 | Full HD display |
| `four-k` | 3840 | 2160 | 4K display |
| `custom` | (manual) | (manual) | Use `-W` / `-H` |

---

## Supported Formats

| Format | Input | Output |
|---|---|---|
| JPEG | Yes | Yes |
| PNG | Yes | Yes |
| WebP (static) | Yes | Yes |
| WebP (animated) | Yes | Re-encoded as animated WebP |
| AVIF | Yes | Yes |
| BMP | Yes | Converted to output format |
| TIFF | Yes | Converted to output format |
| GIF | Skipped | — |

Animated WebP entries use a dedicated path: frame timing, loop count, and ANIM background color are preserved while frames may be resized. Entries already within the configured size bounds remain byte-identical; larger entries are resized with the selected interpolation and re-encoded as lossy `.webp` using the common `--quality` value (default 85, encoder method 4), independently of `--output-format` and `--convert-only`. Choose the resize filter with `--animated-webp-filter`; it is only used when a resize is actually required. `--animated-webp-keyframes bounded` (default) inserts independently decodable frames within the configured `kmin` / `kmax` interval. Choose `disabled` to avoid forced periodic keyframes; supplied `kmin` / `kmax` values are ignored. By default the resized result is written to honor the configured bounds; `--animated-webp-output-policy keep-original-if-larger` opts back into retaining an oversized source. GIF-containing archives are skipped.
BMP and TIFF inputs are converted to the format specified by `--output-format` (default: `jpeg`).  
AVIF is supported as both input and output (`--output-format avif`). AVIF decoding uses the bundled native `libdav1d` runtime on Windows.

---

## GUI Usage

1. Launch `cbz-opt-gui.exe`
2. Drag and drop ZIP/CBZ files or folders onto the window (or use **Add Files…** / **Add Folder…**)
3. Configure options via the **⚙** button and click **▶ Start**
4. A completion summary is shown next to the Start button when processing finishes

| Ready | Done |
|---|---|
| ![GUI file list](docs/screenshots/gui-filelist.png) | ![GUI done](docs/screenshots/gui-done.png) |

**Notes:**
- `cbz-opt.exe` is **not** required alongside the GUI — image processing is built in
- Supports English / 中文 / 日本語 (language selector in the menu bar)
- Settings are saved automatically to `cbz-opt-gui.toml` in the same folder

---

## Build from Source

**Prerequisites (Windows):** Requires the MSVC toolchain (`stable-x86_64-pc-windows-msvc`).  
Install **"Desktop development with C++"** workload from Visual Studio 2022 (or Build Tools for Visual Studio).  
The MSVC linker path is pre-configured in `.cargo/config.toml` — no Developer Command Prompt is required.

```bash
# All crates
cargo build --release

# CLI only  →  produces cbz-opt(.exe)
cargo build --release -p cbz-tools-optimizer-cli

# GUI only (Windows)  →  produces cbz-opt-gui.exe
cargo build --release -p cbz-tools-optimizer-gui
```

---

## Contributing

Bug reports and feature requests are welcome via [GitHub Issues](https://github.com/cbz-tools/cbz-tools-optimizer/issues).  
Please use the provided issue templates.

---

## How It Works

```
Multiple ZIP/CBZ files
  └── rayon::par_iter()   ← parallel across ZIPs
        └── each ZIP entry
              └── rayon::par_iter()   ← parallel across images
                    └── resize / convert with CatmullRom filter
```

- Images already within the pixel-dimension limit are not resized, but are still encoded into the selected output format in normal mode. To preserve their bytes, use `--convert-only` with the matching output format.
- Each ZIP is processed independently; one failure does not abort others
- Default thread count is **half of logical CPUs** to avoid saturating the system (override with `--threads N`)
- Output file conflict is controlled by `--overwrite-mode` (default: skip existing files)
- A log file (`cbz-opt_YYYYMMDD_HHMMSS.log`) is written when `--log-mode both` or `file` is specified
- On completion, total file size savings and elapsed time are reported

---

## Changelog

See [CHANGELOG.md](CHANGELOG.md).

---

## Third-Party Licenses

See [THIRDPARTY_LICENSES.md](THIRDPARTY_LICENSES.md).

---

## License

MIT — see [LICENSE](LICENSE).
