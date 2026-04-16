use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rayon::prelude::*;
use zip::write::SimpleFileOptions;

use crate::{OptimizeConfig, OverwriteMode, ProgressEvent};
use crate::resize::{is_animated_webp, is_image, resize_image_bytes};

/// Outcome of processing a single ZIP
enum ZipOutcome {
    Done { input_bytes: u64, output_bytes: u64 },
    Skipped,
    Failed,
}

/// An entry read from a ZIP archive
enum ZipEntry {
    Directory(String),
    File(String, Vec<u8>),
}

/// Entry point for parallel processing of multiple ZIP files.
///
/// `on_progress` must be `Send + Sync` as it is called across threads.
/// Returns (succeeded, skipped, failed).
pub fn process_zips<F>(
    zip_paths: &[PathBuf],
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

    let outcomes: Vec<ZipOutcome> = pool.install(|| {
        zip_paths
            .par_iter()
            .map(|path| {
                let cb = Arc::clone(&on_progress);
                let cfg = Arc::clone(&config);

                // Catch panics to prevent them from propagating
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    process_one_zip(path, &cfg, &*cb)
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
                        ZipOutcome::Done { input_bytes, output_bytes }
                    }
                    Ok(Ok(None)) => {
                        // ZipSkipped already emitted inside process_one_zip
                        ZipOutcome::Skipped
                    }
                    Ok(Err(e)) => {
                        cb(ProgressEvent::ZipError {
                            path: path.display().to_string(),
                            message: e.to_string(),
                        });
                        ZipOutcome::Failed
                    }
                    Err(_panic) => {
                        cb(ProgressEvent::ZipError {
                            path: path.display().to_string(),
                            message: "Unexpected error occurred".to_string(),
                        });
                        ZipOutcome::Failed
                    }
                }
            })
            .collect()
    });

    let succeeded = outcomes.iter().filter(|o| matches!(o, ZipOutcome::Done { .. })).count();
    let skipped   = outcomes.iter().filter(|o| matches!(o, ZipOutcome::Skipped)).count();
    let failed    = outcomes.iter().filter(|o| matches!(o, ZipOutcome::Failed)).count();
    let total_input_bytes: u64 = outcomes.iter().map(|o| match o {
        ZipOutcome::Done { input_bytes, .. } => *input_bytes,
        _ => 0,
    }).sum();
    let total_output_bytes: u64 = outcomes.iter().map(|o| match o {
        ZipOutcome::Done { output_bytes, .. } => *output_bytes,
        _ => 0,
    }).sum();

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

/// Process a single ZIP. Returns Ok(Some((path, input_bytes))) on success, Ok(None) if skipped, Err on failure.
fn process_one_zip<F>(
    zip_path: &Path,
    config: &OptimizeConfig,
    on_progress: &F,
) -> Result<Option<(PathBuf, u64)>>
where
    F: Fn(ProgressEvent) + Send + Sync,
{
    // --- Read ---
    let zip_data = std::fs::read(zip_path)
        .with_context(|| format!("Failed to read: {}", zip_path.display()))?;
    let input_bytes = zip_data.len() as u64;

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(&zip_data))
        .with_context(|| format!("Failed to open ZIP: {}", zip_path.display()))?;

    // Collect entries, distinguishing directories from files
    let entries: Vec<ZipEntry> = (0..archive.len())
        .map(|i| {
            let mut entry = archive.by_index(i)?;
            let name = entry.name().to_string();
            if entry.is_dir() {
                Ok(ZipEntry::Directory(name))
            } else {
                let mut data = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut data)?;
                Ok(ZipEntry::File(name, data))
            }
        })
        .collect::<Result<_, zip::result::ZipError>>()
        .context("Failed to read ZIP entries")?;

    // Check for animated WebP or GIF before processing (file entries only)
    let has_unsupported_anim = entries.iter().any(|e| match e {
        ZipEntry::File(name, data) => {
            let lower = name.to_lowercase();
            lower.ends_with(".gif")
                || (lower.ends_with(".webp") && is_animated_webp(data))
        }
        ZipEntry::Directory(_) => false,
    });

    if has_unsupported_anim {
        on_progress(ProgressEvent::ZipSkipped {
            path: zip_path.display().to_string(),
            reason: "Skipped: contains animated WebP or GIF".to_string(),
        });
        return Ok(None);
    }

    let image_count = entries.iter().filter(|e| matches!(e, ZipEntry::File(name, _) if is_image(name))).count();
    on_progress(ProgressEvent::ZipStarted {
        path: zip_path.display().to_string(),
        image_count,
    });

    // --- Parallel resize ---
    let zip_path_str = zip_path.display().to_string();
    let total = entries.len();

    // Process entries in parallel; directories are passed through unchanged
    let processed: Vec<ZipEntry> = entries
        .into_par_iter()
        .enumerate()
        .map(|(idx, entry)| match entry {
            ZipEntry::Directory(name) => ZipEntry::Directory(name),
            ZipEntry::File(name, data) => {
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
                    zip_path: zip_path_str.clone(),
                    image_index: idx + 1,
                    total,
                });

                ZipEntry::File(out_name, out_data)
            }
        })
        .collect();

    // --- Write ZIP ---
    let Some(output_path) = resolve_output_path(zip_path, config)? else {
        on_progress(ProgressEvent::ZipSkipped {
            path: zip_path.display().to_string(),
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
            ZipEntry::Directory(name) => {
                writer.add_directory(&name, SimpleFileOptions::default())?;
            }
            ZipEntry::File(name, data) => {
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
    let ext = input.extension().unwrap_or_default().to_string_lossy();
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
            let base_dir: &Path = config.output_dir.as_deref()
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
