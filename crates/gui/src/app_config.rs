use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppConfig {
    pub lang: String,
    pub preset: String,
    pub max_width: u32,
    pub max_height: u32,
    pub jpeg_quality: u8,
    pub output_format: String,
    pub convert_only: bool,
    #[serde(default = "default_animated_webp_filter")]
    pub animated_webp_filter: String,
    #[serde(default = "default_animated_webp_keyframes")]
    pub animated_webp_keyframes: String,
    #[serde(default = "default_animated_webp_kmin")]
    pub animated_webp_kmin: i32,
    #[serde(default = "default_animated_webp_kmax")]
    pub animated_webp_kmax: i32,
    #[serde(default = "default_animated_webp_output_policy")]
    pub animated_webp_output_policy: String,
    pub suffix: String,
    pub output_dir: String,
    pub threads: usize,
    pub overwrite_mode: String,
    pub log_mode: String,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            lang: "en".into(),
            preset: "ipad".into(),
            max_width: 2048,
            max_height: 1536,
            jpeg_quality: 85,
            output_format: "jpeg".into(),
            convert_only: false,
            animated_webp_filter: default_animated_webp_filter(),
            animated_webp_keyframes: default_animated_webp_keyframes(),
            animated_webp_kmin: default_animated_webp_kmin(),
            animated_webp_kmax: default_animated_webp_kmax(),
            animated_webp_output_policy: default_animated_webp_output_policy(),
            suffix: "_new".into(),
            output_dir: String::new(),
            threads: 0,
            overwrite_mode: "skip".into(),
            log_mode: "cli".into(),
            window_x: None,
            window_y: None,
        }
    }
}

fn default_animated_webp_filter() -> String {
    "bilinear".into()
}

fn default_animated_webp_keyframes() -> String {
    "bounded".into()
}

fn default_animated_webp_kmin() -> i32 {
    3
}

fn default_animated_webp_kmax() -> i32 {
    5
}

fn default_animated_webp_output_policy() -> String {
    "always-use-encoded".into()
}

fn config_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
    path.pop();
    path.push("cbz-opt-gui.toml");
    path
}

impl AppConfig {
    pub fn load() -> AppConfig {
        let path = config_path();
        if path.exists() {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            toml::from_str(&text).unwrap_or_default()
        } else {
            AppConfig::default()
        }
    }

    pub fn save(&self) {
        let path = config_path();
        if let Ok(text) = toml::to_string(self) {
            let _ = std::fs::write(path, text);
        }
    }
}
