//! cbz-image-optimizer GUI
//!
//! Calls cbz-image-optimizer-core directly (no CLI subprocess).
//! Supports CJK fonts, enlarged text, and proper Done-removal from list.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app_config;
mod lang;

use app_config::AppConfig;
use lang::{strings, Lang};

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use cbz_tools_optimizer_core::{
    LogMode, OptimizeConfig, OutputFormat, OverwriteMode, ProgressEvent, SizePreset,
    format_elapsed, format_size,
};
use crossbeam_channel::{unbounded, Receiver};
use eframe::egui;
use egui_material_icons::icons::{
    ICON_CLEAR_ALL, ICON_CONTENT_PASTE, ICON_FOLDER_OPEN,
    ICON_NOTE_ADD, ICON_PLAY_ARROW, ICON_REMOVE, ICON_SETTINGS,
};

// ---------------------------------------------------------------------------
// Font & style setup
// ---------------------------------------------------------------------------

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // Material Icons font (fallback — icons live in the Unicode PUA range)
    let mut icon_data = egui::FontData::from_static(egui_material_icons::FONT_DATA);
    icon_data.tweak.y_offset_factor = 0.05;
    fonts.font_data.insert("material-icons".to_owned(), icon_data);
    fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .unwrap()
        .push("material-icons".to_owned());

    // Windows system fonts — try in priority order (Yu Gothic → Meiryo → MS Gothic → Noto CJK)
    let candidates = [
        "C:/Windows/Fonts/YuGothM.ttc",
        "C:/Windows/Fonts/meiryo.ttc",
        "C:/Windows/Fonts/msgothic.ttc",
        "C:/Windows/Fonts/NotoSansCJK-Regular.ttc",
    ];

    for path in &candidates {
        if let Ok(data) = std::fs::read(path) {
            fonts.font_data.insert(
                "cjk".to_owned(),
                egui::FontData::from_owned(data).into(),
            );
            // CJK font has highest priority so Latin and CJK share the same typeface
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "cjk".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .insert(0, "cjk".to_owned());
            break;
        }
    }

    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();

    // Enlarge all text by ~35%
    for (_, font_id) in style.text_styles.iter_mut() {
        font_id.size *= 1.35;
    }

    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);

    ctx.set_style(style);
}

/// Build a button label that renders the icon and text side-by-side at different sizes.
/// Material Symbols glyphs occupy ~60-70% of the em square, so the icon font size
/// must be set larger than the text size to achieve a visually matching height.
fn btn_label(icon: &str, label: &str) -> egui::text::LayoutJob {
    let color = egui::Color32::from_gray(20);
    let mut job = egui::text::LayoutJob::default();
    job.append(
        icon,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(20.0),
            color,
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job.append(
        &format!("  {label}"),
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(13.0),
            color,
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn load_icon() -> egui::viewport::IconData {
    let bytes = include_bytes!("../assets/icon.png");
    let image = image::load_from_memory(bytes).expect("icon load failed").into_rgba8();
    let (width, height) = image.dimensions();
    egui::viewport::IconData { rgba: image.into_raw(), width, height }
}

fn main() -> eframe::Result<()> {
    // Load config early to restore window position before creating the viewport.
    // If the saved monitor no longer exists the OS moves the window to the primary monitor.
    let startup_config = AppConfig::load();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("CBZ Image Optimizer")
        .with_inner_size([700.0, 540.0])
        .with_drag_and_drop(true)
        .with_icon(load_icon());

    if let (Some(x), Some(y)) = (startup_config.window_x, startup_config.window_y) {
        viewport = viewport.with_position(egui::Pos2::new(x as f32, y as f32));
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "CBZ Image Optimizer",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            setup_style(&cc.egui_ctx);
            Ok(Box::new(App::new()))
        }),
    )
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone)]
enum FileStatus {
    Pending,
    Processing,
    Skipped(String),
    Error(String),
}

struct FileEntry {
    path: PathBuf,
    status: FileStatus,
}

enum StatusUpdate {
    Processing(PathBuf),
    Done(PathBuf),
    Skipped(PathBuf, String),
    Error(PathBuf, String),
    AllDone { elapsed: std::time::Duration, input_bytes: u64, output_bytes: u64 },
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct App {
    config: AppConfig,
    lang: Lang,

    files: Vec<FileEntry>,
    selected: Option<usize>,

    running: Arc<AtomicBool>,
    progress: Arc<Mutex<(usize, usize)>>,

    status_rx: Option<Receiver<StatusUpdate>>,

    show_settings: bool,
    show_bulk_add: bool,
    bulk_add_text: String,

    completion_msg: Option<String>,

    settings_draft: AppConfig,

    /// Last known window position — saved to AppConfig on exit
    last_window_pos: Option<egui::Pos2>,
}

impl App {
    fn new() -> Self {
        let config = AppConfig::load();
        let lang = match config.lang.as_str() {
            "zh" => Lang::Zh,
            "ja" => Lang::Ja,
            _ => Lang::En,
        };
        let last_window_pos = match (config.window_x, config.window_y) {
            (Some(x), Some(y)) => Some(egui::Pos2::new(x as f32, y as f32)),
            _ => None,
        };
        Self {
            settings_draft: config.clone(),
            config,
            lang,
            files: Vec::new(),
            selected: None,
            running: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Mutex::new((0, 0))),
            status_rx: None,
            show_settings: false,
            show_bulk_add: false,
            bulk_add_text: String::new(),
            completion_msg: None,
            last_window_pos,
        }
    }

    fn add_path(&mut self, path: &Path) {
        if path.is_dir() {
            self.add_entry(path.to_path_buf());
        } else if is_archive_ext(path) {
            self.add_entry(path.to_path_buf());
        }
    }

    fn add_entry(&mut self, path: PathBuf) {
        if !self.files.iter().any(|e| e.path == path) {
            self.files.push(FileEntry { path, status: FileStatus::Pending });
        }
    }

    fn build_optimize_config(&self) -> OptimizeConfig {
        let preset = match self.config.preset.as_str() {
            "full-hd"  => SizePreset::FullHd,
            "hd"       => SizePreset::Hd,
            "four-k"   => SizePreset::FourK,
            "ipad-pro" => SizePreset::IpadPro,
            "ipad-air" => SizePreset::IpadAir,
            "kindle"   => SizePreset::Kindle,
            "custom"   => SizePreset::Custom,
            _          => SizePreset::Ipad,
        };
        let output_format = match self.config.output_format.as_str() {
            "png"      => OutputFormat::Png,
            "webp"     => OutputFormat::Webp,
            "avif"     => OutputFormat::Avif,
            "original" => OutputFormat::Original,
            _          => OutputFormat::Jpeg,
        };
        let convert_only = self.config.convert_only;
        let overwrite_mode = match self.config.overwrite_mode.as_str() {
            "overwrite" => OverwriteMode::Overwrite,
            "rename"    => OverwriteMode::Rename,
            _           => OverwriteMode::Skip,
        };
        let log_mode = match self.config.log_mode.as_str() {
            "silent" => LogMode::Silent,
            "both"   => LogMode::Both,
            "file"   => LogMode::File,
            _        => LogMode::Cli,
        };
        OptimizeConfig {
            preset,
            max_width: self.config.max_width,
            max_height: self.config.max_height,
            jpeg_quality: self.config.jpeg_quality,
            output_format,
            convert_only,
            output_dir: if self.config.output_dir.is_empty() {
                None
            } else {
                Some(PathBuf::from(&self.config.output_dir))
            },
            output_suffix: self.config.suffix.clone(),
            threads: self.config.threads,
            overwrite_mode,
            log_mode,
        }
    }

    fn start_processing(&mut self, ctx: &egui::Context) {
        // Expand folder entries into ZIP/CBZ files (1 level only)
        let mut expanded: Vec<PathBuf> = Vec::new();
        for entry in &self.files {
            if entry.path.is_dir() {
                if let Ok(rd) = std::fs::read_dir(&entry.path) {
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.is_file() && is_archive_ext(&p) {
                            expanded.push(p);
                        }
                    }
                }
            } else {
                expanded.push(entry.path.clone());
            }
        }
        expanded.sort();
        expanded.dedup();

        self.files = expanded.into_iter().map(|p| FileEntry {
            path: p,
            status: FileStatus::Pending,
        }).collect();

        let total = self.files.len();
        if total == 0 {
            return;
        }
        *self.progress.lock().unwrap() = (0, total);
        self.running.store(true, Ordering::Relaxed);
        self.completion_msg = None;

        let (tx, rx) = unbounded::<StatusUpdate>();
        self.status_rx = Some(rx);

        let paths: Vec<PathBuf> = self.files.iter().map(|e| e.path.clone()).collect();
        let config = self.build_optimize_config();
        let running = Arc::clone(&self.running);
        let ctx2 = ctx.clone();

        thread::spawn(move || {
            // Keep a clone of ctx2 for use after process_zips (which moves ctx2 into closure)
            let ctx_final = ctx2.clone();
            let t0 = std::time::Instant::now();

            cbz_tools_optimizer_core::processor::process_zips(
                &paths,
                &config,
                move |event| {
                    let update = match event {
                        ProgressEvent::ZipStarted { path, .. } => {
                            Some(StatusUpdate::Processing(PathBuf::from(&path)))
                        }
                        ProgressEvent::ZipDone { path, .. } => {
                            Some(StatusUpdate::Done(PathBuf::from(&path)))
                        }
                        ProgressEvent::ZipSkipped { path, reason } => {
                            Some(StatusUpdate::Skipped(PathBuf::from(&path), reason))
                        }
                        ProgressEvent::ZipError { path, message } => {
                            Some(StatusUpdate::Error(PathBuf::from(&path), message))
                        }
                        ProgressEvent::AllDone { total_input_bytes, total_output_bytes, .. } => {
                            Some(StatusUpdate::AllDone {
                                elapsed: t0.elapsed(),
                                input_bytes: total_input_bytes,
                                output_bytes: total_output_bytes,
                            })
                        }
                        _ => None,
                    };
                    if let Some(u) = update {
                        let _ = tx.send(u);
                        ctx2.request_repaint();
                    }
                },
            );

            running.store(false, Ordering::Relaxed);
            ctx_final.request_repaint();
        });
    }
}

// ---------------------------------------------------------------------------
// eframe::App implementation
// ---------------------------------------------------------------------------

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(pos) = self.last_window_pos {
            self.config.window_x = Some(pos.x as i32);
            self.config.window_y = Some(pos.y as i32);
            self.config.save();
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Track window position for monitor memory (saved in on_exit)
        if let Some(rect) = ctx.input(|i| i.viewport().outer_rect) {
            self.last_window_pos = Some(rect.min);
        }

        // Drain status channel
        if let Some(rx) = &self.status_rx {
            while let Ok(update) = rx.try_recv() {
                match update {
                    StatusUpdate::Processing(path) => {
                        if let Some(e) = self.files.iter_mut().find(|e| e.path == path) {
                            e.status = FileStatus::Processing;
                        }
                    }
                    StatusUpdate::Done(path) => {
                        // Remove completed entry from the list
                        self.files.retain(|e| e.path != path);
                        let mut p = self.progress.lock().unwrap();
                        p.0 += 1;
                    }
                    StatusUpdate::Skipped(path, reason) => {
                        if let Some(e) = self.files.iter_mut().find(|e| e.path == path) {
                            e.status = FileStatus::Skipped(reason);
                        }
                        let mut p = self.progress.lock().unwrap();
                        p.0 += 1;
                    }
                    StatusUpdate::Error(path, msg) => {
                        if path.as_os_str().is_empty() {
                            // Mark all pending/processing as error
                            for e in self.files.iter_mut() {
                                if e.status == FileStatus::Processing
                                    || e.status == FileStatus::Pending
                                {
                                    e.status = FileStatus::Error(msg.clone());
                                }
                            }
                        } else if let Some(e) =
                            self.files.iter_mut().find(|e| e.path == path)
                        {
                            e.status = FileStatus::Error(msg);
                        }
                        let mut p = self.progress.lock().unwrap();
                        p.0 += 1;
                    }
                    StatusUpdate::AllDone { elapsed, input_bytes, output_bytes } => {
                        self.completion_msg = Some(format_completion(input_bytes, output_bytes, elapsed));
                    }
                }
            }
        }

        // D&D
        ctx.input(|i| {
            let paths: Vec<PathBuf> = i
                .raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect();
            for path in paths {
                self.add_path(&path);
            }
        });

        let s = strings(&self.lang);
        let is_running = self.running.load(Ordering::Relaxed);

        // Keep repainting while running (throttled to 200ms to avoid busy-loop)
        if is_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }

        // ── Menu bar ──────────────────────────────────────────────────────
        egui::TopBottomPanel::top("menubar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.label(egui::RichText::new(s.app_title).strong());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Settings button
                    if ui.button(egui::RichText::new(ICON_SETTINGS).size(20.0)).clicked() {
                        self.settings_draft = self.config.clone();
                        self.show_settings = true;
                    }

                    ui.separator();

                    // Bulk add menu item
                    if ui.button(btn_label(ICON_CONTENT_PASTE, "Bulk Add")).clicked() {
                        self.show_bulk_add = true;
                    }
                });
            });
        });

        // ── Settings summary bar ──────────────────────────────────────────
        egui::TopBottomPanel::top("settings_summary").show(ctx, |ui| {
            let s = strings(&self.lang);
            let is_jpeg_out = self.config.output_format == "jpeg"
                || (self.config.output_format == "original" && !self.config.convert_only);
            let quality_part = if is_jpeg_out {
                format!("  |  {}  {}", s.quality_label, self.config.jpeg_quality)
            } else {
                String::new()
            };
            if self.config.convert_only {
                ui.label(format!(
                    "{}  |  {}  {}{}",
                    s.convert_only_label,
                    s.format_label,
                    self.config.output_format,
                    quality_part,
                ));
            } else {
                ui.label(format!(
                    "{}  {}  |  {}  {}{}",
                    s.preset_label,
                    self.config.preset,
                    s.format_label,
                    self.config.output_format,
                    quality_part,
                ));
            }
        });

        // ── Bottom panel: progress + start ────────────────────────────────
        egui::TopBottomPanel::bottom("bottom_panel").show(ctx, |ui| {
            let s = strings(&self.lang);

            if is_running {
                let (current, total) = *self.progress.lock().unwrap();
                let frac = if total > 0 { current as f32 / total as f32 } else { 0.0 };
                let prog_text = s
                    .progress
                    .replace("{current}", &current.to_string())
                    .replace("{total}", &total.to_string());
                ui.add(
                    egui::ProgressBar::new(frac)
                        .text(prog_text)
                        .animate(true),
                );
            }

            ui.horizontal(|ui| {
                let has_pending = self.files.iter().any(|e| e.status == FileStatus::Pending);
                let can_start = !is_running && has_pending;
                let start_clicked = ui.scope(|ui| {
                    ui.set_enabled(can_start);
                    ui.add_sized(
                        [140.0, 40.0],
                        egui::Button::new({
                            let color = egui::Color32::from_gray(20);
                            let mut job = egui::text::LayoutJob::default();
                            job.append(ICON_PLAY_ARROW, 0.0, egui::TextFormat {
                                font_id: egui::FontId::proportional(28.0),
                                color,
                                valign: egui::Align::Center,
                                ..Default::default()
                            });
                            job.append(&format!("  {}", s.start), 0.0, egui::TextFormat {
                                font_id: egui::FontId::proportional(13.0),
                                color,
                                valign: egui::Align::Center,
                                ..Default::default()
                            });
                            job
                        }),
                    )
                }).inner.clicked();
                if start_clicked {
                    self.start_processing(ctx);
                }

                if let Some(msg) = &self.completion_msg {
                    ui.label(
                        egui::RichText::new(msg).color(egui::Color32::from_rgb(80, 200, 80)),
                    );
                }

            });
        });

        // ── Central panel: file list + buttons ────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            let s = strings(&self.lang);

            if self.files.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label(
                        egui::RichText::new(s.drop_hint).color(egui::Color32::GRAY),
                    );
                });
            } else {
                // File list table
                let available = ui.available_height() - 36.0;
                egui::ScrollArea::vertical()
                    .max_height(available)
                    .show(ui, |ui| {
                        egui::Grid::new("file_list")
                            .num_columns(2)
                            .striped(true)
                            .min_col_width(100.0)
                            .show(ui, |ui| {
                                // Header
                                ui.label(egui::RichText::new(s.col_file).strong());
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new(s.col_status).strong(),
                                        );
                                    },
                                );
                                ui.end_row();

                                let mut clicked_idx: Option<usize> = None;
                                for (i, entry) in self.files.iter().enumerate() {
                                    let name = entry
                                        .path
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("?");

                                    let is_sel = self.selected == Some(i);
                                    if ui.selectable_label(is_sel, name).clicked() {
                                        clicked_idx = Some(i);
                                    }

                                    let (status_text, color) = match &entry.status {
                                        FileStatus::Pending => (
                                            s.status_pending.to_string(),
                                            egui::Color32::GRAY,
                                        ),
                                        FileStatus::Processing => (
                                            s.status_processing.to_string(),
                                            egui::Color32::from_rgb(100, 180, 255),
                                        ),
                                        FileStatus::Skipped(_) => (
                                            s.status_skipped.to_string(),
                                            egui::Color32::from_rgb(255, 200, 50),
                                        ),
                                        FileStatus::Error(_) => (
                                            s.status_error.to_string(),
                                            egui::Color32::from_rgb(255, 80, 80),
                                        ),
                                    };

                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.colored_label(color, status_text);
                                        },
                                    );
                                    ui.end_row();
                                }
                                if let Some(i) = clicked_idx {
                                    self.selected = Some(i);
                                }
                            });
                    });
            }

            ui.separator();

            // Button row
            ui.horizontal_top(|ui| {
                // Add Files
                if ui.add_sized(
                    [0.0, 28.0],
                    egui::Button::new(btn_label(ICON_NOTE_ADD, s.add_files)),
                ).clicked() {
                    if let Some(paths) = rfd::FileDialog::new()
                        .add_filter("ZIP/CBZ", &["zip", "cbz"])
                        .pick_files()
                    {
                        for p in paths {
                            self.add_entry(p);
                        }
                    }
                }

                // Add Folder
                if ui.add_sized(
                    [0.0, 28.0],
                    egui::Button::new(btn_label(ICON_FOLDER_OPEN, s.add_folder)),
                ).clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.add_entry(folder);
                    }
                }

                // Remove selected
                let remove_clicked = ui.scope(|ui| {
                    ui.set_enabled(!is_running && self.selected.is_some());
                    ui.add_sized(
                        [0.0, 28.0],
                        egui::Button::new(btn_label(ICON_REMOVE, s.remove)),
                    )
                }).inner.clicked();
                if remove_clicked {
                    if let Some(i) = self.selected.take() {
                        if i < self.files.len() {
                            self.files.remove(i);
                        }
                    }
                }

                // Clear
                let clear_clicked = ui.scope(|ui| {
                    ui.set_enabled(!is_running);
                    ui.add_sized(
                        [0.0, 28.0],
                        egui::Button::new(btn_label(ICON_CLEAR_ALL, s.clear)),
                    )
                }).inner.clicked();
                if clear_clicked {
                    self.files.clear();
                    self.selected = None;
                }
            });
        });

        // ── Settings window ───────────────────────────────────────────────
        if self.show_settings {
            let s = strings(&self.lang);
            let mut open = true;
            egui::Window::new(s.settings)
                .collapsible(false)
                .resizable(false)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.screen_rect().center())
                .open(&mut open)
                .show(ctx, |ui| {
                    let d = &mut self.settings_draft;

                    egui::Grid::new("settings_grid")
                        .num_columns(2)
                        .spacing([8.0, 4.0])
                        .show(ui, |ui| {
                            let s2 = strings(&self.lang);

                            // Language
                            let lang_display = match d.lang.as_str() {
                                "zh" => "中文",
                                "ja" => "日本語",
                                _ => "En",
                            };
                            ui.label(s2.lang_label);
                            egui::ComboBox::from_id_salt("lang_combo")
                                .selected_text(lang_display)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(&mut d.lang, "en".into(), "En");
                                    ui.selectable_value(&mut d.lang, "zh".into(), "中文");
                                    ui.selectable_value(&mut d.lang, "ja".into(), "日本語");
                                });
                            ui.end_row();

                            // Preset (disabled when convert_only)
                            ui.label(s2.preset_label);
                            ui.scope(|ui| {
                                ui.set_enabled(!d.convert_only);
                                egui::ComboBox::from_id_salt("preset_combo")
                                    .selected_text(&d.preset)
                                    .show_ui(ui, |ui| {
                                        for p in &[
                                            "ipad", "ipad-air", "ipad-pro", "kindle", "hd",
                                            "full-hd", "four-k", "custom",
                                        ] {
                                            ui.selectable_value(
                                                &mut d.preset,
                                                p.to_string(),
                                                *p,
                                            );
                                        }
                                    });
                            });
                            ui.end_row();

                            // Width / Height (editable for custom only, disabled when convert_only)
                            let is_custom = d.preset == "custom";
                            ui.label(s2.width_label);
                            ui.add_enabled(
                                is_custom && !d.convert_only,
                                egui::DragValue::new(&mut d.max_width).range(1..=65535),
                            );
                            ui.end_row();

                            ui.label(s2.height_label);
                            ui.add_enabled(
                                is_custom && !d.convert_only,
                                egui::DragValue::new(&mut d.max_height).range(1..=65535),
                            );
                            ui.end_row();

                            // Format
                            ui.label(s2.format_label);
                            egui::ComboBox::from_id_salt("format_combo")
                                .selected_text(&d.output_format)
                                .show_ui(ui, |ui| {
                                    for f in &["jpeg", "png", "webp", "avif", "original"] {
                                        ui.selectable_value(
                                            &mut d.output_format,
                                            f.to_string(),
                                            *f,
                                        );
                                    }
                                });
                            ui.end_row();

                            // Convert only
                            ui.label(s2.convert_only_label);
                            ui.checkbox(&mut d.convert_only, "");
                            ui.end_row();

                            // Quality: applies to JPEG output, or Original format when not
                            // convert_only (JPEG inputs may be re-encoded)
                            let quality_active = d.output_format == "jpeg"
                                || (d.output_format == "original" && !d.convert_only);
                            ui.label(s2.quality_label);
                            ui.add_enabled(
                                quality_active,
                                egui::Slider::new(&mut d.jpeg_quality, 1..=100),
                            );
                            ui.end_row();

                            // Suffix
                            ui.label(s2.suffix_label);
                            ui.text_edit_singleline(&mut d.suffix);
                            ui.end_row();

                            // Output Dir
                            ui.label(s2.output_dir_label);
                            ui.text_edit_singleline(&mut d.output_dir);
                            ui.end_row();

                            // Threads
                            ui.label(s2.threads_label);
                            ui.add(
                                egui::DragValue::new(&mut d.threads).range(0..=256),
                            );
                            ui.end_row();

                            // Overwrite Mode
                            ui.label(s2.overwrite_label);
                            egui::ComboBox::from_id_salt("overwrite_combo")
                                .selected_text(&d.overwrite_mode)
                                .show_ui(ui, |ui| {
                                    for m in &["skip", "overwrite", "rename"] {
                                        ui.selectable_value(
                                            &mut d.overwrite_mode,
                                            m.to_string(),
                                            *m,
                                        );
                                    }
                                });
                            ui.end_row();

                            // Log Mode
                            ui.label(s2.log_mode_label);
                            egui::ComboBox::from_id_salt("logmode_combo")
                                .selected_text(&d.log_mode)
                                .show_ui(ui, |ui| {
                                    for m in &["cli", "silent", "both", "file"] {
                                        ui.selectable_value(
                                            &mut d.log_mode,
                                            m.to_string(),
                                            *m,
                                        );
                                    }
                                });
                            ui.end_row();
                        });

                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("OK").clicked() {
                            self.config = self.settings_draft.clone();
                            self.lang = match self.config.lang.as_str() {
                                "zh" => Lang::Zh,
                                "ja" => Lang::Ja,
                                _ => Lang::En,
                            };
                            self.config.save();
                            self.show_settings = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.settings_draft = self.config.clone();
                            self.show_settings = false;
                        }
                        let s2 = strings(&self.lang);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button(s2.reset_defaults).clicked() {
                                self.settings_draft = AppConfig {
                                    lang: self.settings_draft.lang.clone(),
                                    window_x: self.settings_draft.window_x,
                                    window_y: self.settings_draft.window_y,
                                    ..AppConfig::default()
                                };
                            }
                        });
                    });
                });
            if !open {
                self.settings_draft = self.config.clone();
                self.show_settings = false;
            }
        }

        // ── Bulk Add window ───────────────────────────────────────────────
        if self.show_bulk_add {
            let s = strings(&self.lang);
            let mut open = true;
            egui::Window::new(s.bulk_add_title)
                .collapsible(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    let s2 = strings(&self.lang);
                    ui.label(s2.bulk_add_hint);
                    ui.add(
                        egui::TextEdit::multiline(&mut self.bulk_add_text)
                            .desired_rows(10)
                            .desired_width(f32::INFINITY),
                    );
                    if ui.button(s2.bulk_add_ok).clicked() {
                        let paths: Vec<PathBuf> = self
                            .bulk_add_text
                            .lines()
                            .map(|l| PathBuf::from(l.trim()))
                            .filter(|p| p.exists())
                            .collect();
                        for p in paths {
                            self.add_path(&p);
                        }
                        self.bulk_add_text.clear();
                        self.show_bulk_add = false;
                    }
                });
            if !open {
                self.show_bulk_add = false;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn format_completion(input_bytes: u64, output_bytes: u64, elapsed: std::time::Duration) -> String {
    if input_bytes > 0 {
        let saved = input_bytes.saturating_sub(output_bytes);
        let pct = saved as f64 / input_bytes as f64 * 100.0;
        format!(
            "✔ Done | Saved: {} → {} (-{:.0}%) | ⏱ {}",
            format_size(input_bytes),
            format_size(output_bytes),
            pct,
            format_elapsed(elapsed.as_secs()),
        )
    } else {
        format!("✔ Done | ⏱ {}", format_elapsed(elapsed.as_secs()))
    }
}

fn is_archive_ext(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        "zip" | "cbz" | "ZIP" | "CBZ"
    )
}

