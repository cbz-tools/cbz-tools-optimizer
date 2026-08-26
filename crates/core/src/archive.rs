//! Archive input abstraction shared by ZIP/CBZ and RAR/CBR processing.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    Rar,
}

/// One entry read from an archive. It is held only while that entry is in the
/// bounded processing pipeline; the archive itself is never collected.
pub(crate) struct ArchiveEntry {
    pub(crate) name: String,
    pub(crate) data: Vec<u8>,
    pub(crate) is_directory: bool,
    pub(crate) last_modified: Option<zip::DateTime>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ArchiveMetadata {
    pub(crate) image_count: usize,
    pub(crate) entry_count: usize,
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

/// Read lightweight metadata needed by progress events. ZIP names come from
/// the central directory. RAR uses UnRAR's listing mode, which advances by
/// discarding payloads without returning entry buffers; the real reader stays
/// a separate sequential processing pass.
pub(crate) fn archive_metadata(path: &Path) -> Result<ArchiveMetadata> {
    match archive_kind(path) {
        Some(ArchiveKind::Zip) => read_zip_metadata(path),
        Some(ArchiveKind::Rar) => read_rar_metadata(path),
        None => anyhow::bail!("Unsupported archive extension: {}", path.display()),
    }
}

pub(crate) enum ArchiveReader {
    Zip {
        archive: zip::ZipArchive<File>,
        next_index: usize,
    },
    #[cfg(feature = "rar")]
    Rar {
        archive: Option<unrar::OpenArchive<unrar::Process, unrar::CursorBeforeHeader>>,
        path: std::path::PathBuf,
    },
}

impl ArchiveReader {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        match archive_kind(path) {
            Some(ArchiveKind::Zip) => {
                let file = File::open(path)
                    .with_context(|| format!("Failed to open archive: {}", path.display()))?;
                let archive = zip::ZipArchive::new(file)
                    .with_context(|| format!("Failed to open ZIP: {}", path.display()))?;
                Ok(Self::Zip {
                    archive,
                    next_index: 0,
                })
            }
            Some(ArchiveKind::Rar) => open_rar_reader(path),
            None => anyhow::bail!("Unsupported archive extension: {}", path.display()),
        }
    }

    pub(crate) fn next_entry(&mut self) -> Result<Option<ArchiveEntry>> {
        match self {
            Self::Zip {
                archive,
                next_index,
            } => {
                if *next_index >= archive.len() {
                    return Ok(None);
                }
                let index = *next_index;
                *next_index += 1;
                let mut entry = archive.by_index(index)?;
                let name = entry.name().to_string();
                let last_modified = entry.last_modified();
                let is_directory = entry.is_dir();
                let mut data = Vec::new();
                if !is_directory {
                    entry.read_to_end(&mut data)?;
                }
                Ok(Some(ArchiveEntry {
                    name,
                    data,
                    is_directory,
                    last_modified,
                }))
            }
            #[cfg(feature = "rar")]
            Self::Rar { archive, path } => {
                let current = archive
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("RAR reader was already consumed"))?;
                let header = current
                    .read_header()
                    .map_err(|error| format_rar_error(path, "read_header", error.code))?;
                let Some(header) = header else {
                    return Ok(None);
                };

                let name = header.entry().filename.to_string_lossy().into_owned();
                let last_modified = rar_last_modified(header.entry().file_time);
                if header.entry().is_directory() {
                    *archive = Some(
                        header
                            .skip()
                            .map_err(|error| format_rar_error(path, "skip", error.code))?,
                    );
                    Ok(Some(ArchiveEntry {
                        name,
                        data: Vec::new(),
                        is_directory: true,
                        last_modified,
                    }))
                } else {
                    let (data, next) = header
                        .read()
                        .map_err(|error| format_rar_error(path, "read_data", error.code))?;
                    *archive = Some(next);
                    Ok(Some(ArchiveEntry {
                        name,
                        data,
                        is_directory: false,
                        last_modified,
                    }))
                }
            }
        }
    }
}

fn read_zip_metadata(path: &Path) -> Result<ArchiveMetadata> {
    let file =
        File::open(path).with_context(|| format!("Failed to open archive: {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("Failed to open ZIP: {}", path.display()))?;
    let mut image_count = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if !entry.is_dir() && crate::resize::is_image(entry.name()) {
            image_count += 1;
        }
    }
    Ok(ArchiveMetadata {
        image_count,
        entry_count: archive.len(),
    })
}

#[cfg(feature = "rar")]
fn read_rar_metadata(path: &Path) -> Result<ArchiveMetadata> {
    use unrar::Archive;

    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("RAR path is not valid UTF-8: {}", path.display()))?;
    let archive = Archive::new(path_str)
        .open_for_listing()
        .map_err(|error| format_rar_error(path, "metadata", error.code))?;
    let mut image_count = 0;
    let mut entry_count = 0;
    for header in archive {
        let header = header.map_err(|error| format_rar_error(path, "metadata", error.code))?;
        entry_count += 1;
        if !header.is_directory()
            && crate::resize::is_image(header.filename.to_string_lossy().as_ref())
        {
            image_count += 1;
        }
    }
    Ok(ArchiveMetadata {
        image_count,
        entry_count,
    })
}

#[cfg(not(feature = "rar"))]
fn read_rar_metadata(_path: &Path) -> Result<ArchiveMetadata> {
    Ok(ArchiveMetadata {
        // The RAR feature is disabled, so the actual reader will report the
        // feature error after this placeholder metadata path.
        image_count: 0,
        entry_count: 0,
    })
}

#[cfg(feature = "rar")]
fn open_rar_reader(path: &Path) -> Result<ArchiveReader> {
    use unrar::Archive;

    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("RAR path is not valid UTF-8: {}", path.display()))?;
    let archive = Archive::new(path_str)
        .open_for_processing()
        .map_err(|error| format_rar_error(path, "open", error.code))?;
    Ok(ArchiveReader::Rar {
        archive: Some(archive),
        path: path.to_path_buf(),
    })
}

#[cfg(not(feature = "rar"))]
fn open_rar_reader(path: &Path) -> Result<ArchiveReader> {
    anyhow::bail!(
        "RAR support is disabled; rebuild with the 'rar' feature: {}",
        path.display()
    )
}

#[cfg(feature = "rar")]
fn rar_last_modified(file_time: u32) -> Option<zip::DateTime> {
    zip::DateTime::try_from(((file_time >> 16) as u16, file_time as u16)).ok()
}

#[cfg(feature = "rar")]
fn format_rar_error(path: &Path, phase: &str, code: unrar::error::Code) -> anyhow::Error {
    format_rar_error_from_path(phase, code).context(format!("path={}", path.display()))
}

#[cfg(feature = "rar")]
fn format_rar_error_from_path(phase: &str, code: unrar::error::Code) -> anyhow::Error {
    use unrar::error::Code;

    let detail = match code {
        Code::MissingPassword | Code::BadPassword => "password required or invalid",
        _ => "archive error",
    };
    anyhow::anyhow!("RAR {phase} failure ({detail}): code={code:?}")
}
