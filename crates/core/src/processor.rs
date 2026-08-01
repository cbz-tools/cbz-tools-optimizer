use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rayon::prelude::*;
use zip::write::SimpleFileOptions;

use crate::archive::{archive_kind, read_archive_entries, ArchiveEntry, ArchiveKind};
use crate::resize::{is_image, resize_image_bytes};
use crate::{OptimizeConfig, OverwriteMode, ProgressEvent};

/// Outcome of processing a single archive.
enum ArchiveOutcome {
    Done { input_bytes: u64, output_bytes: u64 },
    Skipped,
    Failed,
}

/// Entry point for parallel processing of multiple ZIP/CBZ/RAR/CBR files.
///
/// `on_progress` must be `Send + Sync` as it is called across threads.
/// Returns (succeeded, skipped, failed).
pub fn process_archives<F>(
    archive_paths: &[PathBuf],
    config: &OptimizeConfig,
    on_progress: F,
) -> (usize, usize, usize)
where
    F: Fn(ProgressEvent) + Send + Sync,
{
    // Thread pool (0 = auto = logical CPU count)
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(if config.threads == 0 {
            // Default: half of logical CPUs to avoid saturating the system
            (num_cpus() / 2).max(1)
        } else {
            config.threads
        })
        .build()
        .expect("rayon pool");

    let on_progress = Arc::new(on_progress);
    let config = Arc::new(config.clone());

    let outcomes: Vec<ArchiveOutcome> = pool.install(|| {
        archive_paths
            .par_iter()
            .map(|path| {
                let cb = Arc::clone(&on_progress);
                let cfg = Arc::clone(&config);

                // Catch panics to prevent them from propagating
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_one_archive(path, &cfg, &*cb)
                }));

                match result {
                    Ok(Ok(Some((out, input_bytes)))) => {
                        let output_bytes = out.metadata().map(|m| m.len()).unwrap_or(0);
                        cb(ProgressEvent::ZipDone {
                            path: path.display().to_string(),
                            output_path: out.display().to_string(),
                            input_bytes,
                            output_bytes,
                        });
                        ArchiveOutcome::Done {
                            input_bytes,
                            output_bytes,
                        }
                    }
                    Ok(Ok(None)) => {
                        // ZipSkipped already emitted inside process_one_archive
                        ArchiveOutcome::Skipped
                    }
                    Ok(Err(e)) => {
                        cb(ProgressEvent::ZipError {
                            path: path.display().to_string(),
                            message: e.to_string(),
                        });
                        ArchiveOutcome::Failed
                    }
                    Err(_panic) => {
                        cb(ProgressEvent::ZipError {
                            path: path.display().to_string(),
                            message: "Unexpected error occurred".to_string(),
                        });
                        ArchiveOutcome::Failed
                    }
                }
            })
            .collect()
    });

    let succeeded = outcomes
        .iter()
        .filter(|o| matches!(o, ArchiveOutcome::Done { .. }))
        .count();
    let skipped = outcomes
        .iter()
        .filter(|o| matches!(o, ArchiveOutcome::Skipped))
        .count();
    let failed = outcomes
        .iter()
        .filter(|o| matches!(o, ArchiveOutcome::Failed))
        .count();
    let total_input_bytes: u64 = outcomes
        .iter()
        .map(|o| match o {
            ArchiveOutcome::Done { input_bytes, .. } => *input_bytes,
            _ => 0,
        })
        .sum();
    let total_output_bytes: u64 = outcomes
        .iter()
        .map(|o| match o {
            ArchiveOutcome::Done { output_bytes, .. } => *output_bytes,
            _ => 0,
        })
        .sum();

    on_progress(ProgressEvent::AllDone {
        total_zips: outcomes.len(),
        succeeded,
        skipped,
        failed,
        total_input_bytes,
        total_output_bytes,
    });

    (succeeded, skipped, failed)
}

/// Backward-compatible ZIP-named entry point for existing library callers.
pub fn process_zips<F>(
    archive_paths: &[PathBuf],
    config: &OptimizeConfig,
    on_progress: F,
) -> (usize, usize, usize)
where
    F: Fn(ProgressEvent) + Send + Sync,
{
    process_archives(archive_paths, config, on_progress)
}

/// Process a single archive. Returns Ok(Some((path, input_bytes))) on success,
/// Ok(None) if skipped, Err on failure.
fn process_one_archive<F>(
    archive_path: &Path,
    config: &OptimizeConfig,
    on_progress: &F,
) -> Result<Option<(PathBuf, u64)>>
where
    F: Fn(ProgressEvent) + Send + Sync,
{
    // --- Read ---
    let input_bytes = std::fs::metadata(archive_path)
        .with_context(|| format!("Failed to stat: {}", archive_path.display()))?
        .len();
    let entries = read_archive_entries(archive_path)?;

    // GIF animation remains unsupported. Animated WebP is handled per entry by
    // the dedicated optimization path, so it must not skip the whole archive.
    let has_unsupported_animation = entries.iter().any(|e| match e {
        ArchiveEntry::File(name, _) => name.to_lowercase().ends_with(".gif"),
        ArchiveEntry::Directory(_) => false,
    });

    if has_unsupported_animation {
        on_progress(ProgressEvent::ZipSkipped {
            path: archive_path.display().to_string(),
            reason: "Skipped: contains GIF (animation is not supported)".to_string(),
        });
        return Ok(None);
    }

    let image_count = entries
        .iter()
        .filter(|e| matches!(e, ArchiveEntry::File(name, _) if is_image(name)))
        .count();
    on_progress(ProgressEvent::ZipStarted {
        path: archive_path.display().to_string(),
        image_count,
    });

    // --- Parallel resize ---
    let archive_path_str = archive_path.display().to_string();
    let total = entries.len();

    // Process entries in parallel; directories are passed through unchanged
    let processed: Vec<ArchiveEntry> = entries
        .into_par_iter()
        .enumerate()
        .map(|(idx, entry)| match entry {
            ArchiveEntry::Directory(name) => ArchiveEntry::Directory(name),
            ArchiveEntry::File(name, data) => {
                let (out_data, out_name) = if is_image(&name) {
                    match resize_image_bytes(&data, &name, config) {
                        Ok((resized, ext)) => (resized, replace_extension(&name, ext)),
                        Err(e) => {
                            log::warn!("Resize failed for {name}: {e}");
                            (data, name.clone())
                        }
                    }
                } else {
                    (data, name.clone())
                };

                on_progress(ProgressEvent::ImageDone {
                    zip_path: archive_path_str.clone(),
                    image_index: idx + 1,
                    total,
                });

                ArchiveEntry::File(out_name, out_data)
            }
        })
        .collect();

    // --- Write ZIP ---
    let Some(output_path) = resolve_output_path(archive_path, config)? else {
        on_progress(ProgressEvent::ZipSkipped {
            path: archive_path.display().to_string(),
            reason: "Output file already exists (skip mode)".to_string(),
        });
        return Ok(None);
    };
    let out_file = std::fs::File::create(&output_path)
        .with_context(|| format!("Failed to create output file: {}", output_path.display()))?;

    let mut writer = zip::ZipWriter::new(out_file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(6));

    for entry in processed {
        match entry {
            ArchiveEntry::Directory(name) => {
                writer.add_directory(&name, SimpleFileOptions::default())?;
            }
            ArchiveEntry::File(name, data) => {
                writer.start_file(&name, options)?;
                writer.write_all(&data)?;
            }
        }
    }
    writer.finish()?;

    Ok(Some((output_path, input_bytes)))
}

/// Resolve output file path according to overwrite_mode.
/// Returns Ok(None) if the file should be skipped (Skip mode and file exists).
fn resolve_output_path(input: &Path, config: &OptimizeConfig) -> Result<Option<PathBuf>> {
    let stem = input.file_stem().unwrap_or_default().to_string_lossy();
    let ext = match archive_kind(input) {
        Some(ArchiveKind::Rar) => "cbz".to_owned(),
        _ => input
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    };
    let filename = format!("{}{}.{}", stem, config.output_suffix, ext);
    let base_path = match &config.output_dir {
        Some(dir) => dir.join(&filename),
        None => input.parent().unwrap_or(Path::new(".")).join(&filename),
    };

    match config.overwrite_mode {
        OverwriteMode::Skip => {
            if base_path.exists() {
                return Ok(None);
            }
            Ok(Some(base_path))
        }
        OverwriteMode::Overwrite => Ok(Some(base_path)),
        OverwriteMode::Rename => {
            if !base_path.exists() {
                return Ok(Some(base_path));
            }
            let base_dir: &Path = config
                .output_dir
                .as_deref()
                .unwrap_or_else(|| input.parent().unwrap_or(Path::new(".")));
            for n in 1..=9999 {
                let renamed = format!("{}{}({}).{}", stem, config.output_suffix, n, ext);
                let candidate = base_dir.join(&renamed);
                if !candidate.exists() {
                    return Ok(Some(candidate));
                }
            }
            anyhow::bail!("Could not find available filename after 9999 attempts")
        }
    }
}

/// Replace the extension of an entry name, preserving any directory prefix
fn replace_extension(name: &str, new_ext: &str) -> String {
    let path = std::path::Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    match path.parent() {
        Some(parent) if parent != std::path::Path::new("") => {
            format!("{}/{}{}", parent.display(), stem, new_ext)
        }
        _ => format!("{}{}", stem, new_ext),
    }
}

/// Logical CPU count (without rayon dependency)
fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::resolve_output_path;
    use crate::{OptimizeConfig, OverwriteMode};
    use std::path::{Path, PathBuf};

    #[test]
    fn rar_and_cbr_outputs_use_cbz_extension() {
        let mut config = OptimizeConfig::default();
        config.output_dir = Some(PathBuf::from("test-output"));
        config.overwrite_mode = OverwriteMode::Overwrite;

        assert_eq!(
            resolve_output_path(Path::new("book.rar"), &config)
                .unwrap()
                .unwrap(),
            PathBuf::from("test-output/book_new.cbz")
        );
        assert_eq!(
            resolve_output_path(Path::new("book.CBR"), &config)
                .unwrap()
                .unwrap(),
            PathBuf::from("test-output/book_new.cbz")
        );
    }

    #[test]
    fn zip_and_cbz_outputs_preserve_their_existing_extensions() {
        let mut config = OptimizeConfig::default();
        config.output_dir = Some(PathBuf::from("test-output"));
        config.overwrite_mode = OverwriteMode::Overwrite;

        assert_eq!(
            resolve_output_path(Path::new("book.zip"), &config)
                .unwrap()
                .unwrap(),
            PathBuf::from("test-output/book_new.zip")
        );
        assert_eq!(
            resolve_output_path(Path::new("book.cbz"), &config)
                .unwrap()
                .unwrap(),
            PathBuf::from("test-output/book_new.cbz")
        );
    }
}
