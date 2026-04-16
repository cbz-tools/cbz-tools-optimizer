pub mod processor;
pub mod resize;

/// Log output mode
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum LogMode {
    /// Output to CLI only (default)
    Cli,
    /// No output
    Silent,
    /// Output to both CLI and log file
    Both,
    /// Output to log file only
    File,
}

/// Output file conflict resolution
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum OverwriteMode {
    /// Skip if output file already exists (default)
    Skip,
    /// Overwrite existing output file
    Overwrite,
    /// Rename output file if exists: name_new(1).zip, name_new(2).zip ...
    Rename,
}

/// Output image format
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum OutputFormat {
    /// Convert all images to JPEG (default)
    Jpeg,
    /// Keep the original format of each image
    Original,
    /// Convert all images to PNG
    Png,
    /// Convert all images to lossless WebP
    Webp,
}

/// Size preset
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
pub enum SizePreset {
    /// 1920×1080
    FullHd,
    /// 1280×720
    Hd,
    /// 3840×2160
    FourK,
    /// 2732×2048
    IpadPro,
    /// 2360×1640
    IpadAir,
    /// 2048×1536
    Ipad,
    /// 1264×1680
    Kindle,
    /// Use --max-width / --max-height values
    Custom,
}

impl SizePreset {
    pub fn dimensions(&self) -> Option<(u32, u32)> {
        match self {
            Self::FullHd  => Some((1920, 1080)),
            Self::Hd      => Some((1280, 720)),
            Self::FourK   => Some((3840, 2160)),
            Self::IpadPro => Some((2732, 2048)),
            Self::IpadAir => Some((2360, 1640)),
            Self::Ipad    => Some((2048, 1536)),
            Self::Kindle  => Some((1264, 1680)),
            Self::Custom  => None,
        }
    }

    /// Return preset dimensions, or fall back to the given values
    pub fn effective_dimensions(&self, max_width: u32, max_height: u32) -> (u32, u32) {
        self.dimensions().unwrap_or((max_width, max_height))
    }
}

/// Processing configuration
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OptimizeConfig {
    /// Size preset (Custom uses max_width / max_height)
    pub preset: SizePreset,
    /// Maximum width after resizing (px) — used only when preset is Custom
    pub max_width: u32,
    /// Maximum height after resizing (px) — used only when preset is Custom
    pub max_height: u32,
    /// JPEG quality (1-100)
    pub jpeg_quality: u8,
    /// Output directory (None = same as input)
    pub output_dir: Option<std::path::PathBuf>,
    /// Output filename suffix (e.g. "_new")
    pub output_suffix: String,
    /// Number of threads (0 = auto)
    pub threads: usize,
    /// Output image format
    pub output_format: OutputFormat,
    /// Log output mode
    pub log_mode: LogMode,
    /// Output file conflict resolution
    pub overwrite_mode: OverwriteMode,
}

impl OptimizeConfig {
    /// Return effective (width, height) from preset or max_width/max_height
    pub fn effective_dimensions(&self) -> (u32, u32) {
        self.preset.dimensions().unwrap_or((self.max_width, self.max_height))
    }
}

impl Default for OptimizeConfig {
    fn default() -> Self {
        Self {
            preset: SizePreset::Ipad,
            max_width: 2048,
            max_height: 1536,
            jpeg_quality: 85,
            output_dir: None,
            output_suffix: "_new".to_string(),
            threads: 0,
            output_format: OutputFormat::Jpeg,
            log_mode: LogMode::Cli,
            overwrite_mode: OverwriteMode::Skip,
        }
    }
}

/// Format byte size as human-readable string (e.g. "12.3 MB")
pub fn format_size(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    const KIB: u64 = 1024;
    if bytes >= GIB {
        format!("{:.1} GB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KB", bytes as f64 / KIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Format elapsed seconds as e.g. "2m14s"
pub fn format_elapsed(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{:02}m{:02}s", secs / 3600, (secs % 3600) / 60, secs % 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

/// Progress events (used by both CLI stdout and GUI channel)
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum ProgressEvent {
    /// ZIP processing started
    ZipStarted { path: String, image_count: usize },
    /// One image resized
    ImageDone { zip_path: String, image_index: usize, total: usize },
    /// ZIP processing done
    ZipDone { path: String, output_path: String, input_bytes: u64, output_bytes: u64 },
    /// ZIP skipped (e.g. contains animated WebP or GIF)
    ZipSkipped { path: String, reason: String },
    /// ZIP processing error
    ZipError { path: String, message: String },
    /// All done
    AllDone { total_zips: usize, succeeded: usize, skipped: usize, failed: usize, total_input_bytes: u64, total_output_bytes: u64 },
}
