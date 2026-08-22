# Third-Party Licenses

This file documents the third-party native components distributed with release archives.
It is not an exhaustive notice for Rust crate dependencies; those are recorded in `Cargo.lock`
and, where needed, in a generated dependency-license report.

## UnRAR

- Purpose: RAR/CBR archive input through the UnRAR backend
- Role: `unrar_sys` compiles the bundled UnRAR source into the application through static linking
- Scope: RAR archive handling only; no RAR-compatible archive writing
- Distribution: No external UnRAR DLL is distributed or loaded
- License: UnRAR is used under the following notice from `unrar_sys/vendor/unrar/license.txt`:

```text
 ******    *****   ******   UnRAR - free utility for RAR archives
 **   **  **   **  **   **  ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
 ******   *******  ******    License for use and distribution of
 **   **  **   **  **   **   FREE portable version
 **   **  **   **  **   **   ~~~~~~~~~~~~~~~~~~~~~

      The source code of UnRAR utility is freeware. This means:

   1. All copyrights to RAR and the utility UnRAR are exclusively
      owned by the author - Alexander Roshal.

   2. UnRAR source code may be used in any software to handle
      RAR archives without limitations free of charge, but cannot be
      used to develop RAR (WinRAR) compatible archiver and to
      re-create RAR compression algorithm, which is proprietary.
      Distribution of modified UnRAR source code in separate form
      or as a part of other software is permitted, provided that
      full text of this paragraph, starting from "UnRAR source code"
      words, is included in license, or in documentation if license
      is not available, and in source code comments of resulting package.

   3. The UnRAR utility may be freely distributed. It is allowed
      to distribute UnRAR inside of other software packages.

   4. THE RAR ARCHIVER AND THE UnRAR UTILITY ARE DISTRIBUTED "AS IS".
      NO WARRANTY OF ANY KIND IS EXPRESSED OR IMPLIED. YOU USE AT
      YOUR OWN RISK. THE AUTHOR WILL NOT BE LIABLE FOR DATA LOSS,
      DAMAGES, LOSS OF PROFITS OR ANY OTHER KIND OF LOSS WHILE USING
      OR MISUSING THIS SOFTWARE.

   5. Installing and using the UnRAR utility signifies acceptance of
      these terms and conditions of the license.

   6. If you don't agree with terms of the license you must remove
      UnRAR files from your storage devices and cease to use the
      utility.

      Thank you for your interest in RAR and UnRAR.


                                            Alexander L. Roshal
```

## dav1d DLL

- Purpose: Runtime dependency for AVIF decoding through `image/avif-native`
- License: BSD-2-Clause
- Source managed in Git: `third_party/dav1d/dav1d.dll`
- Distribution: Each Windows launcher embeds `dav1d.dll`; it is extracted beside its core into the versioned local runtime directory
- Loading: Loaded from beside the extracted core through the standard Windows DLL search path
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

## Static image acceleration crates

- `fast_image_resize` — MIT OR Apache-2.0; Rust SIMD resize implementation
- `turbojpeg` / `turbojpeg-sys` — Unlicense OR MIT Rust bindings; libjpeg-turbo is statically built with CMake
- `webp` / `libwebp-sys` — MIT OR Apache-2.0 Rust wrapper and BSD 3-Clause libwebp native library, statically linked
- These components do not require additional runtime DLLs in the release package. Their exact versions and transitive notices are recorded in `Cargo.lock` and the generated dependency-license report.
