# Third-Party Licenses

This file documents the third-party native components distributed with release archives.
It is not an exhaustive notice for Rust crate dependencies; those are recorded in `Cargo.lock`
and, where needed, in a generated dependency-license report.

## UnRAR DLL

- Purpose: RAR/CBR archive input through the UnRAR backend
- Source managed in Git: `third_party/unrar/x64/UnRAR64.dll`
- Distribution: `UnRAR64.dll` is placed beside the executables in Windows release archives
- Loading: Checked and loaded lazily when a RAR/CBR input is opened
- Scope: RAR archive handling only; no RAR-compatible archive writing
- Release archives include the license text at `third_party/unrar/LICENSE.txt`

## dav1d DLL

- Purpose: Runtime dependency for AVIF decoding through `image/avif-native`
- License: BSD-2-Clause
- Source managed in Git: `third_party/dav1d/dav1d.dll`
- Distribution: `dav1d.dll` is placed beside the executables in Windows release archives
- Loading: Loaded from beside the executable through the standard Windows DLL search path
- Release archives include the license text at `third_party/dav1d/LICENSE`

## SVT-AV1

- Purpose: AVIF still-image encoding backend
- Version: v4.1.0
- License: BSD 3-Clause Clear
- Build: The native library obtained by `shiguredo_svt_av1` is statically linked
- Runtime DLL: Not required
- Release archives include the license text at `third_party/svt-av1/LICENSE`

## shiguredo_svt_av1

- Purpose: Rust bindings for SVT-AV1
- License: Apache-2.0
- Source: The `shiguredo_svt_av1` crate on crates.io
- Release archives include the license text at `third_party/shiguredo_svt_av1/LICENSE`
