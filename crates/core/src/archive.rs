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
        _ => "archive error",
    };
    anyhow::anyhow!(
        "RAR {phase} failure ({detail}): path={} code={code:?}",
        path.display()
    )
}
