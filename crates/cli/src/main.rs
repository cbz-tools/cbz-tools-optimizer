use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use clap::Parser;
use cbz_tools_optimizer_core::{
    LogMode, OptimizeConfig, OutputFormat, OverwriteMode, ProgressEvent, SizePreset,
    format_elapsed, format_size,
    processor::process_zips,
};

#[derive(Parser, Debug)]
#[command(
    name = "cbz-opt",
    version,
    about = "Resize images inside ZIP/CBZ files — blazing fast with parallel processing",
    long_about = None,
)]
struct Args {
    /// Input ZIP/CBZ files (multiple files supported)
    #[arg(required = true, value_name = "FILE")]
    files: Vec<PathBuf>,

    /// Maximum width after resizing [px] (only used when --preset custom)
    #[arg(short = 'W', long, default_value_t = 1920, hide_default_value = true)]
    max_width: u32,

    /// Maximum height after resizing [px] (only used when --preset custom)
    #[arg(short = 'H', long, default_value_t = 1080, hide_default_value = true)]
    max_height: u32,

    /// JPEG quality (1-100) — used when --output-format is jpeg, or when --output-format
    /// is original and --convert-only is not set (JPEG inputs may be re-encoded)
    #[arg(short, long, default_value_t = 85)]
    quality: u8,

    /// Output directory (default: same as input)
    #[arg(short, long, value_name = "DIR")]
    output_dir: Option<PathBuf>,

    /// Output filename suffix
    #[arg(short, long, default_value = "_new")]
    suffix: String,

    /// Number of threads (0 = auto: half of logical CPUs)
    #[arg(short, long, default_value_t = 0)]
    threads: usize,

    /// Size preset (overrides --max-width / --max-height).
    /// Use 'custom' to specify exact dimensions with -W / -H.
    ///   full-hd : 1920x1080 / hd      : 1280x720
    ///   four-k  : 3840x2160 / ipad-pro: 2732x2048
    ///   ipad-air: 2360x1640 / ipad    : 2048x1536 (default)
    ///   kindle  : 1264x1680 / custom  : use -W / -H
    #[arg(long, value_enum, default_value = "ipad", verbatim_doc_comment)]
    preset: SizePreset,

    /// Output image format:
    ///   jpeg    : convert all images to JPEG (default)
    ///   png     : convert all images to PNG
    ///   webp    : convert all images to lossless WebP
    ///   avif    : convert all images to AVIF
    ///   original: keep original format
    #[arg(long, value_enum, default_value = "jpeg", verbatim_doc_comment)]
    output_format: OutputFormat,

    /// Convert format only — skip resize entirely.
    /// --preset / --max-width / --max-height are ignored when this flag is set.
    /// If input and output formats match, bytes are passed through without re-encoding
    /// (zero degradation). Combine with --output-format to change format without resizing.
    #[arg(long)]
    convert_only: bool,

    /// Log output mode:
    ///   cli    : console output only (default)
    ///   silent : no output
    ///   both   : console + log file (cbz-opt_YYYYMMDD_HHMMSS.log)
    ///   file   : log file only (cbz-opt_YYYYMMDD_HHMMSS.log)
    #[arg(long, value_enum, default_value = "cli", verbatim_doc_comment)]
    log_mode: LogMode,

    /// Output file conflict resolution:
    ///   skip      : skip processing if output already exists (default)
    ///   overwrite : overwrite existing output file
    ///   rename    : auto-rename output (e.g. file_new(1).zip, file_new(2).zip)
    #[arg(long, value_enum, default_value = "skip", verbatim_doc_comment)]
    overwrite_mode: OverwriteMode,

    /// Output progress as JSON lines (for scripting and automation)
    #[arg(long)]
    json: bool,
}

/// Log entry collected during processing
struct LogEntry {
    status: &'static str, // "SUCCESS" / "SKIPPED" / "ERROR"
    input: String,
    detail: String,       // output path, skip reason, or error message
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    let args = Args::parse();
    let json_mode = args.json;

    // Warn and confirm before overwriting originals when suffix is empty and overwrite mode is on
    if args.suffix.is_empty() && args.overwrite_mode == OverwriteMode::Overwrite && !args.json {
        eprintln!("Warning: suffix is empty and overwrite mode is on.");
        eprintln!("Original files will be OVERWRITTEN.");
        eprint!("Continue? [y/N]: ");
        std::io::Write::flush(&mut std::io::stderr()).ok();

        let mut input = String::new();
        std::io::stdin().read_line(&mut input).ok();

        if !matches!(input.trim().to_lowercase().as_str(), "y" | "yes") {
            eprintln!("Aborted.");
            std::process::exit(0);
        }
    }

    let config = OptimizeConfig {
        preset: args.preset,
        max_width: args.max_width,
        max_height: args.max_height,
        jpeg_quality: args.quality,
        output_dir: args.output_dir,
        output_suffix: args.suffix,
        threads: args.threads,
        output_format: args.output_format,
        convert_only: args.convert_only,
        log_mode: args.log_mode,
        overwrite_mode: args.overwrite_mode,
    };

    let total = args.files.len();

    let print_cli = !json_mode && matches!(config.log_mode, LogMode::Cli | LogMode::Both);

    if print_cli {
        // Quality applies to JPEG output, or Original format when not convert_only
        // (JPEG inputs in the archive may be re-encoded)
        let is_jpeg_out = matches!(config.output_format, OutputFormat::Jpeg)
            || (matches!(config.output_format, OutputFormat::Original) && !config.convert_only);
        let (display_w, display_h) = config.preset.effective_dimensions(config.max_width, config.max_height);
        eprintln!("cbz-opt  Processing {} file(s)", total);
        if config.convert_only {
            eprintln!(
                "Settings: convert-only / format={:?}{}/ threads={}",
                config.output_format,
                if is_jpeg_out { format!(" / quality={} ", config.jpeg_quality) } else { " ".to_string() },
                if config.threads == 0 { "auto (half of CPUs)".to_string() } else { config.threads.to_string() }
            );
        } else {
            eprintln!(
                "Settings: preset={:?} ({}x{}) / format={:?}{}/ threads={}",
                config.preset,
                display_w,
                display_h,
                config.output_format,
                if is_jpeg_out { format!(" / quality={} ", config.jpeg_quality) } else { " ".to_string() },
                if config.threads == 0 { "auto (half of CPUs)".to_string() } else { config.threads.to_string() }
            );
        }
        eprintln!("{}", "-".repeat(60));
    }
    let write_file = matches!(config.log_mode, LogMode::Both | LogMode::File);

    // Collect log entries inside the callback (only when writing to file)
    let log_entries: Arc<Mutex<Vec<LogEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let log_entries_cb = Arc::clone(&log_entries);

    // Capture AllDone stats (bytes + elapsed) for log file
    let completion: Arc<Mutex<(u64, u64, std::time::Duration)>> =
        Arc::new(Mutex::new((0, 0, std::time::Duration::ZERO)));
    let completion_cb = Arc::clone(&completion);

    let start_time = Instant::now();

    let (succeeded, skipped, failed) = process_zips(&args.files, &config, move |event| {
        // Always capture AllDone stats so write_log can use them
        if let ProgressEvent::AllDone { total_input_bytes, total_output_bytes, .. } = &event {
            *completion_cb.lock().unwrap() =
                (*total_input_bytes, *total_output_bytes, start_time.elapsed());
        }

        // Collect log entries for file output
        if write_file {
            match &event {
                ProgressEvent::ZipDone { path, output_path, .. } => {
                    log_entries_cb.lock().unwrap().push(LogEntry {
                        status: "SUCCESS",
                        input: path.clone(),
                        detail: output_path.clone(),
                    });
                }
                ProgressEvent::ZipSkipped { path, reason } => {
                    log_entries_cb.lock().unwrap().push(LogEntry {
                        status: "SKIPPED",
                        input: path.clone(),
                        detail: reason.clone(),
                    });
                }
                ProgressEvent::ZipError { path, message } => {
                    log_entries_cb.lock().unwrap().push(LogEntry {
                        status: "ERROR",
                        input: path.clone(),
                        detail: message.clone(),
                    });
                }
                _ => {}
            }
        }

        if json_mode {
            // Output JSON lines to stdout for scripting / automation
            if let Ok(json) = serde_json::to_string(&event) {
                println!("{json}");
            }
        } else if print_cli {
            // Human-readable output
            // Note: ZIPs are processed in parallel, so we print each event on its own line.
            // ImageDone is omitted to avoid interleaved noise across concurrent ZIPs.
            match &event {
                ProgressEvent::ZipStarted { path, image_count } => {
                    eprintln!("▶ {} ({} image(s))", short_path(path), image_count);
                }
                ProgressEvent::ImageDone { .. } => {
                    // Omitted: parallel output would interleave across ZIPs
                }
                ProgressEvent::ZipDone { output_path, .. } => {
                    eprintln!("  ✓ → {}", short_path(output_path));
                }
                ProgressEvent::ZipSkipped { path, reason } => {
                    eprintln!("  ⏭ {} : {}", short_path(path), reason);
                }
                ProgressEvent::ZipError { path, message } => {
                    eprintln!("  ✗ {} : {}", short_path(path), message);
                }
                ProgressEvent::AllDone { total_zips, succeeded, skipped, failed, total_input_bytes, total_output_bytes } => {
                    let (_, _, elapsed) = *completion_cb.lock().unwrap();
                    eprintln!("{}", "-".repeat(60));
                    eprintln!(
                        "Done: {}/{} succeeded  {} skipped  {} failed",
                        succeeded, total_zips, skipped, failed
                    );
                    if *total_input_bytes > 0 {
                        let saved = total_input_bytes.saturating_sub(*total_output_bytes);
                        let pct = saved as f64 / *total_input_bytes as f64 * 100.0;
                        eprintln!(
                            "✔ Saved: {} → {} (-{:.0}%) | ⏱ {}",
                            format_size(*total_input_bytes),
                            format_size(*total_output_bytes),
                            pct,
                            format_elapsed(elapsed.as_secs()),
                        );
                    }
                }
            }
        }
    });

    // Write log file if requested
    if write_file {
        let entries = log_entries.lock().unwrap();
        let (input_bytes, output_bytes, elapsed) = *completion.lock().unwrap();
        if let Err(e) = write_log(&config, &entries, succeeded, skipped, failed, input_bytes, output_bytes, elapsed, print_cli) {
            if print_cli {
                eprintln!("Warning: failed to write log file: {e}");
            }
        }
    }

    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Write a log file for this run
fn write_log(
    config: &OptimizeConfig,
    entries: &[LogEntry],
    succeeded: usize,
    skipped: usize,
    failed: usize,
    total_input_bytes: u64,
    total_output_bytes: u64,
    elapsed: std::time::Duration,
    print_cli: bool,
) -> Result<()> {
    let now = chrono::Local::now();
    let timestamp = now.format("%Y%m%d_%H%M%S").to_string();
    let datetime_str = now.format("%Y-%m-%d %H:%M:%S").to_string();
    let log_path = format!("cbz-opt_{}.log", timestamp);

    let (preset_name, (pw, ph)) = {
        let name = format!("{:?}", config.preset).to_lowercase();
        let dims = config.effective_dimensions();
        (name, dims)
    };
    let format_name = format!("{:?}", config.output_format).to_lowercase();
    let threads_str = if config.threads == 0 { "auto (half of CPUs)".to_string() } else { config.threads.to_string() };

    let mut buf = String::new();
    buf.push_str("========================================\n");
    buf.push_str("cbz-opt run log\n");
    buf.push_str(&format!("Date: {}\n", datetime_str));
    buf.push_str("========================================\n");
    let is_jpeg_out = matches!(config.output_format, OutputFormat::Jpeg)
        || (matches!(config.output_format, OutputFormat::Original) && !config.convert_only);
    buf.push_str("Settings:\n");
    if config.convert_only {
        buf.push_str("  Mode    : convert-only (no resize)\n");
    } else {
        buf.push_str(&format!("  Preset  : {} ({}x{})\n", preset_name, pw, ph));
    }
    if is_jpeg_out {
        buf.push_str(&format!("  Quality : {}\n", config.jpeg_quality));
    }
    buf.push_str(&format!("  Format  : {}\n", format_name));
    buf.push_str(&format!("  Threads : {}\n", threads_str));
    buf.push_str("----------------------------------------\n");

    for entry in entries {
        buf.push_str(&format!(
            "[{:<7}] {} - {}\n",
            entry.status,
            short_path(&entry.input),
            entry.detail,
        ));
    }

    buf.push_str("----------------------------------------\n");
    buf.push_str(&format!(
        "Total: {}  Succeeded: {}  Skipped: {}  Failed: {}\n",
        entries.len(), succeeded, skipped, failed
    ));
    if total_input_bytes > 0 {
        let saved = total_input_bytes.saturating_sub(total_output_bytes);
        let pct = saved as f64 / total_input_bytes as f64 * 100.0;
        buf.push_str(&format!(
            "✔ Saved: {} → {} (-{:.0}%) | ⏱ {}\n",
            format_size(total_input_bytes),
            format_size(total_output_bytes),
            pct,
            format_elapsed(elapsed.as_secs()),
        ));
    }
    buf.push_str("========================================\n");

    std::fs::write(&log_path, buf)?;
    if print_cli {
        eprintln!("Log: {}", log_path);
    }
    Ok(())
}

// Show filename only (trim long paths)
fn short_path(p: &str) -> &str {
    std::path::Path::new(p)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(p)
}

