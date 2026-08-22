#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use cbz_tools_optimizer_launcher::EmbeddedAsset;

mod assets {
    use super::EmbeddedAsset;
    include!(concat!(env!("OUT_DIR"), "/embedded_gui.rs"));
}

fn main() {
    std::process::exit(cbz_tools_optimizer_launcher::run_gui(assets::ASSETS));
}
