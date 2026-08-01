//! Archive input abstraction shared by ZIP/CBZ and RAR/CBR processing.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Rar,
}

/// An archive entry collected before image processing.
pub(crate) enum ArchiveEntry {
    Directory(String),
    File(String, Vec<u8>),
}

/// Classify supported archive paths by extension.
pub fn archive_kind(path: &Path) -> Option<ArchiveKind> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "zip" | "cbz" => Some(ArchiveKind::Zip),
        "rar" | "cbr" => Some(ArchiveKind::Rar),
        _ => None,
    }
}

pub fn is_supported_archive_path(path: &Path) -> bool {
    archive_kind(path).is_some()
}

pub(crate) fn read_archive_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
    match archive_kind(path) {
        Some(ArchiveKind::Zip) => read_zip_entries(path),
        Some(ArchiveKind::Rar) => read_rar_entries(path),
        None => anyhow::bail!("Unsupported archive extension: {}", path.display()),
    }
}

fn read_zip_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
    let archive_data = std::fs::read(path)
        .with_context(|| format!("Failed to read archive: {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&archive_data))
        .with_context(|| format!("Failed to open ZIP: {}", path.display()))?;

    (0..archive.len())
        .map(|index| {
            let mut entry = archive.by_index(index)?;
            let name = entry.name().to_string();
            if entry.is_dir() {
                Ok(ArchiveEntry::Directory(name))
            } else {
                let mut data = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut data)?;
                Ok(ArchiveEntry::File(name, data))
            }
        })
        .collect::<std::result::Result<Vec<_>, zip::result::ZipError>>()
        .context("Failed to read ZIP entries")
}

#[cfg(feature = "rar")]
fn read_rar_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
    use unrar::Archive;

    ensure_unrar_dll_for_current_target()?;
    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("RAR path is not valid UTF-8: {}", path.display()))?;
    let mut archive = Archive::new(path_str)
        .open_for_processing()
        .map_err(|error| format_rar_error(path, "open", error.code))?;
    let mut entries = Vec::new();

    loop {
        let header = archive
            .read_header()
            .map_err(|error| format_rar_error(path, "read_header", error.code))?;
        let Some(header) = header else {
            break;
        };

        let name = header.entry().filename.to_string_lossy().into_owned();
        if header.entry().is_directory() {
            entries.push(ArchiveEntry::Directory(name));
            archive = header
                .skip()
                .map_err(|error| format_rar_error(path, "skip", error.code))?;
        } else {
            let (data, next) = header
                .read()
                .map_err(|error| format_rar_error(path, "read_data", error.code))?;
            entries.push(ArchiveEntry::File(name, data));
            archive = next;
        }
    }

    Ok(entries)
}

#[cfg(not(feature = "rar"))]
fn read_rar_entries(path: &Path) -> Result<Vec<ArchiveEntry>> {
    anyhow::bail!(
        "RAR support is disabled; rebuild with the 'rar' feature: {}",
        path.display()
    )
}

#[cfg(feature = "rar")]
fn format_rar_error(path: &Path, phase: &str, code: unrar::error::Code) -> anyhow::Error {
    use unrar::error::Code;

    let detail = match code {
        Code::MissingPassword | Code::BadPassword => "password required or invalid",
        Code::EOpen => "possible DLL missing/load failure",
        _ => "archive error",
    };
    anyhow::anyhow!(
        "RAR {phase} failure ({detail}): path={} code={code:?}",
        path.display()
    )
}

#[cfg(all(feature = "rar", windows))]
fn expected_unrar_dll_name() -> &'static str {
    #[cfg(target_pointer_width = "64")]
    {
        "UnRAR64.dll"
    }
    #[cfg(target_pointer_width = "32")]
    {
        "UnRAR.dll"
    }
}

#[cfg(all(feature = "rar", windows))]
fn ensure_unrar_dll_for_current_target() -> Result<()> {
    let dll_name = expected_unrar_dll_name();
    let current_exe = std::env::current_exe().ok();
    let exe_dir_dll_path = current_exe
        .as_ref()
        .and_then(|path| path.parent().map(|dir| dir.join(dll_name)));
    let cwd_dll_path = Path::new(dll_name).to_path_buf();
    let resolved_dll_path = exe_dir_dll_path
        .as_ref()
        .filter(|path| path.exists())
        .cloned()
        .or_else(|| cwd_dll_path.exists().then_some(cwd_dll_path.clone()));

    if resolved_dll_path.is_some() {
        Ok(())
    } else {
        anyhow::bail!(
            "RAR support is unavailable: expected '{}' next to the executable (fallback: '{}')",
            exe_dir_dll_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| format!("<unknown-exe-dir>\\{dll_name}")),
            cwd_dll_path.display()
        )
    }
}

#[cfg(all(feature = "rar", not(windows)))]
fn ensure_unrar_dll_for_current_target() -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{archive_kind, is_supported_archive_path, ArchiveKind};
    use std::path::Path;

    #[test]
    fn archive_kind_accepts_zip_and_rar_aliases_case_insensitively() {
        assert_eq!(archive_kind(Path::new("book.zip")), Some(ArchiveKind::Zip));
        assert_eq!(archive_kind(Path::new("book.CBZ")), Some(ArchiveKind::Zip));
        assert_eq!(archive_kind(Path::new("book.rar")), Some(ArchiveKind::Rar));
        assert_eq!(archive_kind(Path::new("book.CbR")), Some(ArchiveKind::Rar));
        assert_eq!(archive_kind(Path::new("book.7z")), None);
    }

    #[test]
    fn supported_archive_path_requires_an_archive_extension() {
        assert!(is_supported_archive_path(Path::new("book.cbr")));
        assert!(!is_supported_archive_path(Path::new("book.jpg")));
        assert!(!is_supported_archive_path(Path::new("book")));
    }
}
